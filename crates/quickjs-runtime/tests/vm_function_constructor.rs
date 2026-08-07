use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{
    CompilerExecutableKind, FunctionTemplateId, VerificationLimits, VerifiedBytecode,
};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, DynamicFunctionFamily, ExceptionKind, ExecutionError,
    ExecutionLimits, Function, JsNumber, JsString, OrdinaryDynamicFunctionCompiler,
    OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits, RuntimeResource, ValueKind,
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
        let (kind, source_name): (DynamicFunctionKind, Arc<str>) = match source.family() {
            DynamicFunctionFamily::Function => (
                DynamicFunctionKind::Function,
                Arc::from("<runtime Function>"),
            ),
            DynamicFunctionFamily::GeneratorFunction => (
                DynamicFunctionKind::GeneratorFunction,
                Arc::from("<runtime GeneratorFunction>"),
            ),
            DynamicFunctionFamily::AsyncFunction => (
                DynamicFunctionKind::AsyncFunction,
                Arc::from("<runtime AsyncFunction>"),
            ),
            DynamicFunctionFamily::AsyncGeneratorFunction => (
                DynamicFunctionKind::AsyncGeneratorFunction,
                Arc::from("<runtime AsyncGeneratorFunction>"),
            ),
        };
        let dynamic_source =
            DynamicFunctionSource::new(kind, &parameters, SourceFragment::new(&body_text));
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(unit, source_name)
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
    let authority = dynamic_function_authority(parameters, body);
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn dynamic_function_authority(parameters: &[&str], body: &str) -> Arc<VerifiedBytecode> {
    let parameters = parameters
        .iter()
        .map(|parameter| JsString::from_utf8(parameter).expect("parameter"))
        .collect::<Vec<_>>();
    TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from(parameters),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority")
}

fn compiler() -> Arc<dyn OrdinaryDynamicFunctionCompiler> {
    Arc::new(TestCompiler)
}

fn ordinary_dynamic_function_template(authority: &VerifiedBytecode) -> FunctionTemplateId {
    let index = authority
        .functions()
        .position(|function| {
            function.metadata().executable_kind() == CompilerExecutableKind::OrdinaryFunction
        })
        .expect("ordinary dynamic function");
    FunctionTemplateId::new(u32::try_from(index).expect("small template index"))
}

fn reserved_frame_values(authority: &VerifiedBytecode, function: FunctionTemplateId) -> u64 {
    let control_flow = authority
        .function(function)
        .expect("function")
        .function()
        .control_flow();
    let domains = control_flow.domains();
    u64::from(domains.argument_count())
        + u64::from(domains.local_count())
        + u64::from(control_flow.computed_stack_size())
        + 1
}

fn assert_number(value: &quickjs_runtime::JsValue, expected: i32) {
    let number = value.as_number().expect("live value").expect("number");
    assert!(number.strict_equals(JsNumber::from_i32(expected)));
}

fn assert_string(value: &quickjs_runtime::JsValue, expected: &str) {
    let string = value.as_string().expect("live value").expect("string");
    assert_eq!(string.to_utf8_lossy().expect("UTF-8"), expected);
}

#[test]
fn async_function_constructor_has_exact_intrinsics_and_executes_await() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let C=(async function(){}).constructor;\
         let f=new C('value','return await value;');\
         let p=Object.getPrototypeOf(f);\
         let cd=Object.getOwnPropertyDescriptor(p,'constructor');\
         let pd=Object.getOwnPropertyDescriptor(C,'prototype');\
         let nonconstructable=false;\
         try{new f();}catch(error){nonconstructable=error instanceof TypeError;}\
         let state={result:C.name+'|'+C.length+'|'+\
             (Object.getPrototypeOf(C)===Function)+'|'+\
             (Object.getPrototypeOf(p)===Function.prototype)+'|'+\
             (f.prototype===undefined)+'|'+\
             nonconstructable+'|'+\
             cd.writable+','+cd.enumerable+','+cd.configurable+'|'+\
             pd.writable+','+pd.enumerable+','+pd.configurable+'|'};\
         f(7).then(function(value){state.result=state.result+value;});\
         return state;",
    );
    let read = dynamic_function(&mut context, &["state"], "return state.result;");

    let state = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("AsyncFunction construction");
    let result = context
        .call(&read, &[state], ExecutionLimits::default())
        .expect("async result");

    assert_string(
        &result,
        "AsyncFunction|1|true|true|true|true|false,false,true|false,false,false|7",
    );
}

