use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    DynamicFunctionCompileFailure, ExecutionLimits, JsString, JsValue,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Error>"))
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

fn call<T>(body: &str, inspect: impl FnOnce(&JsValue) -> T) -> T {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function");
    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Error operation");
    inspect(&value)
}

fn boolean(value: &JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn string(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn error_families_publish_exact_core_metadata_and_branded_call_new_results() {
    let result = call(
        "\
            let a=Error(\"a\");\
            let b=new EvalError(\"b\");\
            let c=RangeError(\"c\");\
            let d=new ReferenceError(\"d\");\
            let e=SyntaxError(\"e\");\
            let f=new TypeError(\"f\");\
            let g=URIError(\"g\");\
            let h=new InternalError(\"h\");\
            return Error.length===1&&Error.name===\"Error\"\
                &&Error.prototype.constructor===Error\
                &&Error.prototype.name===\"Error\"\
                &&Error.prototype.message===\"\"\
                &&Error.prototype.toString.length===0\
                &&Error.prototype.toString.name===\"toString\"\
                &&EvalError.length===1&&EvalError.name===\"EvalError\"\
                &&EvalError.prototype.constructor===EvalError\
                &&EvalError.prototype.name===\"EvalError\"\
                &&RangeError.prototype.constructor===RangeError\
                &&ReferenceError.prototype.constructor===ReferenceError\
                &&SyntaxError.prototype.constructor===SyntaxError\
                &&TypeError.prototype.constructor===TypeError\
                &&URIError.prototype.constructor===URIError\
                &&InternalError.prototype.constructor===InternalError\
                &&a.message===\"a\"&&b.message===\"b\"\
                &&c.message===\"c\"&&d.message===\"d\"\
                &&e.message===\"e\"&&f.message===\"f\"\
                &&g.message===\"g\"&&h.message===\"h\"\
                &&Error.isError(a)&&Error.isError(b)&&Error.isError(h)\
                &&!Error.isError({});",
        boolean,
    );
    assert!(result);
}

#[test]
fn error_message_and_cause_follow_quickjs_conversion_and_get_order() {
    let result = call(
        "\
            let log=\"\";\
            let message={\
                toString(){log=log+\"message-toString,\";return \"boom\";},\
                valueOf(){log=log+\"message-valueOf,\";return 1;}\
            };\
            let options={get cause(){log=log+\"cause-get,\";return 17;}};\
            let error=TypeError(message,options);\
            let absent=Error(void 0,{cause:void 0});\
            return log+\"|\"+error.name+\"|\"+error.message+\"|\"+error.cause\
                +\"|\"+Error.prototype.toString.call(error)\
                +\"|\"+absent.message+\"|\"+(absent.cause===void 0);",
        string,
    );
    assert_eq!(
        result,
        "message-toString,cause-get,|TypeError|boom|17|TypeError: boom||true"
    );
}

#[test]
fn error_prototype_to_string_gets_and_converts_name_before_message() {
    let result = call(
        "\
            let log=\"\";\
            let value={\
                get name(){\
                    log=log+\"get-name,\";\
                    return {toString(){log=log+\"name-toString,\";return \"Named\";}};\
                },\
                get message(){\
                    log=log+\"get-message,\";\
                    return {toString(){log=log+\"message-toString,\";return \"detail\";}};\
                }\
            };\
            let rendered=Error.prototype.toString.call(value);\
            let defaults=Error.prototype.toString.call({});\
            let emptyName=Error.prototype.toString.call({name:\"\",message:\"m\"});\
            let emptyMessage=Error.prototype.toString.call({name:\"N\",message:\"\"});\
            return rendered+\"|\"+log+\"|\"+defaults+\"|\"+emptyName+\"|\"+emptyMessage;",
        string,
    );
    assert_eq!(
        result,
        "Named: detail|get-name,name-toString,get-message,message-toString,|Error|m|N"
    );
}

#[test]
fn error_prototype_to_string_rejects_nonobjects_and_stops_on_abrupt_name() {
    let result = call(
        "\
            let first;\
            try{Error.prototype.toString.call(null);}catch(error){\
                first=error.name+\":\"+error.message;\
            }\
            let log=\"\";\
            let second;\
            try{\
                Error.prototype.toString.call({\
                    get name(){log=log+\"name,\";throw \"stop\";},\
                    get message(){log=log+\"message,\";return \"late\";}\
                });\
            }catch(error){second=error;}\
            let third;\
            try{Error.prototype.toString.call({name:Symbol(\"x\")});}\
            catch(error){third=error.name+\":\"+error.message;}\
            return first+\"|\"+log+second+\"|\"+third;",
        string,
    );
    assert_eq!(
        result,
        "TypeError:not an object|name,stop|TypeError:cannot convert symbol to string"
    );
}

#[test]
fn error_stack_is_headerless_and_starts_at_the_calling_function() {
    let error_stack = call(
        "\
            function makeError(){return Error(\"boom\").stack;}\
            return makeError();",
        string,
    );
    assert!(error_stack.starts_with("    at makeError ("));

    let aggregate_stack = call(
        "\
            function makeAggregate(){return AggregateError([]).stack;}\
            return makeAggregate();",
        string,
    );
    assert!(aggregate_stack.starts_with("    at makeAggregate ("));
}

#[test]
fn caught_engine_errors_are_branded_and_freeze_a_throw_site_stack() {
    let stack = call(
        "\
            function fail(){return null.value;}\
            try{fail();}catch(error){\
                if(!Error.isError(error))return \"unbranded\";\
                return error.stack;\
            }",
        string,
    );
    assert!(stack.starts_with("    at fail ("));
}
