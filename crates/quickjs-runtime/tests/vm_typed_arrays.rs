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
fn typed_array_last_index_of_preserves_an_absent_from_index_and_uses_strict_equality() {
    assert_eq!(
        rendered(
            "var values=new Float32Array([1,2,1,NaN]),bigints=new BigInt64Array([1n,2n,1n]);\
             return [Uint8Array.prototype.lastIndexOf.length,Uint8Array.prototype.lastIndexOf.name,\
               values.lastIndexOf(1),values.lastIndexOf(1,undefined),values.lastIndexOf(1,Infinity),\
               values.lastIndexOf(1,-1),values.lastIndexOf(1,-3),values.lastIndexOf(1,-4),\
               values.lastIndexOf(1,-5),values.lastIndexOf(NaN),bigints.lastIndexOf(1n),bigints.lastIndexOf(1),\
               new Uint8Array(0).lastIndexOf(0,{valueOf(){throw new Error('unexpected')}})].join('|');"
        ),
        "1|lastIndexOf|2|0|2|2|0|0|-1|-1|2|-1|-1"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);\
             values[3]=7;return String(values.lastIndexOf(7,{valueOf(){buffer.resize(1);return Infinity}}));"
        ),
        "-1"
    );
    assert_eq!(
        thrown("return new Uint8Array(1).lastIndexOf(0,1n);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_fill_converts_value_before_range_and_revalidates_resizable_views() {
    assert_eq!(
        rendered(
            "var values=new Uint8Array([1,2,3,4]),bigints=new BigInt64Array([1n,2n,3n]);\
             var returned=values.fill(258,1,-1);bigints.fill(-2n,1);\
             return [Uint8Array.prototype.fill.length,Uint8Array.prototype.fill.name,returned===values,\
               values[0],values[1],values[2],values[3],String(bigints[0]),String(bigints[1]),String(bigints[2])].join('|');"
        ),
        "1|fill|true|1|2|2|4|1|-2|-2"
    );
    assert_eq!(
        rendered(
            "var log=[],value={valueOf(){log.push('value');return 9}},\
             start={valueOf(){log.push('start');return 1}},end={valueOf(){log.push('end');return 2}};\
             new Uint8Array(3).fill(value,start,end);return log.join('|');"
        ),
        "value|start|end"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);\
             values.fill({valueOf(){buffer.resize(2);return 7}},1,4);\
             return [values.length,values[0],values[1]].join('|');"
        ),
        "2|0|7"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);\
             values.fill(7,{valueOf(){buffer.resize(1);return 0}});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new BigInt64Array(1).fill(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_copy_within_preserves_raw_overlap_and_conditionally_revalidates() {
    assert_eq!(
        rendered(
            "var values=new Uint8Array([1,2,3,4,5]),bigints=new BigInt64Array([1n,2n,3n]);\
             var returned=values.copyWithin(1,0,4);bigints.copyWithin(1,0,2);\
             return [Uint8Array.prototype.copyWithin.length,Uint8Array.prototype.copyWithin.name,returned===values,\
               values[0],values[1],values[2],values[3],values[4],\
               String(bigints[0]),String(bigints[1]),String(bigints[2])].join('|');"
        ),
        "2|copyWithin|true|1|1|2|3|4|1|1|2"
    );
    assert_eq!(
        rendered(
            "var log=[],target={valueOf(){log.push('target');return 1}},\
             start={valueOf(){log.push('start');return 0}},end={valueOf(){log.push('end');return 2}};\
             new Uint8Array(3).copyWithin(target,start,end);return log.join('|');"
        ),
        "target|start|end"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);\
             values[0]=3;values[1]=4;values.copyWithin({valueOf(){buffer.resize(3);return 1}},0,4);\
             return [values.length,values[0],values[1],values[2]].join('|');"
        ),
        "3|3|3|4"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);\
             values.copyWithin(0,{valueOf(){buffer.resize(1);return 0}});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);\
             return String(values.copyWithin(2,{valueOf(){buffer.resize(1);return 0}})===values);"
        ),
        "true"
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

