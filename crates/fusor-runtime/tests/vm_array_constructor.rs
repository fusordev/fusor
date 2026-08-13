use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsNumber, JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Array>"))
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

fn boolean(value: &fusor_runtime::JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn string(value: &fusor_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn escaping_exception(result: Result<fusor_runtime::JsValue, ExecutionError>) {
    let Err(ExecutionError::Exception(exception)) = result else {
        panic!("invalid Array length must escape as a JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::RangeError));
    assert_eq!(
        exception
            .message()
            .expect("RangeError message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "invalid array length"
    );
}

#[test]
fn array_call_new_and_intrinsic_metadata_cover_the_core_vertical() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let called=Array(1,\"x\",true);\
         let constructed=new Array(2,3);\
         return Array.name===\"Array\"\
             && Array.length===1\
             && Array.prototype.constructor===Array\
             && Array.prototype.length===0\
             && Array().length===0\
             && (new Array()).length===0\
             && called.length===3\
             && called[0]===1\
             && called[1]===\"x\"\
             && called[2]===true\
             && constructed.length===2\
             && constructed[0]===2\
             && constructed[1]===3\
             && ({}).toString.call(constructed)===\"[object Array]\";",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Array call and construction");

    assert!(boolean(&value));
}

#[test]
fn array_unscopables_has_the_normative_null_prototype_table() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let table=Array.prototype[Symbol.unscopables];\
         let outer=Object.getOwnPropertyDescriptor(Array.prototype,Symbol.unscopables);\
         let names=[\
             'at','copyWithin','entries','fill','find','findIndex',\
             'findLast','findLastIndex','flat','flatMap','includes','keys',\
             'toReversed','toSorted','toSpliced','values'\
         ];\
         if(Object.getPrototypeOf(table)!==null||\
             outer.value!==table||outer.writable!==false||\
             outer.enumerable!==false||outer.configurable!==true||\
             Object.prototype.hasOwnProperty.call(table,'with')){\
             return false;\
         }\
         for(let index=0;index<names.length;index+=1){\
             let descriptor=Object.getOwnPropertyDescriptor(table,names[index]);\
             if(descriptor.value!==true||descriptor.writable!==true||\
                 descriptor.enumerable!==true||descriptor.configurable!==true){\
                 return false;\
             }\
         }\
         return true;",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("inspect Array.prototype[Symbol.unscopables]");

    assert!(boolean(&value));
}

#[test]
fn one_primitive_number_creates_a_sparse_exact_uint32_length() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let negativeZero=Array(-0);\
         let three=Array(3);\
         let maximum=new Array(4294967295);\
         let keys=\"\";\
         for(let key in three)keys+=key;\
         return negativeZero.length+\"|\"+three.length+\"|\"+three[0]+\"|\"\
             +keys+\"|\"+maximum.length+\"|\"+maximum[4294967294];",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("sparse Array lengths");

    assert_eq!(string(&value), "0|3|undefined||4294967295|undefined");
}

#[test]
fn invalid_primitive_number_lengths_throw_the_exact_realm_range_error() {
    for expression in [
        "Array(1.5)",
        "new Array(-1)",
        "Array(1/0)",
        "new Array(0/0)",
    ] {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let run = dynamic_function(&mut context, &format!("return {expression};"));

        escaping_exception(context.call(&run, &[], ExecutionLimits::default()));
    }
}

#[test]
fn a_single_non_number_is_element_zero_without_coercion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let hits=0;\
         let exotic={valueOf(){hits+=1;return 4;}};\
         let boxed=new Number(3);\
         let first=Array(exotic);\
         let second=new Array(boxed);\
         let third=Array(\"3\");\
         return hits===0\
             && first.length===1&&first[0]===exotic\
             && second.length===1&&second[0]===boxed\
             && third.length===1&&third[0]===\"3\";",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("non-Number single elements");

    assert!(boolean(&value));
}

#[test]
fn exact_numeric_boundaries_do_not_coerce_or_round() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let valid = dynamic_function(&mut context, "return Array(4294967295).length;");
    let invalid = dynamic_function(&mut context, "return Array(4294967295.5).length;");

    let value = context
        .call(&valid, &[], ExecutionLimits::default())
        .expect("maximum uint32 length");
    let number = value.as_number().expect("live value").expect("Number");
    assert!(number.strict_equals(JsNumber::from_f64(4_294_967_295.0)));
    escaping_exception(context.call(&invalid, &[], ExecutionLimits::default()));
}