#[test]
fn async_generator_function_constructor_has_exact_intrinsics_and_executes_yield() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let C=(async function*(){}).constructor;\
         let f=new C('value','yield await value;');\
         let p=Object.getPrototypeOf(f);\
         let gp=Object.getPrototypeOf(f.prototype);\
         let cd=Object.getOwnPropertyDescriptor(p,'constructor');\
         let pd=Object.getOwnPropertyDescriptor(C,'prototype');\
         let nonconstructable=false;\
         try{new f();}catch(error){nonconstructable=error instanceof TypeError;}\
         let state={result:C.name+'|'+C.length+'|'+\
             (Object.getPrototypeOf(C)===Function)+'|'+\
             (Object.getPrototypeOf(p)===Function.prototype)+'|'+\
             (Object.getPrototypeOf(gp)[Symbol.asyncIterator].call({})!==undefined)+'|'+\
             nonconstructable+'|'+\
             cd.writable+','+cd.enumerable+','+cd.configurable+'|'+\
             pd.writable+','+pd.enumerable+','+pd.configurable+'|'};\
         f(7).next().then(function(result){\
             state.result=state.result+result.value+':'+result.done;\
         });\
         return state;",
    );
    let read = dynamic_function(&mut context, &["state"], "return state.result;");

    let state = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("AsyncGeneratorFunction construction");
    let result = context
        .call(&read, &[state], ExecutionLimits::default())
        .expect("async-generator result");

    assert_string(
        &result,
        "AsyncGeneratorFunction|1|true|true|true|true|false,false,true|false,false,false|7:false",
    );
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
fn generated_function_accepts_a_formal_rest_fragment() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('fixed','...rest',\
            'return arguments.length*100+rest.length*10+rest[1];');\
            return f.length*1000+f(1,2,3);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function rest parameter");

    assert_number(&result, 1_323);
}

#[test]
fn generated_function_accepts_parameter_default_expressions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('a=1','b=a+1','return a*10+b;');\
            return f.length*10000+f()*100+f(5);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function parameter defaults");

    assert_number(&result, 1_256);
}

#[test]
fn generated_function_infers_anonymous_parameter_default_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('callback=(function(){})','{nested=function(){}}={}',\
            'return callback.name===\"callback\"&&nested.name===\"nested\";');\
            return f();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function anonymous parameter names");

    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_infers_anonymous_declaration_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('let local=(function(){});let {nested=function(){}}={};\
            return local.name===\"local\"&&nested.name===\"nested\";');\
            return f();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function anonymous declaration names");

    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_infers_anonymous_assignment_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('let local,logical=false;local=(function(){});\
            logical||=function(){};generatedGlobal=function(){};\
            return local.name===\"local\"&&logical.name===\"logical\"&&\
                generatedGlobal.name===\"generatedGlobal\";');\
            return f();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function anonymous assignment names");

    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_infers_destructuring_assignment_default_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('let arrayElement,objectElement;\
            [arrayElement=function(){}]=[];\
            ({objectElement=function(){}}={});\
            [generatedDefault=function(){}]=[];\
            return arrayElement.name===\"arrayElement\"&&\
                objectElement.name===\"objectElement\"&&\
                generatedDefault.name===\"generatedDefault\";');\
            return f();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function destructuring assignment default names");

    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_infers_static_data_property_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('const object={handler:function(){},1:function(){},\
            \"__proto__\":function(){}};\
            return object.handler.name===\"handler\"&&object[1].name===\"1\"&&\
                object.__proto__.name===\"__proto__\";');\
            return f();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function static data-property names");

    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_infers_computed_data_property_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let f=Function('const key=\"computed\",symbol=Symbol(\"token\"),empty=Symbol();\
            const object={[key]:function(){},[symbol]:function(){},[empty]:function(){}};\
            const descriptor=Object.getOwnPropertyDescriptor(object[key],\"name\");\
            return object[key].name===\"computed\"&&object[symbol].name===\"[token]\"&&\
                object[empty].name===\"\"&&!descriptor.writable&&!descriptor.enumerable&&\
                descriptor.configurable;');\
            return f();",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function computed data-property names");

    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn generated_function_splits_parameter_and_body_environments() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let copied=Function('a=1','var a;return a;');\
            let separated=Function('a=1','reader=function inner(){return a;}',\
                'var a=2;return reader()*10+a;');\
            let declared=Function('a=1','reader=function inner(){return a;}',\
                'function a(){return 3;}return reader()*10+a();');\
            let args=Function('value=arguments.length',\
                'var arguments;return value*10+arguments.length;');\
            return copied()*1000000+separated()*10000+declared()*100+args(undefined,6);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function parameter/body environments");

    assert_number(&result, 1_121_322);
}