#[test]
fn typed_array_slice_uses_species_and_copies_without_aliasing_by_default() {
    assert_eq!(
        rendered(
            "var source=new Uint16Array([1,258,3,4]),out=source.slice(1,-1),bigints=new BigInt64Array([1n,2n]).slice(1);\
             source[1]=9;return [Uint8Array.prototype.slice.length,Uint8Array.prototype.slice.name,\
               out.length,out[0],out[1],out.buffer===source.buffer,String(bigints[0])].join('|');"
        ),
        "2|slice|2|258|3|false|2"
    );
    assert_eq!(
        rendered(
            "var source=new Uint8Array([257,2,3]);source.constructor={[Symbol.species]:Uint16Array};\
             var out=source.slice(0,2);return [out.constructor===Uint16Array,out[0],out[1]].join('|');"
        ),
        "true|1|2"
    );
    assert_eq!(
        thrown(
            "var source=new Uint8Array(2);source.constructor={[Symbol.species]:BigInt64Array};source.slice();"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "var source=new Uint8Array([1,2]);source.constructor={[Symbol.species]:function C(){return new Uint8Array(0)}};source.slice();"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_slice_revalidates_after_species_and_observes_forward_overlap() {
    assert_eq!(
        rendered(
            "var source=new Uint8Array([1,2,3,4]);\
             source.constructor={[Symbol.species]:function C(n){return new Uint8Array(source.buffer,1,n)}};\
             var out=source.slice(0,3);return [out[0],out[1],out[2],source[0],source[1],source[2],source[3]].join('|');"
        ),
        "1|1|1|1|1|1|1"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),source=new Uint8Array(buffer);source[0]=7;\
             source.constructor={[Symbol.species]:function C(n){buffer.resize(1);return new Uint8Array(n)}};\
             var out=source.slice(0,4);return [out.length,out[0],out[1],out[2],out[3]].join('|');"
        ),
        "4|7|0|0|0"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),source=new Uint8Array(buffer,2,2);\
             source.constructor={[Symbol.species]:function C(n){buffer.resize(1);return new Uint8Array(n)}};source.slice();"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),source=new Uint8Array(buffer,2,2);\
             source.constructor={[Symbol.species]:function C(n){buffer.resize(1);return new Uint8Array(n)}};\
             return String(source.slice(0,0).length);"
        ),
        "0"
    );
}

