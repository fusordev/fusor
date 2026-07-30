use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsNumber, JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits, RuntimeResource, ValueKind,
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
        let parameter_text = source
            .parameters()
            .iter()
            .map(JsString::to_utf8_lossy)
            .collect::<Result<Vec<_>, _>>()
            .map_err(engine_failure)?;
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let parameters = parameter_text
            .iter()
            .map(|parameter| SourceFragment::new(parameter.as_str()))
            .collect::<Vec<_>>();
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Function>"))
                        .map_err(engine_failure)?;
                context
                    .compile_dynamic_function_script(VerificationLimits::default())
                    .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                    .map_err(engine_failure)
            },
        )
        .map_err(|error| {
            if matches!(
                error.stage(),
                quickjs_frontend::DiagnosticStage::Parser
                    | quickjs_frontend::DiagnosticStage::Semantic
            ) {
                let message = error
                    .diagnostics()
                    .first()
                    .map_or("dynamic source rejected", |diagnostic| {
                        diagnostic.message.as_str()
                    });
                DynamicFunctionCompileFailure::Syntax {
                    message: JsString::from_utf8(message).expect("diagnostic string"),
                }
            } else {
                engine_failure(error)
            }
        })?
    }
}

fn engine_failure(error: impl Error + Send + Sync + 'static) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(TestCompileError(error.to_string())),
    }
}

fn dynamic_function(context: &mut Context<'_>, parameters: &[&str], body: &str) -> Function {
    let parameters = parameters
        .iter()
        .map(|parameter| JsString::from_utf8(parameter).expect("parameter"))
        .collect::<Vec<_>>();
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from(parameters),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn compiler() -> Arc<dyn OrdinaryDynamicFunctionCompiler> {
    Arc::new(TestCompiler)
}

fn assert_number(value: &quickjs_runtime::JsValue, expected: i32) {
    let number = value.as_number().expect("live value").expect("number");
    assert!(number.strict_equals(JsNumber::from_i32(expected)));
}

#[test]
fn global_function_call_compiles_executes_and_calls_the_result() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return Function('value','return value;')(7);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function call");

    assert_number(&result, 7);
}

#[test]
fn new_function_uses_constructor_dispatch_and_returns_a_callable() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return new Function('value','return value;')(8);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("new Function");

    assert_number(&result, 8);
}

#[test]
fn generated_function_materializes_ordinary_function_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let name = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return Function('first','second','return first;').name;",
    );
    let length = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return Function('first','second','return first;').length;",
    );
    let constructor_link = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "let f=Function('return 1;');return f.prototype.constructor===f;",
    );
    let mut context = runtime.context(&realm).expect("context");
    let compiler = compiler();

    let actual_name = context
        .call_with_dynamic_function_compiler(&name, &[], ExecutionLimits::default(), &compiler)
        .expect("function name");
    assert_eq!(
        actual_name
            .as_string()
            .expect("live name")
            .expect("string name")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "anonymous"
    );
    let actual_length = context
        .call_with_dynamic_function_compiler(&length, &[], ExecutionLimits::default(), &compiler)
        .expect("function length");
    assert_number(&actual_length, 2);
    let linked = context
        .call_with_dynamic_function_compiler(
            &constructor_link,
            &[],
            ExecutionLimits::default(),
            &compiler,
        )
        .expect("prototype constructor link");
    assert_eq!(linked.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_executes_as_an_ordinary_constructor() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let F=Function('value','this.answer=value;');let object=new F(12);return object.answer;",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("ordinary constructor");

    assert_number(&value, 12);
}

#[test]
fn generated_function_never_captures_the_caller_frame() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let hidden=9;return Function('return typeof hidden;')();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("isolated Function");

    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "undefined"
    );
}

#[test]
fn nested_function_construction_stays_in_one_iterative_vm_session() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return Function(\"return Function('return 4;')();\")();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("nested Function");

    assert_number(&result, 4);
}

#[test]
fn dynamic_compilation_count_is_bounded_per_interpreter_session() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return Function(\"return Function('return 4;')();\")();",
    );

    let error = context
        .call_with_dynamic_function_compiler(
            &run,
            &[],
            ExecutionLimits::default().with_dynamic_compilations(1),
            &compiler(),
        )
        .expect_err("second dynamic compilation exceeds the session limit");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::DynamicCompilations,
            limit: 1,
            observed: 2,
        }
    ));
}

#[test]
fn generated_source_units_are_bounded_before_compilation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &[], "return Function();");

    let error = context
        .call_with_dynamic_function_compiler(
            &run,
            &[],
            ExecutionLimits::default().with_dynamic_source_code_units(27),
            &compiler(),
        )
        .expect_err("empty exact wrapper contains 28 UTF-16 code units");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::DynamicSourceCodeUnits,
            limit: 27,
            observed: 28,
        }
    ));
}

#[test]
fn numeric_source_arguments_use_javascript_number_spelling() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &[], "return Function(1e-7)();");

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("numeric body source");

    assert_eq!(value.kind().expect("live value"), ValueKind::Undefined);
}

