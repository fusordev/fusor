//! Core `%ArrayBuffer%` construction, resizability, copying, and detachment.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::{Duration, Instant},
};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits, RuntimeResource,
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

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    project(context.call(&run, &[], ExecutionLimits::default()))
}

fn rendered(body: &str) -> String {
    evaluate(body, |result| value_string(result.expect("completed")))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the test helper consumes and releases its public runtime root after rendering"
)]
fn value_string(value: JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
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
fn array_buffer_transfer_optional_parameters_have_zero_function_length() {
    assert_eq!(
        rendered(
            "return [ArrayBuffer.prototype.transfer.length,\
             ArrayBuffer.prototype.transferToFixedLength.length,\
             ArrayBuffer.prototype.transferToImmutable.length].join('|');"
        ),
        "0|0|0"
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
fn immutable_array_buffers_are_fixed_read_only_copies() {
    assert_eq!(
        rendered(
            "var source=new ArrayBuffer(4,{maxByteLength:8}),view=new Uint8Array(source);\
             view[0]=1;view[1]=2;view[2]=3;view[3]=4;\
             var immutable=source.transferToImmutable(6),slice=immutable.sliceToImmutable(1,4);\
             return [source.detached,immutable.immutable,immutable.resizable,\
               immutable.byteLength,immutable.maxByteLength,new Uint8Array(immutable).join(','),\
               slice.immutable,slice.byteLength,new Uint8Array(slice).join(',')].join('|');"
        ),
        "true|true|false|6|6|1,2,3,4,0,0|true|3|2,3,4"
    );
}

#[test]
fn immutable_array_buffer_writes_reject_before_argument_coercion() {
    assert_eq!(
        rendered(
            "var immutable=(new ArrayBuffer(8)).transferToImmutable(),log=[];\
             var index={valueOf:function(){log.push('index');return 0}},\
             value={valueOf:function(){log.push('value');return 1}},\
             source={get length(){log.push('source.length');return 1}},\
             offset={valueOf:function(){log.push('offset');return 0}};\
             try{new DataView(immutable).setUint8(index,value)}catch(e){log.push(e.name)}\
             try{new Uint8Array(immutable).set(source,offset)}catch(e){log.push(e.name)}\
             try{Atomics.store(new Int32Array(immutable),index,value)}catch(e){log.push(e.name)}\
             var notified=Atomics.notify(new Int32Array(immutable),0);\
             try{immutable.resize(index)}catch(e){log.push(e.name)}\
             try{immutable.transfer(index)}catch(e){log.push(e.name)}\
             return [notified,log.join(',')].join('|');"
        ),
        "0|TypeError,TypeError,TypeError,TypeError,index,TypeError"
    );
}

#[test]
fn immutable_typed_array_indices_are_read_only_and_freezable() {
    assert_eq!(
        rendered(
            "var view=new Uint8Array((new ArrayBuffer(1)).transferToImmutable()),calls=[],\
             value={valueOf:function(){calls.push('value');return 1}},strictKind='',sameKind='',\
             differentKind='',freezeKind='';\
             var sloppy=(function(target,next){target[0]=next;return target[0]})(view,value);\
             try{(function(target,next){'use strict';target[0]=next})(view,value)}catch(e){strictKind=e.name}\
             var reflected=Reflect.set(view,'0',value);\
             var same=false;\
             try{same=Object.defineProperty(view,'0',{value:0})===view}catch(e){sameKind=e.name}\
             try{Object.defineProperty(view,'0',{value:1})}catch(e){differentKind=e.name}\
             var descriptor=Object.getOwnPropertyDescriptor(view,'0')||\
               {value:'missing',writable:'missing',enumerable:'missing',configurable:'missing'};\
             try{Object.freeze(view)}catch(e){freezeKind=e.name}\
             var frozen;try{frozen=Object.isFrozen(view)}catch(e){frozen='error:'+e.name}\
             return [sloppy,strictKind,reflected,same,sameKind,differentKind,freezeKind,calls.join(','),\
               descriptor.value,descriptor.writable,descriptor.enumerable,descriptor.configurable,\
               frozen].join('|');"
        ),
        "0|TypeError|false|true||TypeError|||0|false|true|false|true"
    );
}

#[test]
fn slice_to_immutable_uses_original_bounds_and_rechecks_source() {
    assert_eq!(
        rendered(
            "var source=new ArrayBuffer(10,{maxByteLength:12}),view=new Uint8Array(source);\
             for(var i=0;i<10;i++)view[i]=i+1;\
             var start={valueOf:function(){source.resize(11);return -7}},\
             end={valueOf:function(){source.resize(12);return -4}},\
             result=source.sliceToImmutable(start,end),negativeZero=source.sliceToImmutable(0,-0.9),\
             detached=new ArrayBuffer(2),kind='';\
             try{detached.sliceToImmutable(0,{valueOf:function(){detached.transfer();return 1}})}\
             catch(e){kind=e.name}\
             return [new Uint8Array(result).join(','),negativeZero.byteLength,source.byteLength,kind].join('|');"
        ),
        "4,5,6|0|12|TypeError"
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

#[test]
fn atomics_operate_on_shared_integer_views_in_specification_order() {
    assert_eq!(
        rendered(
            "return [typeof Atomics,Atomics.add.length,Atomics.compareExchange.length,\
             Atomics.isLockFree.length,Atomics.pause.length,Object.prototype.toString.call(Atomics)].join('|');"
        ),
        "object|3|4|1|0|[object Atomics]"
    );
    assert_eq!(
        rendered(
            "var bytes=new Int8Array(new SharedArrayBuffer(4));bytes[0]=127;\
             var add=Atomics.add(bytes,0,2);var sub=Atomics.sub(bytes,0,1);\
             var exchange=Atomics.exchange(bytes,0,1);\
             var compared=Atomics.compareExchange(bytes,0,1,7);\
             var missed=Atomics.compareExchange(bytes,0,1,9);\
             return [add,bytes[0],sub,exchange,compared,missed].join('|');"
        ),
        "127|7|-127|-128|1|7"
    );
    assert_eq!(
        rendered(
            "var words=new Uint16Array(new SharedArrayBuffer(4)),ints=new Int32Array(new SharedArrayBuffer(4));\
             words[0]=65280;var and=Atomics.and(words,0,255);var or=Atomics.or(words,0,240);\
             var xor=Atomics.xor(words,0,15);var stored=Atomics.store(ints,0,1.9);\
             return [and,words[0],or,xor,words[0],stored,Atomics.load(ints,0),\
             Atomics.isLockFree(1),Atomics.isLockFree(3),Atomics.isLockFree(8)].join('|');"
        ),
        "65280|255|0|240|255|1|1|true|false|true"
    );
    assert_eq!(
        rendered(
            "var bigints=new BigInt64Array(new SharedArrayBuffer(8)),bytes=new Int8Array(new SharedArrayBuffer(4)),log=[];\
             bigints[0]=1n;var add=Atomics.add(bigints,0,2n);\
             var compared=Atomics.compareExchange(bigints,0,3n,9n);\
             var index={valueOf:function(){log.push('index');return 0}},\
             value={valueOf:function(){log.push('value');return 2}},\
             replacement={valueOf:function(){log.push('replacement');return 4}};\
             Atomics.compareExchange(bytes,index,value,replacement);\
             return [String(add),String(bigints[0]),String(compared),log.join(',')].join('|');"
        ),
        "1|9|3|index,value,replacement"
    );
    assert_eq!(
        rendered(
            "var values=new BigUint64Array(new SharedArrayBuffer(8)),\
             value={valueOf:function(){return 33n}};\
             return String(Atomics.store(values,0,value));"
        ),
        "33"
    );
    assert_eq!(
        rendered(
            "var values=new BigUint64Array(new ArrayBuffer(8)),\
             value={valueOf:function(){return 33n}};values[0]=value;return String(values[0]);"
        ),
        "33"
    );
    assert_eq!(
        rendered("return String(BigInt({valueOf:function(){return 33n}}));"),
        "33"
    );
}

#[test]
fn atomics_reject_non_shared_non_integer_and_invalid_indexed_accesses() {
    assert_eq!(
        thrown("return Atomics.add(new Float32Array(new SharedArrayBuffer(4)),0,1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Atomics.add(new Int8Array(new SharedArrayBuffer(1)),1,1);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Atomics.add(new BigInt64Array(new SharedArrayBuffer(8)),0,1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Atomics.add({},0,1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Atomics.wait(new Int32Array(new ArrayBuffer(4)),0,0,0);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Atomics.notify(new Int8Array(new SharedArrayBuffer(1)),0);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn atomics_notify_and_wait_preserve_single_agent_and_nonshared_semantics() {
    assert_eq!(
        rendered(
            "var local=new Int32Array(new ArrayBuffer(4)),shared=new Int32Array(new SharedArrayBuffer(4)),\
             bigints=new BigInt64Array(new SharedArrayBuffer(8));\
             return [Atomics.notify.length,Atomics.wait.length,Atomics.add(local,0,1),Atomics.load(local,0),\
             Atomics.notify(local,0,1),Atomics.notify(shared,0),\
             Atomics.wait(shared,0,1,0),Atomics.wait(shared,0,0,0),\
             Atomics.wait(bigints,0,0n,0)].join('|');"
        ),
        "3|4|0|1|0|0|not-equal|timed-out|timed-out"
    );
    assert_eq!(
        thrown(
            "var values=new Int32Array(new SharedArrayBuffer(4));\
             var poison={valueOf:function(){return Symbol('timeout converted')}};\
             return Atomics.wait(values,0,1,poison);"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn atomics_wait_async_is_fifo_and_preserves_promise_job_order() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setup = dynamic_function(
        &mut context,
        "globalThis.atomicsLog=[];globalThis.timeoutConverted=false;\
         var values=new Int32Array(new SharedArrayBuffer(4));\
         var first=Atomics.waitAsync(values,0,0,Infinity),\
             second=Atomics.waitAsync(values,0,0,Infinity),\
             third=Atomics.waitAsync(values,0,0,Infinity);\
         first.value.then(function(value){atomicsLog.push('first:'+value)});\
         second.value.then(function(value){atomicsLog.push('second:'+value)});\
         third.value.then(function(value){atomicsLog.push('third:'+value)});\
         Atomics.add(values,0,0);\
         var firstCount=Atomics.notify(values,0,2);\
         Promise.resolve().then(function(){atomicsLog.push('later')});\
         var secondCount=Atomics.notify(values,0,1);\
         var bigValues=new BigInt64Array(new SharedArrayBuffer(8)),\
             big=Atomics.waitAsync(bigValues,0,0n,Infinity);\
         big.value.then(function(value){atomicsLog.push('big:'+value)});\
         var bigCount=Atomics.notify(bigValues,0,1);\
         var immediate=Atomics.waitAsync(values,0,1,{valueOf:function(){timeoutConverted=true;return 0}}),\
             zero=Atomics.waitAsync(values,0,0,0),\
             descriptor=Object.getOwnPropertyDescriptor(immediate,'async');\
         return [typeof Atomics.waitAsync,Atomics.waitAsync.name,Atomics.waitAsync.length,\
           first.async,first.value instanceof Promise,firstCount,secondCount,big.async,bigCount,\
           immediate.async,immediate.value,zero.async,zero.value,\
           timeoutConverted,Object.keys(immediate).join(','),\
           [descriptor.writable,descriptor.enumerable,descriptor.configurable].join(',')].join('|');",
    );
    assert_eq!(
        value_string(
            context
                .call(&setup, &[], ExecutionLimits::default())
                .expect("waitAsync setup"),
        ),
        "function|waitAsync|4|true|true|2|1|true|1|false|not-equal|false|timed-out|true|async,value|true,true,true"
    );
    assert_eq!(context.runtime_usage().pending_atomics_waiters(), 0);

    let read = dynamic_function(&mut context, "return globalThis.atomicsLog.join(',');");
    assert_eq!(
        value_string(
            context
                .call(&read, &[], ExecutionLimits::default())
                .expect("waitAsync reactions"),
        ),
        "first:ok,second:ok,later,third:ok,big:ok"
    );
}

#[test]
fn atomics_notify_does_not_settle_unrelated_tokio_timeouts_mid_turn() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setup = dynamic_function(
        &mut context,
        "globalThis.atomicsTurnLog=[];\
         var values=new Int32Array(new SharedArrayBuffer(8)),\
             timeout=Atomics.waitAsync(values,0,0,10),\
             notified=Atomics.waitAsync(values,1,0,Infinity);\
         timeout.value.then(function(){atomicsTurnLog.push('timeout')});\
         notified.value.then(function(){atomicsTurnLog.push('notified')});\
         Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,100);\
         Atomics.notify(values,1,1);\
         Promise.resolve().then(function(){atomicsTurnLog.push('later')});",
    );
    context
        .call(&setup, &[], ExecutionLimits::default())
        .expect("waitAsync turn setup");
    let read = dynamic_function(&mut context, "return globalThis.atomicsTurnLog.join(',');");
    assert_eq!(
        value_string(
            context
                .call(&read, &[], ExecutionLimits::default())
                .expect("waitAsync turn order"),
        ),
        "notified,later,timeout"
    );
}

#[test]
fn atomics_wait_async_uses_tokio_deadlines_and_roots_pending_promises() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let setup = dynamic_function(
            &mut context,
            "globalThis.atomicsTimeout='pending';\
             var values=new Int32Array(new SharedArrayBuffer(4)),\
                 result=Atomics.waitAsync(values,0,0,25);\
             result.value.then(function(value){globalThis.atomicsTimeout=value});\
             return String(result.async);",
        );
        assert_eq!(
            value_string(
                context
                    .call(&setup, &[], ExecutionLimits::default())
                    .expect("waitAsync timeout setup"),
            ),
            "true"
        );
        assert_eq!(context.runtime_usage().pending_atomics_waiters(), 1);
    }

    runtime
        .collect_cycles()
        .expect("pending waitAsync promise collection roots");
    let mut context = runtime.context(&realm).expect("context after collection");
    assert_eq!(context.runtime_usage().pending_atomics_waiters(), 1);
    let read = dynamic_function(&mut context, "return globalThis.atomicsTimeout;");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let observed = value_string(
            context
                .call(&read, &[], ExecutionLimits::default())
                .expect("waitAsync timeout checkpoint"),
        );
        if observed == "timed-out" {
            break;
        }
        assert_eq!(observed, "pending");
        assert!(Instant::now() < deadline, "Tokio deadline did not settle");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(context.runtime_usage().pending_atomics_waiters(), 0);
}

#[test]
fn atomics_wait_async_notification_cancels_its_tokio_deadline() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setup = dynamic_function(
        &mut context,
        "globalThis.atomicsCancellation=[];\
         var values=new Int32Array(new SharedArrayBuffer(4)),\
             result=Atomics.waitAsync(values,0,0,40);\
         result.value.then(function(value){atomicsCancellation.push(value)});\
         return Atomics.notify(values,0,1)+'|'+result.async;",
    );
    assert_eq!(
        value_string(
            context
                .call(&setup, &[], ExecutionLimits::default())
                .expect("waitAsync notification"),
        ),
        "1|true"
    );
    assert_eq!(context.runtime_usage().pending_atomics_waiters(), 0);
    thread::sleep(Duration::from_millis(80));
    let read = dynamic_function(
        &mut context,
        "return globalThis.atomicsCancellation.join(',');",
    );
    assert_eq!(
        value_string(
            context
                .call(&read, &[], ExecutionLimits::default())
                .expect("cancelled waitAsync deadline"),
        ),
        "ok"
    );
    assert_eq!(context.runtime_usage().pending_atomics_waiters(), 0);
}

#[test]
fn atomics_wait_async_obeys_the_pending_waiter_limit() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_pending_atomics_waiters(1))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "var values=new Int32Array(new SharedArrayBuffer(4));\
         Atomics.waitAsync(values,0,0,Infinity);\
         return Atomics.waitAsync(values,0,0,Infinity);",
    );
    assert!(matches!(
        context.call(&run, &[], ExecutionLimits::default()),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::AtomicsWaiters,
            limit: 1,
            observed: 2,
        })
    ));
    assert_eq!(context.runtime_usage().pending_atomics_waiters(), 1);
}