#[test]
fn typed_array_iterators_share_the_values_function_and_observe_live_views() {
    assert_eq!(
        rendered(
            "var values=new Uint8Array([2,3]),valueIterator=values.values(),keyIterator=values.keys(),\
             entry=values.entries().next();\
             return [Uint8Array.prototype.entries.length,Uint8Array.prototype.entries.name,\
               Uint8Array.prototype.keys.length,Uint8Array.prototype.keys.name,\
               Uint8Array.prototype.values.length,Uint8Array.prototype.values.name,\
               values.values===values[Symbol.iterator],valueIterator.next().value,keyIterator.next().value,\
               entry.value[0],entry.value[1],values[Symbol.iterator]().next().value].join('|');"
        ),
        "0|entries|0|keys|0|values|true|2|0|0|2|2"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer);values[0]=7;\
             var iterator=values.values();buffer.resize(1);var first=iterator.next();buffer.resize(0);\
             var done=iterator.next();return [first.value,first.done,done.value,done.done].join('|');"
        ),
        "7|false||true"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.entries();"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_join_captures_length_before_separator_conversion() {
    assert_eq!(
        rendered(
            "var values=new Uint8Array([1,2,3]),bigints=new BigInt64Array([1n,-2n]);\
             return [Uint8Array.prototype.join.length,Uint8Array.prototype.join.name,\
               values.join(),values.join('-'),bigints.join(':')].join('|');"
        ),
        "1|join|1,2,3|1-2-3|1:-2"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer),log=[];\
             values[0]=7;values[1]=8;var text=values.join({toString(){log.push(values.length);buffer.resize(1);return '-'}});\
             return text+'|'+log.join('|');"
        ),
        "7---|4"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.join();"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_to_reversed_creates_an_independent_same_type_copy() {
    assert_eq!(
        rendered(
            "var source=new Int16Array([1,-2,3]),out=source.toReversed(),bigints=new BigInt64Array([1n,-2n]).toReversed();\
             source[0]=9;return [Int16Array.prototype.toReversed.length,Int16Array.prototype.toReversed.name,\
               out.constructor===Int16Array,out.buffer===source.buffer,out.length,out[0],out[1],out[2],\
               String(bigints[0]),String(bigints[1])].join('|');"
        ),
        "0|toReversed|true|false|3|3|-2|1|-2|1"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.toReversed();"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_with_converts_index_then_value_before_its_final_witness() {
    assert_eq!(
        rendered(
            "var log=[],source=new Uint8Array([1,2,3]),out=source.with(\
               {valueOf(){log.push('index');return -1}},\
               {valueOf(){log.push('value');return 258}}),\
             bigints=new BigInt64Array([1n,2n]).with(0,-2n);\
             return [Uint8Array.prototype.with.length,Uint8Array.prototype.with.name,\
               out.constructor===Uint8Array,out.buffer===source.buffer,out[0],out[1],out[2],\
               String(bigints[0]),String(bigints[1]),log.join('|')].join('|');"
        ),
        "2|with|true|false|1|2|2|-2|2|index|value"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),source=new Uint8Array(buffer);\
             source[1]=9;var out=source.with({valueOf(){buffer.resize(1);return 0}},\
               {valueOf(){return 7}});return [out.length,out[0],out[1],out[2],out[3]].join('|');"
        ),
        "4|7|0|0|0"
    );
    assert_eq!(
        rendered(
            "var log=[];try{new Uint8Array(1).with(1,{valueOf(){log.push('value');return 0}})}\
             catch(error){return error.name+'|'+log.join('|');}"
        ),
        "RangeError|value"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.with(0,1);"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_callbacks_capture_the_initial_view_and_visit_later_missing_indices() {
    assert_eq!(
        rendered(
            "var values=new Uint8Array([1,2,3]),seen=[],context={tag:'e'};\
             var every=values.every(function(value,index,array){seen.push(this.tag+value+index+(array===values));return value<4},context);\
             var some=values.some(function(value){return value===2});\
             var each=values.forEach(function(value,index){seen.push('f'+value+index)});\
             var found=values.find(function(value){return value>1}),foundIndex=values.findIndex(function(value){return value>1});\
             var last=values.findLast(function(value){return value>1}),lastIndex=values.findLastIndex(function(value){return value>1});\
             return [Uint8Array.prototype.every.length,Uint8Array.prototype.every.name,\
               Uint8Array.prototype.some.length,Uint8Array.prototype.some.name,\
               Uint8Array.prototype.forEach.length,Uint8Array.prototype.forEach.name,\
               Uint8Array.prototype.find.length,Uint8Array.prototype.find.name,\
               Uint8Array.prototype.findIndex.length,Uint8Array.prototype.findIndex.name,\
               Uint8Array.prototype.findLast.length,Uint8Array.prototype.findLast.name,\
               Uint8Array.prototype.findLastIndex.length,Uint8Array.prototype.findLastIndex.name,\
               every,some,each===undefined,found,foundIndex,last,lastIndex,seen.join(',')].join('|');"
        ),
        "1|every|1|some|1|forEach|1|find|1|findIndex|1|findLast|1|findLastIndex|true|true|true|2|1|3|2|e10true,e21true,e32true,f10,f21,f32"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer),seen=[];\
             values[0]=7;values[1]=8;\
             var every=values.every(function(value,index){seen.push(value===undefined?'u'+index:String(value));if(index===0)buffer.resize(1);return true});\
             var reverseBuffer=new ArrayBuffer(4,{maxByteLength:4}),reverse=new Uint8Array(reverseBuffer),back=[];\
             reverse[0]=9;reverse[3]=4;\
             var last=reverse.findLast(function(value,index){back.push(value===undefined?'u'+index:String(value));if(index===3)reverseBuffer.resize(1);return index===0});\
             return [every,seen.join(','),last,back.join(',')].join('|');"
        ),
        "true|7,u1,u2,u3|9|4,u2,u1,9"
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.find(function(){return true});"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_map_constructs_species_before_callbacks_and_uses_fresh_writes() {
    assert_eq!(
        rendered(
            "var source=new Uint8Array([1,2,3]),log=[];source.constructor={[Symbol.species]:Uint16Array};\
             var out=source.map(function(value,index,array){log.push(value+':'+index+':'+(array===source));return value*257});\
             return [Uint8Array.prototype.map.length,Uint8Array.prototype.map.name,\
               out.constructor===Uint16Array,out.length,out[0],out[1],out[2],log.join(',')].join('|');"
        ),
        "1|map|true|3|257|514|771|1:0:true,2:1:true,3:2:true"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),source=new Uint8Array(buffer),order=[];\
             source[0]=7;source[1]=8;source.constructor={[Symbol.species]:function C(length){order.push('species'+length);return new Uint8Array(length)}};\
             var out=source.map(function(value,index){order.push(value===undefined?'u'+index:String(value));if(index===0)buffer.resize(1);return index+1});\
             return [out.length,out[0],out[1],out[2],out[3],order.join(',')].join('|');"
        ),
        "4|1|2|3|4|species4,7,u1,u2,u3"
    );
    assert_eq!(
        rendered(
            "var source=new Uint8Array([1,2,3,4]),buffer=new ArrayBuffer(4,{maxByteLength:4}),target;\
             source.constructor={[Symbol.species]:function C(length){target=new Uint8Array(buffer);return target}};\
             var out=source.map(function(value,index){return {valueOf(){if(index===0)buffer.resize(1);return value+1}}});\
             return [out===target,out.length,out[0]].join('|');"
        ),
        "true|1|2"
    );
    assert_eq!(
        thrown(
            "var source=new Uint8Array(2);source.constructor={[Symbol.species]:BigInt64Array};source.map(function(value){return value});"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_reductions_capture_the_initial_view_and_refresh_each_element_read() {
    assert_eq!(
        rendered(
            "var values=new Uint8Array([1,2,3]),calls=[],rightCalls=[],callbackThis='not-set';\
             var left=values.reduce(function(accumulator,value,index,array){calls.push(accumulator+':'+value+':'+index+':'+(array===values));return accumulator+value});\
             var right=values.reduceRight(function(accumulator,value,index,array){'use strict';callbackThis=this;rightCalls.push(index+':'+(array===values));return accumulator-value});\
             var explicit=values.reduce(function(accumulator,value){return String(accumulator)+value},undefined);\
             var empty=new Uint8Array(0).reduce(function(){throw new Error('unexpected')},'seed');\
             return [Uint8Array.prototype.reduce.length,Uint8Array.prototype.reduce.name,\
               Uint8Array.prototype.reduceRight.length,Uint8Array.prototype.reduceRight.name,\
               left,right,explicit,empty,callbackThis===undefined,calls.join(','),rightCalls.join(',')].join('|');"
        ),
        "1|reduce|1|reduceRight|6|0|undefined123|seed|true|1:2:1:true,3:3:2:true|1:true,0:true"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer),seen=[];\
             values[0]=7;values[1]=8;\
             var left=values.reduce(function(accumulator,value,index){seen.push(value===undefined?'u'+index:String(value));if(index===0)buffer.resize(1);return accumulator+String(value)},'');\
             var reverseBuffer=new ArrayBuffer(4,{maxByteLength:4}),reverse=new Uint8Array(reverseBuffer),back=[];\
             reverse[0]=9;reverse[3]=4;\
             var right=reverse.reduceRight(function(accumulator,value,index){back.push(value===undefined?'u'+index:String(value));if(index===3)reverseBuffer.resize(1);return accumulator+String(value)},'');\
             return [left,seen.join(','),right,back.join(',')].join('|');"
        ),
        "7undefinedundefinedundefined|7,u1,u2,u3|4undefinedundefined9|4,u2,u1,9"
    );
    assert_eq!(
        thrown("return new Uint8Array(0).reduce(function(){});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Uint8Array(0).reduceRight(function(){});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.reduce(function(){});"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn typed_array_filter_collects_before_species_and_keeps_fresh_resizable_reads() {
    assert_eq!(
        rendered(
            "var source=new Uint8Array([1,2,3,4]),calls=[],context={tag:'x'};\
             source.constructor={[Symbol.species]:Uint16Array};\
             var out=source.filter(function(value,index,array){calls.push(this.tag+value+index+(array===source));return value%2===0},context);\
             return [Uint8Array.prototype.filter.length,Uint8Array.prototype.filter.name,\
               out.constructor===Uint16Array,out.length,out[0],out[1],calls.join(',')].join('|');"
        ),
        "1|filter|true|2|2|4|x10true,x21true,x32true,x43true"
    );
    assert_eq!(
        rendered(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),source=new Uint8Array(buffer),order=[];\
             source[0]=7;source[1]=8;\
             source.constructor={[Symbol.species]:function C(length){order.push('species'+length);return new Uint8Array(length)}};\
             var out=source.filter(function(value,index){order.push(value===undefined?'u'+index:String(value));if(index===0)buffer.resize(1);return true});\
             return [out.length,out[0],out[1],out[2],out[3],order.join(',')].join('|');"
        ),
        "4|7|0|0|0|7,u1,u2,u3,species4"
    );
    assert_eq!(
        thrown(
            "var source=new Uint8Array(2);source.constructor={[Symbol.species]:BigInt64Array};source.filter(function(){return true});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "var buffer=new ArrayBuffer(4,{maxByteLength:4}),values=new Uint8Array(buffer,2,2);buffer.resize(1);values.filter(function(){return true});"
        ),
        ExceptionKind::TypeError
    );
}