#[test]
fn malformed_dynamic_source_throws_syntax_error_without_installation() {
    let source = "return Function('return (');";
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &[], source);
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("invalid dynamic source");
    let ExecutionError::Exception(exception) = error else {
        panic!("syntax rejection must be a JavaScript exception");
    };

    assert_eq!(exception.kind(), Some(ExceptionKind::SyntaxError));
    assert_eq!(exception.source_name(), "<runtime Function>");
    let span = exception.source_span();
    assert_eq!(
        &exception.source_text()[span.start() as usize..span.end() as usize],
        "Function('return (')"
    );
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn directly_called_function_constructor_returns_a_javascript_syntax_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let getter = dynamic_function(&mut context, &[], "return Function;");
    let function_constructor = context
        .call(&getter, &[], ExecutionLimits::default())
        .expect("Function value")
        .into_function()
        .expect("Function");
    let invalid_body = context.string(JsString::from_utf8("return (").expect("source"));
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(
            &function_constructor,
            &[invalid_body],
            ExecutionLimits::default(),
            &compiler(),
        )
        .expect_err("invalid Function body");
    let ExecutionError::Exception(exception) = error else {
        panic!("direct Function syntax rejection must be a JavaScript exception");
    };

    assert_eq!(exception.kind(), Some(ExceptionKind::SyntaxError));
    assert_eq!(exception.source_name(), "<native Function>");
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn function_without_a_compiler_service_fails_closed_before_installation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &[], "return Function('return 1;');");
    let baseline = context.runtime_usage();

    let error = context
        .call(&run, &[], ExecutionLimits::default())
        .expect_err("missing compiler service");

    assert!(matches!(
        error,
        ExecutionError::DynamicFunctionCompilation(DynamicFunctionCompileFailure::Engine { .. })
    ));
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn failed_dynamic_frame_admission_rolls_back_the_root_environment() {
    let mut runtime = Runtime::try_new(
        RuntimeLimits::default()
            .with_max_active_frames(1)
            .with_max_realm_global_bindings(8),
    )
    .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return Function('return transientDynamicGlobal;');",
    );
    let baseline = context.runtime_usage();

    for _ in 0..2 {
        let error = context
            .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
            .expect_err("dynamic Script frame limit");
        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource: RuntimeResource::Frames,
                limit: 1,
                observed: 2,
            }
        ));
        assert_eq!(context.runtime_usage(), baseline);
    }
}

#[test]
fn function_prototype_is_callable_but_not_constructable() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let callable = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return Function.prototype();",
    );
    let construct = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return new Function.prototype();",
    );
    let mut context = runtime.context(&realm).expect("context");

    let value = context
        .call(&callable, &[], ExecutionLimits::default())
        .expect("Function.prototype call");
    assert_eq!(value.kind().expect("live value"), ValueKind::Undefined);

    let error = context
        .call(&construct, &[], ExecutionLimits::default())
        .expect_err("Function.prototype is not a constructor");
    let ExecutionError::Exception(exception) = error else {
        panic!("nonconstructor must throw");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "not a constructor"
    );
}

#[test]
fn new_function_preserves_non_nullish_primitive_wrapper_escape() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return new Function('}), 17 || (function(){');",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("primitive wrapper escape");

    assert_number(&value, 17);
}

#[test]
fn new_function_rejects_nullish_wrapper_escape_as_not_an_object() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return new Function('}), null && (function(){');",
    );

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("nullish wrapper escape");
    let ExecutionError::Exception(exception) = error else {
        panic!("nullish constructor completion must throw");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "not an object"
    );
}

#[test]
fn foreign_function_constructor_uses_its_home_realm_globals() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let constructor_realm = runtime.create_realm().expect("constructor realm");
    let caller_realm = runtime.create_realm().expect("caller realm");
    let getter = dynamic_function(
        &mut runtime
            .context(&constructor_realm)
            .expect("constructor context"),
        &[],
        "return Function;",
    );
    let setter = dynamic_function(
        &mut runtime
            .context(&constructor_realm)
            .expect("constructor context"),
        &[],
        "foreignMarker=11;return foreignMarker;",
    );
    let invoke = dynamic_function(
        &mut runtime.context(&caller_realm).expect("caller context"),
        &["F"],
        "return F('return foreignMarker;')();",
    );
    let function_constructor = runtime
        .context(&constructor_realm)
        .expect("constructor context")
        .call(&getter, &[], ExecutionLimits::default())
        .expect("Function value")
        .into_function()
        .expect("Function");
    runtime
        .context(&constructor_realm)
        .expect("constructor context")
        .call(&setter, &[], ExecutionLimits::default())
        .expect("set constructor-realm marker");
    let mut caller = runtime.context(&caller_realm).expect("caller context");

    let value = caller
        .call_with_dynamic_function_compiler(
            &invoke,
            &[function_constructor.as_value().clone()],
            ExecutionLimits::default(),
            &compiler(),
        )
        .expect("foreign Function");

    assert_number(&value, 11);
}