#[test]
fn function_prototype_call_forwards_the_dynamic_function_compiler_service() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return Function.call(null,'value','return value;')(13);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Function.prototype.call dynamic target");

    assert_number(&result, 13);
}

#[test]
fn function_prototype_call_lets_the_target_normalize_a_sloppy_nullish_receiver() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "return Function('this.callMarker=21;return callMarker;').call(null);",
    );

    let result = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("sloppy target receiver");

    assert_number(&result, 21);
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
fn generator_source_units_include_the_generator_wrapper_token() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let GeneratorFunction=(function*(){}).constructor; return GeneratorFunction();",
    );

    let error = context
        .call_with_dynamic_function_compiler(
            &run,
            &[],
            ExecutionLimits::default().with_dynamic_source_code_units(28),
            &compiler(),
        )
        .expect_err("empty generator wrapper contains 29 UTF-16 code units");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::DynamicSourceCodeUnits,
            limit: 28,
            observed: 29,
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
fn invalid_chained_continue_throws_the_exact_syntax_error_without_installation() {
    let source = "return Function('outer: inner: { continue outer; }');";
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &[], source);
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("invalid chained continue target");
    let ExecutionError::Exception(exception) = error else {
        panic!("chained continue rejection must be a JavaScript exception");
    };

    assert_eq!(exception.kind(), Some(ExceptionKind::SyntaxError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "break/continue label not found"
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

/// `AddRestrictedFunctionProperties` installs one realm-owned
/// `%ThrowTypeError%` as both accessors for `caller` and `arguments`.
#[test]
fn function_prototype_has_restricted_caller_and_arguments_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let inspect = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "var caller=Object.getOwnPropertyDescriptor(Function.prototype,'caller');\
         var args=Object.getOwnPropertyDescriptor(Function.prototype,'arguments');\
         var thrower=caller.get,name=Object.getOwnPropertyDescriptor(thrower,'name'),\
             length=Object.getOwnPropertyDescriptor(thrower,'length');\
         var getError=false,setError=false;\
         try{Function.prototype.caller;}catch(error){\
           getError=error instanceof TypeError&&error.message==='invalid property access';}\
         try{Function.prototype.arguments=1;}catch(error){\
           setError=error instanceof TypeError&&error.message==='invalid property access';}\
         return Object.getOwnPropertyNames(Function.prototype).join(',')===\
           'length,name,caller,arguments,call,apply,bind,toString,constructor'&&\
           caller.get===caller.set&&caller.get===args.get&&args.get===args.set&&\
           !caller.enumerable&&caller.configurable&&!args.enumerable&&args.configurable&&\
           thrower.name===''&&thrower.length===0&&!Object.isExtensible(thrower)&&\
           !name.writable&&!name.enumerable&&!name.configurable&&\
           !length.writable&&!length.enumerable&&!length.configurable&&getError&&setError;",
    );
    let value = runtime
        .context(&realm)
        .expect("context")
        .call(&inspect, &[], ExecutionLimits::default())
        .expect("restricted Function properties");

    assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn function_prototype_call_has_native_source_and_is_not_constructable() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let source = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return Function.prototype.call.toString();",
    );
    let construct = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return new Function.prototype.call();",
    );
    let mut context = runtime.context(&realm).expect("context");

    let source = context
        .call(&source, &[], ExecutionLimits::default())
        .expect("native source");
    assert_eq!(
        source
            .as_string()
            .expect("live source")
            .expect("source string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "function call() {\n    [native code]\n}"
    );

    let error = context
        .call(&construct, &[], ExecutionLimits::default())
        .expect_err("call is not a constructor");
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
        "call is not a constructor"
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

#[test]
fn object_prototype_conversion_methods_cover_current_object_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let display = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "let object={};return object.toString();",
    );
    let identity = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "let object={};return object.valueOf()===object;",
    );
    let mut context = runtime.context(&realm).expect("context");

    let value = context
        .call(&display, &[], ExecutionLimits::default())
        .expect("Object.prototype.toString");
    assert_eq!(
        value
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "[object Object]"
    );
    let value = context
        .call(&identity, &[], ExecutionLimits::default())
        .expect("Object.prototype.valueOf");
    assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn object_prototype_conversion_methods_handle_unbound_nullish_receivers_exactly() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let display = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "let stringify={}.toString;return stringify();",
    );
    let value_of = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "let valueOf={}.valueOf;return valueOf();",
    );
    let mut context = runtime.context(&realm).expect("context");

    let value = context
        .call(&display, &[], ExecutionLimits::default())
        .expect("undefined Object.prototype.toString receiver");
    assert_eq!(
        value
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "[object Undefined]"
    );

    let error = context
        .call(&value_of, &[], ExecutionLimits::default())
        .expect_err("undefined Object.prototype.valueOf receiver");
    let ExecutionError::Exception(exception) = error else {
        panic!("undefined valueOf receiver must throw");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "cannot convert to object"
    );
}

