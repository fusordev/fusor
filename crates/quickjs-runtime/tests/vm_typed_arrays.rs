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

#[test]
fn typed_array_constructors_clone_typed_array_sources_and_check_content_types() {
    assert_eq!(
        rendered(
            "var source=new Int16Array(3);source[0]=-2;source[1]=258;source[2]=7;\
             var converted=new Uint8Array(source),same=new Int16Array(source);source[0]=9;\
             return [converted.length,converted[0],converted[1],converted[2],\
               same[0],same[1],same[2]].join('|');"
        ),
        "3|254|2|7|-2|258|7"
    );
    assert_eq!(
        thrown("return new BigInt64Array(new Int8Array(1));"),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_constructors_initialize_iterable_and_array_like_inputs_in_their_distinct_orders() {
    assert_eq!(
        rendered(
            "var iterable=new Uint8Array([1,258,3]);\
             var arrayLike={0:4,1:261,length:2};var copied=new Uint8Array(arrayLike);\
             var bigints=new BigInt64Array([1n,-2n]);\
             return [iterable.length,iterable[0],iterable[1],iterable[2],\
               copied.length,copied[0],copied[1],String(bigints[0]),String(bigints[1])].join('|');"
        ),
        "3|1|2|3|2|4|5|1|-2"
    );
}

#[test]
fn typed_array_constructors_expose_the_shared_species_getter() {
    assert_eq!(
        rendered(
            "return [Int8Array[Symbol.species]===Int8Array,\
             BigInt64Array[Symbol.species]===BigInt64Array,\
             Object.getOwnPropertyDescriptor(Int8Array,Symbol.species).get.name].join('|');"
        ),
        "true|true|get [Symbol.species]"
    );
}

#[test]
fn typed_array_set_copies_typed_and_array_like_sources_with_fresh_target_indices() {
    assert_eq!(
        rendered(
            "var target=new Uint8Array([1,2,3,4]),source=new Int16Array([9,258]);\
             target.set(source,1);\
             var overlap=new Uint8Array([1,2,3,4]);overlap.set(new Uint8Array(overlap.buffer,0,3),1);\
             var arrayLike={0:-2,1:260,length:2},second=new Uint8Array(3);second.set(arrayLike,1);\
             var bigints=new BigInt64Array(2);bigints.set({0:1n,1:-2n,length:2});\
             return [target[0],target[1],target[2],target[3],overlap[0],overlap[1],overlap[2],overlap[3],second[0],second[1],second[2],\
               String(bigints[0]),String(bigints[1])].join('|');"
        ),
        "1|9|2|4|1|1|2|3|0|254|4|1|-2"
    );
    assert_eq!(
        thrown("new Uint8Array(1).set(new Uint8Array(2));"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("new Uint8Array(1).set([1],-1);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("new BigInt64Array(1).set(new Int8Array(1));"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),target=new Uint8Array(buffer);\
             var source={length:1,get 0(){buffer.resize(0);return 1}};target.set(source);"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_subarray_uses_relative_bounds_shared_storage_and_species() {
    assert_eq!(
        rendered(
            "var source=new Uint16Array([1,2,3,4]),zeroStart=source.subarray(0,1),view=source.subarray(1,-1);view[0]=99;\
             var speciesSource=new Uint8Array([7,8,9]);\
             speciesSource.constructor={[Symbol.species]:Uint8Array};var speciesView=speciesSource.subarray(1,2);\
             return [zeroStart[0],view.length,view.byteOffset,view[0],source[1],view.buffer===source.buffer,\
               speciesView.constructor===Uint8Array,speciesView.length,speciesView[0]].join('|');"
        ),
        "1|2|2|99|99|true|true|1|8"
    );
    assert_eq!(
        thrown(
            "var source=new Uint8Array(8);source.constructor={[Symbol.species]:BigInt64Array};source.subarray(0,1);"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(8,{maxByteLength:8}),source=new Uint8Array(buffer);\
             source.subarray({valueOf(){buffer.resize(2);return 1}});"
        ),
        ExceptionKind::RangeError
    );
}

#[test]
fn typed_array_at_uses_the_initial_length_but_a_fresh_element_witness() {
    assert_eq!(
        rendered(
            "var values=new Int16Array([3,4,5]);\
             return [values.at(0),values.at(-1),values.at(3),values.at(-4)].join('|');"
        ),
        "3|5||"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);\
             values[1]=7;return String(values.at({valueOf(){buffer.resize(1);return 1}}));"
        ),
        "undefined"
    );
}

#[test]
fn typed_array_includes_uses_same_value_zero_without_coercing_the_search_value() {
    assert_eq!(
        rendered(
            "var floats=new Float32Array([NaN,-0,4]),bigints=new BigInt64Array([1n]);\
             return [floats.includes(NaN),floats.includes(0),floats.includes(4,3),\
               floats.includes(NaN,1),bigints.includes(1n),bigints.includes(1),\
               new Uint8Array(0).includes(0,{valueOf(){throw new Error('unexpected')}})].join('|');"
        ),
        "true|true|false|false|true|false|false"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);\
             values[1]=7;return String(values.includes(7,{valueOf(){buffer.resize(1);return 0}}));"
        ),
        "false"
    );
}

#[test]
fn typed_array_index_of_uses_strict_equality_with_fresh_element_witnesses() {
    assert_eq!(
        rendered(
            "var floats=new Float32Array([NaN,-0,4]),bigints=new BigInt64Array([1n]);\
             return [Uint8Array.prototype.indexOf.length,Uint8Array.prototype.indexOf.name,\
               floats.indexOf(NaN),floats.indexOf(0),floats.indexOf(4,3),\
               floats.indexOf(4,-1),bigints.indexOf(1n),bigints.indexOf(1),\
               new Uint8Array(0).indexOf(0,{valueOf(){throw new Error('unexpected')}})].join('|');"
        ),
        "1|indexOf|-1|1|-1|2|0|-1|-1"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);\
             values[1]=7;return String(values.indexOf(7,{valueOf(){buffer.resize(1);return 0}}));"
        ),
        "-1"
    );
}

#[test]
fn typed_array_reverse_mutates_in_place_for_number_and_bigint_content() {
    assert_eq!(
        rendered(
            "var numbers=new Int16Array([1,-2,3]),bigints=new BigInt64Array([1n,-2n]);\
             var returned=numbers.reverse();bigints.reverse();\
             return [returned===numbers,numbers[0],numbers[1],numbers[2],\
               String(bigints[0]),String(bigints[1])].join('|');"
        ),
        "true|3|-2|1|-2|1"
    );
}
