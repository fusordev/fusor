//! Concrete typed-array constructors and their shared accessor surface.

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
                    Arc::from("<runtime TypedArray>"),
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
fn typed_array_constructors_allocate_fixed_views_and_expose_the_shared_accessors() {
    assert_eq!(
        rendered(
            "var a=new Int8Array(3),b=new BigInt64Array(1),f=new Float16Array(1);\
             a[0]=257;b[0]=3n;f[0]=1.5;\
             return [typeof Int8Array,Int8Array.length,Int8Array.name,\
               a.length,a.byteLength,a.byteOffset,a.buffer instanceof ArrayBuffer,a[0],\
               String(b[0]),f[0],Int8Array.BYTES_PER_ELEMENT,\
               Int8Array.prototype.BYTES_PER_ELEMENT,\
               Object.getPrototypeOf(Int8Array.prototype)===Object.getPrototypeOf(Uint8Array.prototype),\
               Object.prototype.toString.call(a),ArrayBuffer.isView(a)].join('|');"
        ),
        "function|3|Int8Array|3|3|0|true|1|3|1.5|1|1|true|[object Int8Array]|true"
    );
}

#[test]
fn typed_array_constructors_require_new_and_validate_the_length() {
    assert_eq!(thrown("return Int8Array(1);"), ExceptionKind::TypeError);
    assert_eq!(
        thrown("return new Uint8Array(-1);"),
        ExceptionKind::RangeError
    );
}

#[test]
fn typed_array_constructors_create_fixed_and_length_tracking_array_buffer_views() {
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(12,{maxByteLength:20});\
             var tracking=new Uint16Array(buffer,2),fixed=new Uint16Array(buffer,2,3);\
             tracking[0]=0x1234;fixed[2]=0x5678;buffer.resize(7);\
             return [tracking.buffer===buffer,tracking.byteOffset,tracking.length,\
               tracking[0],fixed.length,tracking.BYTES_PER_ELEMENT].join('|');"
        ),
        "true|2|2|4660|0|2"
    );
    assert_eq!(
        thrown("return new Uint16Array(new ArrayBuffer(3));"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Uint16Array(new ArrayBuffer(4),1);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Uint16Array(new ArrayBuffer(4),0,3);"),
        ExceptionKind::RangeError
    );
}
