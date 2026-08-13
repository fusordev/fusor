//! `%DataView%` construction, resizable bounds, and element access semantics.

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime DataView>"))
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

fn evaluate<T>(
    body: &str,
    project: impl FnOnce(Result<fusor_runtime::JsValue, ExecutionError>) -> T,
) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    project(context.call(&run, &[], ExecutionLimits::default()))
}

fn rendered(body: &str) -> String {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn thrown(body: &str) -> ExceptionKind {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected JavaScript exception");
        };
        exception.kind().expect("engine exception kind")
    })
}

#[test]
fn data_view_reads_writes_and_brands_every_element_family() {
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(24),view=new DataView(buffer,1,20);\
             view.setUint16(0,0x1234);view.setInt8(2,-2);\
             view.setFloat16(4,1.5,true);view.setFloat32(6,1.5,true);\
             view.setBigInt64(10,-2n,true);view.setUint16(18,0x5678,'');\
             return [typeof DataView,DataView.length,DataView.name,view.buffer===buffer,\
               view.byteOffset,view.byteLength,Object.prototype.toString.call(view),\
               view.getUint8(0),view.getUint8(1),view.getInt8(2),\
               view.getFloat16(4,true),view.getFloat32(6,true),\
               String(view.getBigInt64(10,true)),view.getUint8(18),view.getUint8(19),\
               ArrayBuffer.isView(view),\
               ArrayBuffer.isView(buffer)].join('|');"
        ),
        "function|1|DataView|true|1|20|[object DataView]|18|52|-2|1.5|1.5|-2|86|120|true|false"
    );
}

#[test]
fn data_view_uses_resizable_buffer_witnesses_and_coerces_in_spec_order() {
    assert_eq!(
        rendered(
            "var log=[],buffer=new ArrayBuffer(8,{maxByteLength:12});\
             var offset={valueOf:function(){log.push('offset');return 2;}};\
             var length={valueOf:function(){log.push('length');return 4;}};\
             var fixed=new DataView(buffer,offset,length),auto=new DataView(buffer,2);\
             buffer.resize(5);var fixedOut=false;try{fixed.byteLength}catch(error){fixedOut=error instanceof TypeError;}\
             var autoLength=auto.byteLength;buffer.resize(1);var autoOut=false;\
             try{auto.byteOffset}catch(error){autoOut=error instanceof TypeError;}\
             return [log.join(','),fixedOut,autoLength,autoOut].join('|');"
        ),
        "offset,length|true|3|true"
    );
}

#[test]
fn data_view_constructor_rechecks_resized_buffer_after_prototype_lookup() {
    assert_eq!(
        rendered(
            "function target(buffer,size){\
               var newTarget=function(){}.bind(null);\
               Object.defineProperty(newTarget,'prototype',{get:function(){buffer.resize(size);}});\
               return newTarget;\
             }\
             var fixedBuffer=new ArrayBuffer(3,{maxByteLength:3}),fixedError;\
             try{Reflect.construct(DataView,[fixedBuffer,1,2],target(fixedBuffer,2));}\
             catch(error){fixedError=error.constructor===RangeError;}\
             var autoBuffer=new ArrayBuffer(3,{maxByteLength:3}),autoError;\
             try{Reflect.construct(DataView,[autoBuffer,2],target(autoBuffer,1));}\
             catch(error){autoError=error.constructor===RangeError;}\
             return [fixedError,autoError].join('|');"
        ),
        "true|true"
    );
}

#[test]
fn data_view_constructor_and_accessors_reject_invalid_receivers_and_ranges() {
    assert_eq!(
        thrown("return DataView(new ArrayBuffer(1));"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new DataView({},0);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new DataView(new ArrayBuffer(1),2);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return DataView.prototype.getUint8.call({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("var view=new DataView(new ArrayBuffer(8));view.setBigInt64(0,1);"),
        ExceptionKind::TypeError
    );
}