#[test]
fn function_prototype_to_string_returns_verified_and_native_source() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let verified = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return Function('value','return value;').toString();",
    );
    let native = dynamic_function(
        &mut runtime.context(&realm).expect("context"),
        &[],
        "return Function.toString();",
    );
    let mut context = runtime.context(&realm).expect("context");

    let verified_source = context
        .call_with_dynamic_function_compiler(
            &verified,
            &[],
            ExecutionLimits::default(),
            &compiler(),
        )
        .expect("verified source");
    assert_eq!(
        verified_source
            .as_string()
            .expect("live source")
            .expect("source string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "function anonymous(value\n) {\nreturn value;\n}"
    );

    let native_source = context
        .call(&native, &[], ExecutionLimits::default())
        .expect("native source");
    assert_eq!(
        native_source
            .as_string()
            .expect("live source")
            .expect("source string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "function Function() {\n    [native code]\n}"
    );
}

#[test]
fn function_prototype_to_string_rejects_an_unbound_receiver() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let stringify=Function.prototype.toString;return stringify();",
    );

    let error = context
        .call(&run, &[], ExecutionLimits::default())
        .expect_err("undefined receiver is not callable");
    let ExecutionError::Exception(exception) = error else {
        panic!("wrong receiver must throw");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "not a function"
    );
}

#[test]
fn function_source_objects_coerce_left_to_right_through_bytecode_methods() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let parameter={toString:function parameterString(){phase=1;return 'value';}};\
         let body={toString:function bodyString(){phase=2;return 'return phase;';}};\
         let generated=Function(parameter,body);\
         return generated(3);",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("object source conversion");

    assert_number(&value, 2);
}

#[test]
fn function_source_falls_back_from_object_to_string_result_to_value_of() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let source={\
             toString:function sourceToString(){return {};},\
             valueOf:function sourceValueOf(){return 'return 7;';}\
         };\
         return Function(source)();",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("valueOf fallback");

    assert_number(&value, 7);
}

#[test]
fn function_source_rejects_an_object_after_both_ordinary_methods() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let source={\
             toString:0,\
             valueOf:function sourceValueOf(){return {};}\
         };\
         return Function(source);",
    );

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("object source has no primitive conversion");
    let ExecutionError::Exception(exception) = error else {
        panic!("ordinary conversion failure must be a JavaScript exception");
    };

    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "toPrimitive"
    );
}

#[test]
fn function_source_conversion_uses_proxy_get() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let log='';let source=new Proxy({}, {get(target,key,receiver){\
             log=log+(typeof key==='symbol'?'@':key)+',';\
             if(typeof key==='symbol'){return undefined;}\
             if(key==='toString'){return function(){return 'return 9;';};}\
             return Reflect.get(target,key,receiver);\
         }});\
         return Function(source)()+'|'+log;",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("Proxy-backed dynamic Function source");
    assert_eq!(
        value
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "9|@,toString,"
    );
}

#[test]
fn function_source_bytecode_throw_stops_conversion_and_escapes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let source={\
             toString:function sourceToString(){throw 41;},\
             valueOf:function sourceValueOf(){throw 42;}\
         };\
         return Function(source);",
    );

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("source coercion throw");
    let ExecutionError::Exception(exception) = error else {
        panic!("bytecode throw must remain a JavaScript exception");
    };
    let thrown = exception
        .thrown_value()
        .expect("explicit throw")
        .as_number()
        .expect("live value")
        .expect("number");

    assert!(thrown.strict_equals(JsNumber::from_i32(41)));
    assert_eq!(exception.caller_frames().len(), 1);
    assert!(
        exception.caller_frames()[0]
            .source_text()
            .contains("Function(source)")
    );
}

#[test]
fn function_values_use_the_native_function_to_string_during_source_conversion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let source=function namedSource(){return 1;};\
         return Function(source)();",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("function source conversion");

    assert_eq!(value.kind().expect("live value"), ValueKind::Undefined);
}

#[test]
fn native_function_used_as_to_string_resumes_the_outer_conversion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        &[],
        "let source={\
             toString:Function,\
             valueOf:function sourceValueOf(){return 'return 8;';}\
         };\
         return Function(source)();",
    );

    let value = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect("nested native conversion method");

    assert_number(&value, 8);
}

