//! Core `%ArrayBuffer%` construction, resizability, copying, and detachment.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
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
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime ArrayBuffer>"),
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

fn evaluate<T>(
    body: &str,
    project: impl FnOnce(Result<quickjs_runtime::JsValue, ExecutionError>) -> T,
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
fn array_buffer_construction_resize_transfer_slice_and_intrinsics_are_branded() {
    assert_eq!(
        rendered(
            "var fixed=new ArrayBuffer(4),resizable=new ArrayBuffer(2,{maxByteLength:6});\
             resizable.resize(5);var moved=resizable.transfer(3),slice=fixed.slice(1,3);\
             return [typeof ArrayBuffer,ArrayBuffer.length,ArrayBuffer.name,\
               fixed.byteLength,fixed.maxByteLength,fixed.resizable,\
               resizable.detached,resizable.byteLength,resizable.maxByteLength,resizable.resizable,\
               moved.byteLength,moved.maxByteLength,moved.resizable,\
               slice.byteLength,slice.resizable,Object.prototype.toString.call(fixed),\
               ArrayBuffer.isView(fixed),ArrayBuffer.isView({})].join('|');"
        ),
        "function|1|ArrayBuffer|4|4|false|true|0|0|true|3|6|true|2|false|[object ArrayBuffer]|false|false"
    );
}

#[test]
fn array_buffer_observable_conversion_and_species_order_matches_the_specification() {
    assert_eq!(
        rendered(
            "var log=[];var length={valueOf:function(){log.push('length');return 2;}};\
             var options={get maxByteLength(){log.push('max');return {valueOf:function(){log.push('max index');return 5;}}}};\
             var source=new ArrayBuffer(4);source.constructor={get [Symbol.species](){log.push('species');return ArrayBuffer;}};\
             var result=source.slice({valueOf:function(){log.push('start');return 1;}},{valueOf:function(){log.push('end');return 3;}});\
             var resized=new ArrayBuffer(length,options);\
             return [result.byteLength,resized.maxByteLength,log.join(',')].join('|');"
        ),
        "2|5|start,end,species,length,max,max index"
    );
}

#[test]
fn array_buffer_constructor_and_brand_failures_are_the_required_error_kinds() {
    assert_eq!(thrown("return ArrayBuffer(1);"), ExceptionKind::TypeError);
    assert_eq!(
        thrown("return new ArrayBuffer(-1);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new ArrayBuffer(2,{maxByteLength:1});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return ArrayBuffer.prototype.resize.call(new ArrayBuffer(1),1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn shared_array_buffer_construction_growth_views_slice_and_intrinsics_are_branded() {
    assert_eq!(
        rendered(
            "var fixed=new SharedArrayBuffer(4),growable=new SharedArrayBuffer(2,{maxByteLength:6});\
             var view=new Uint8Array(fixed);view[0]=7;growable.grow(5);var slice=fixed.slice(0,1);\
             return [typeof SharedArrayBuffer,SharedArrayBuffer.length,SharedArrayBuffer.name,\
               fixed.byteLength,fixed.maxByteLength,fixed.growable,view[0],\
               growable.byteLength,growable.maxByteLength,growable.growable,\
               slice.byteLength,Object.prototype.toString.call(fixed),\
               ArrayBuffer.isView(view),ArrayBuffer.isView(fixed)].join('|');"
        ),
        "function|1|SharedArrayBuffer|4|4|false|7|5|6|true|1|[object SharedArrayBuffer]|true|false"
    );
}

#[test]
fn shared_array_buffer_preserves_the_distinct_array_buffer_brand() {
    assert_eq!(
        thrown("return SharedArrayBuffer(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return ArrayBuffer.prototype.slice.call(new SharedArrayBuffer(1));"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return SharedArrayBuffer.prototype.slice.call(new ArrayBuffer(1));"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new SharedArrayBuffer(2,{maxByteLength:1});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new SharedArrayBuffer(1).grow(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new SharedArrayBuffer(7 * Math.pow(1024,5));"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new SharedArrayBuffer(0,{maxByteLength:7 * Math.pow(1024,5)});"),
        ExceptionKind::RangeError
    );
}