#[test]
fn shared_array_buffer_handles_wake_blocking_waiters_across_runtimes() {
    let handle = {
        let mut seed_runtime = Runtime::try_new(RuntimeLimits::default()).expect("seed runtime");
        let seed_realm = seed_runtime.create_realm().expect("seed realm");
        let mut seed_context = seed_runtime.context(&seed_realm).expect("seed context");
        let create = dynamic_function(&mut seed_context, "return new SharedArrayBuffer(4);");
        let buffer = seed_context
            .call(&create, &[], ExecutionLimits::default())
            .expect("shared buffer");
        let handle = seed_context
            .shared_array_buffer_handle(&buffer)
            .expect("shared buffer handle")
            .expect("SharedArrayBuffer value");
        assert_eq!(handle.byte_length(), 4);
        handle
    };

    let worker_handle = handle.clone();
    let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("worker runtime");
        let realm = runtime.create_realm().expect("worker realm");
        let mut context = runtime.context(&realm).expect("worker context");
        let buffer = context
            .import_shared_array_buffer(&worker_handle)
            .expect("worker shared buffer");
        let wait = dynamic_function(
            &mut context,
            "var values=new Int32Array(arguments[0]),\
                 status=Atomics.wait(values,0,0,2000);\
             if(status==='ok')Atomics.add(values,0,1);\
             return status;",
        );
        ready_sender.send(()).expect("worker ready");
        value_string(
            context
                .call(&wait, &[buffer], ExecutionLimits::default())
                .expect("blocking wait"),
        )
    });

    ready_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker admission");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("notifier runtime");
    let realm = runtime.create_realm().expect("notifier realm");
    let mut context = runtime.context(&realm).expect("notifier context");
    let buffer = context
        .import_shared_array_buffer(&handle)
        .expect("notifier shared buffer");
    let notify = dynamic_function(
        &mut context,
        "return String(Atomics.notify(new Int32Array(arguments[0]),0,1));",
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let notified = value_string(
            context
                .call(
                    &notify,
                    std::slice::from_ref(&buffer),
                    ExecutionLimits::default(),
                )
                .expect("cross-runtime notify"),
        );
        if notified == "1" {
            break;
        }
        assert_eq!(notified, "0");
        assert!(
            Instant::now() < deadline,
            "worker waiter was not registered"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(worker.join().expect("worker thread"), "ok");

    let read = dynamic_function(
        &mut context,
        "return String(Atomics.load(new Int32Array(arguments[0]),0));",
    );
    assert_eq!(
        value_string(
            context
                .call(&read, &[buffer], ExecutionLimits::default())
                .expect("cross-runtime shared bytes"),
        ),
        "1"
    );
}

#[test]
fn shared_array_buffer_handles_wake_async_waiters_across_runtimes() {
    let mut waiting_runtime = Runtime::try_new(RuntimeLimits::default()).expect("waiting runtime");
    let waiting_realm = waiting_runtime.create_realm().expect("waiting realm");
    let handle = {
        let mut waiting_context = waiting_runtime
            .context(&waiting_realm)
            .expect("waiting context");
        let setup = dynamic_function(
            &mut waiting_context,
            "globalThis.crossAgentBuffer=new SharedArrayBuffer(8,{maxByteLength:12});\
             globalThis.crossAgentResult=[];\
             var values=new Int32Array(crossAgentBuffer),\
                 first=Atomics.waitAsync(values,0,0,Infinity),\
                 second=Atomics.waitAsync(values,1,0,Infinity);\
             first.value.then(function(value){\
               crossAgentResult.push('first:'+value);\
               Promise.resolve().then(function(){crossAgentResult.push('first-nested')})\
             });\
             second.value.then(function(value){crossAgentResult.push('second:'+value)});\
             return crossAgentBuffer;",
        );
        let buffer = waiting_context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("cross-agent waitAsync setup");
        let handle = waiting_context
            .shared_array_buffer_handle(&buffer)
            .expect("cross-agent handle")
            .expect("SharedArrayBuffer value");
        assert_eq!(waiting_context.runtime_usage().pending_atomics_waiters(), 2);
        handle
    };

    {
        let mut notifying_runtime =
            Runtime::try_new(RuntimeLimits::default()).expect("notifying runtime");
        let notifying_realm = notifying_runtime.create_realm().expect("notifying realm");
        let mut notifying_context = notifying_runtime
            .context(&notifying_realm)
            .expect("notifying context");
        let buffer = notifying_context
            .import_shared_array_buffer(&handle)
            .expect("import shared buffer");
        let notify = dynamic_function(
            &mut notifying_context,
            "arguments[0].grow(12);\
             var values=new Int32Array(arguments[0]);\
             return arguments[0].byteLength+'|'+Atomics.notify(values,0,1)+'|'+Atomics.notify(values,1,1);",
        );
        assert_eq!(
            value_string(
                notifying_context
                    .call(&notify, &[buffer], ExecutionLimits::default())
                    .expect("cross-agent async notify"),
            ),
            "12|1|1"
        );
    }

    let mut waiting_context = waiting_runtime
        .context(&waiting_realm)
        .expect("waiting context after notify");
    let read = dynamic_function(
        &mut waiting_context,
        "return globalThis.crossAgentResult.join(',')+'|'+globalThis.crossAgentBuffer.byteLength;",
    );
    assert_eq!(
        value_string(
            waiting_context
                .call(&read, &[], ExecutionLimits::default())
                .expect("cross-agent async result"),
        ),
        "first:ok,first-nested,second:ok|12"
    );
    assert_eq!(waiting_context.runtime_usage().pending_atomics_waiters(), 0);
}

#[test]
fn shared_array_buffer_atomic_rmw_is_serialized_across_runtimes() {
    let mut seed_runtime = Runtime::try_new(RuntimeLimits::default()).expect("seed runtime");
    let seed_realm = seed_runtime.create_realm().expect("seed realm");
    let mut seed_context = seed_runtime.context(&seed_realm).expect("seed context");
    let create = dynamic_function(&mut seed_context, "return new SharedArrayBuffer(4);");
    let buffer = seed_context
        .call(&create, &[], ExecutionLimits::default())
        .expect("shared buffer");
    let handle = seed_context
        .shared_array_buffer_handle(&buffer)
        .expect("shared buffer handle")
        .expect("SharedArrayBuffer value");
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let handle = handle.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("worker runtime");
            let realm = runtime.create_realm().expect("worker realm");
            let mut context = runtime.context(&realm).expect("worker context");
            let buffer = context
                .import_shared_array_buffer(&handle)
                .expect("worker shared buffer");
            let add = dynamic_function(
                &mut context,
                "var values=new Int32Array(arguments[0]);\
                 for(var index=0;index<2000;index++)Atomics.add(values,0,1);",
            );
            barrier.wait();
            context
                .call(&add, &[buffer], ExecutionLimits::default())
                .expect("cross-runtime atomic increments");
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().expect("atomic worker thread");
    }

    let read = dynamic_function(
        &mut seed_context,
        "return String(Atomics.load(new Int32Array(arguments[0]),0));",
    );
    assert_eq!(
        value_string(
            seed_context
                .call(&read, &[buffer], ExecutionLimits::default())
                .expect("final atomic value"),
        ),
        "4000"
    );
}