#[test]
fn host_direct_function_call_resumes_bytecode_source_conversion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let expose_constructor = dynamic_function(&mut context, &[], "return Function;");
    let make_source = dynamic_function(
        &mut context,
        &[],
        "return {toString:function sourceToString(){return 'return 13;';}};",
    );
    let constructor = context
        .call(&expose_constructor, &[], ExecutionLimits::default())
        .expect("global Function")
        .into_function()
        .expect("Function value");
    let source = context
        .call(&make_source, &[], ExecutionLimits::default())
        .expect("source object");

    let generated = context
        .call_with_dynamic_function_compiler(
            &constructor,
            &[source],
            ExecutionLimits::default(),
            &compiler(),
        )
        .expect("host Function call")
        .into_function()
        .expect("generated function");
    let value = context
        .call(&generated, &[], ExecutionLimits::default())
        .expect("generated function call");

    assert_number(&value, 13);
}

#[test]
fn suspended_source_conversion_counts_toward_the_frame_limit_and_cleans_up() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let expose_constructor = dynamic_function(&mut context, &[], "return Function;");
    let make_source = dynamic_function(
        &mut context,
        &[],
        "return {toString:function sourceToString(){return 'return 13;';}};",
    );
    let constructor = context
        .call(&expose_constructor, &[], ExecutionLimits::default())
        .expect("global Function")
        .into_function()
        .expect("Function value");
    let source = context
        .call(&make_source, &[], ExecutionLimits::default())
        .expect("source object");
    let baseline = context.runtime_usage();

    for _ in 0..2 {
        let error = context
            .call_with_dynamic_function_compiler(
                &constructor,
                std::slice::from_ref(&source),
                ExecutionLimits::default(),
                &compiler(),
            )
            .expect_err("coercion continuation plus method frame exceeds the limit");
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
fn native_immediate_source_conversion_obeys_the_suspended_frame_limit() {
    let authority = dynamic_function_authority(&[], "return Function({});");
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("run");
    let cleanup = dynamic_function(&mut context, &[], "return 0;");
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("native toString must not bypass the suspended-frame ceiling");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::Frames,
            limit: 1,
            observed: 2,
        }
    ));
    context
        .call(&cleanup, &[], ExecutionLimits::default())
        .expect("collection safe point");
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn native_throwing_source_conversion_obeys_the_suspended_frame_limit() {
    let authority = dynamic_function_authority(
        &[],
        "let source={toString:Function.prototype.toString};\
         return Function(source);",
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("run");
    let cleanup = dynamic_function(&mut context, &[], "return 0;");
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("native throw must not bypass the suspended-frame ceiling");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::Frames,
            limit: 1,
            observed: 2,
        }
    ));
    context
        .call(&cleanup, &[], ExecutionLimits::default())
        .expect("collection safe point");
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn constructor_source_conversion_charges_new_target_against_the_value_limit() {
    let authority = dynamic_function_authority(
        &[],
        "let first,second,third,fourth;\
         return new Function({});",
    );
    let function_template = ordinary_dynamic_function_template(&authority);
    let run_values = reserved_frame_values(&authority, function_template);
    let script_values = reserved_frame_values(&authority, authority.root_id());
    let limit = run_values.saturating_add(1);
    assert!(script_values <= limit);
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frame_values(limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("run");
    let cleanup = dynamic_function(&mut context, &[], "return 0;");
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("newTarget must count as a suspended heap edge");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::FrameValues,
            limit: actual_limit,
            observed,
        } if actual_limit == limit && observed == run_values + 2
    ));
    context
        .call(&cleanup, &[], ExecutionLimits::default())
        .expect("collection safe point");
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn primitive_constructor_source_keeps_new_target_charged_in_the_wrapper_frame() {
    let authority = dynamic_function_authority(&[], "return new Function('return 1;');");
    let function_template = ordinary_dynamic_function_template(&authority);
    let run_values = reserved_frame_values(&authority, function_template);
    let wrapper = dynamic_function_authority(&[], "return 1;");
    let wrapper_values = reserved_frame_values(&wrapper, wrapper.root_id());
    let limit = run_values.saturating_add(wrapper_values);
    assert!(reserved_frame_values(&authority, authority.root_id()) <= limit);
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frame_values(limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("run");
    let baseline = context.runtime_usage();

    let error = context
        .call_with_dynamic_function_compiler(&run, &[], ExecutionLimits::default(), &compiler())
        .expect_err("wrapper frame must retain and charge newTarget");

    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::FrameValues,
            limit: actual_limit,
            observed,
        } if actual_limit == limit && observed == limit + 1
    ));
    assert_eq!(context.runtime_usage(), baseline);
}
