//! `WeakRef` and `FinalizationRegistry` surface, ordering, and GC semantics.
//!
//! The observable surface is pinned to `QuickJS` 2026-06-04. Validation,
//! kept-alive targets, and cleanup scheduling follow ECMA-262's Managing
//! Memory algorithms.

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionLimits, Function, JsString, Object,
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
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime weak references>"),
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

fn rendered(body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    context
        .call(&run, &[], ExecutionLimits::default())
        .expect("completed")
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn call_state(
    runtime: &mut Runtime,
    realm: &fusor_runtime::Realm,
    run: &Function,
    state: &Object,
) -> String {
    let mut context = runtime.context(realm).expect("context");
    context
        .call(run, &[state.as_value()], ExecutionLimits::default())
        .expect("state call")
        .as_string()
        .expect("live state result")
        .expect("String state result")
        .to_utf8_lossy()
        .expect("UTF-8 state result")
}

#[test]
fn weak_reference_surfaces_match_the_pinned_engine() {
    assert_eq!(
        rendered(
            "return Object.getOwnPropertyNames(WeakRef).join(',')+'|'+\
             Object.getOwnPropertyNames(WeakRef.prototype).join(',')+'|'+\
             WeakRef.length+'|'+Object.prototype.toString.call(new WeakRef({}))+'|'+\
             Object.getOwnPropertyNames(FinalizationRegistry).join(',')+'|'+\
             Object.getOwnPropertyNames(FinalizationRegistry.prototype).join(',')+'|'+\
             FinalizationRegistry.length+'|'+\
             Object.prototype.toString.call(new FinalizationRegistry(function(){}));"
        ),
        "length,name,prototype|deref,constructor|1|[object WeakRef]|length,name,prototype|register,unregister,constructor|1|[object FinalizationRegistry]"
    );
}

#[test]
fn weak_reference_validation_brand_and_new_target_order_are_exact() {
    assert_eq!(
        rendered(
            "var unique=Symbol('unique');var wellKnown=Symbol.iterator;var registered=Symbol.for('registered');\
             var object={};var uniqueRef=new WeakRef(unique);var wellKnownRef=new WeakRef(wellKnown);\
             function Derived(){}var prototype=Object.create(WeakRef.prototype);Derived.prototype=prototype;\
             var derived=Reflect.construct(WeakRef,[object],Derived);var errors=[];\
             try{WeakRef.prototype.deref.call({})}catch(error){errors.push(error.message)}\
             try{new WeakRef(1)}catch(error){errors.push(error.message)}\
             try{new WeakRef(registered)}catch(error){errors.push(error.message)}\
             return [uniqueRef.deref()===unique,wellKnownRef.deref()===wellKnown,\
                     Object.getPrototypeOf(derived)===prototype,derived instanceof Derived,\
                     derived.deref()===object,errors.join(',')].join('|');"
        ),
        "true|true|true|true|true|WeakRef object expected,invalid target,invalid target"
    );
}

#[test]
fn finalization_registry_register_and_unregister_follow_specification_order() {
    assert_eq!(
        rendered(
            "var registry=new FinalizationRegistry(function(){});var token={};var first={};var second={};\
             var result=registry.register(first,'first',token);registry.register(second,'second',token);\
             var removed=registry.unregister(token);var removedAgain=registry.unregister(token);var errors=[];\
             try{FinalizationRegistry.prototype.register.call({},1,1)}catch(error){errors.push(error.message)}\
             try{registry.register(1,2)}catch(error){errors.push(error.message)}\
             try{registry.register(first,first)}catch(error){errors.push(error.message)}\
             try{registry.register(first,1,1)}catch(error){errors.push(error.message)}\
             try{registry.unregister(1)}catch(error){errors.push(error.message)}\
             return [String(result),removed,removedAgain,errors.join(',')].join('|');"
        ),
        "undefined|true|false|FinalizationRegistry object expected,invalid target,held value cannot be the target,invalid unregister token,invalid unregister token"
    );
}

#[test]
fn collection_clears_weak_refs_and_runs_finalizers_only_after_a_host_turn() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (state, observe_before_cleanup, observe_after_cleanup) = {
        let mut context = runtime.context(&realm).expect("setup context");
        let setup = dynamic_function(
            &mut context,
            "var log=[];var selected=function(held){log.push('captured:'+held.tag)};\
             var registry=new FinalizationRegistry(selected);\
             selected=function(held){log.push('replacement:'+held.tag)};\
             var target={};var reference=new WeakRef(target);registry.register(target,{tag:'held'});\
             return {log:log,registry:registry,reference:reference};",
        );
        let state = context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("setup")
            .into_object()
            .expect("state");
        let observe_before_cleanup = dynamic_function(
            &mut context,
            "var state=arguments[0];return [String(state.reference.deref()),state.log.join(',')].join('|');",
        );
        let observe_after_cleanup = dynamic_function(
            &mut context,
            "var state=arguments[0];return state.log.join(',');",
        );
        (state, observe_before_cleanup, observe_after_cleanup)
    };

    runtime.collect_cycles().expect("target collection");
    assert_eq!(
        call_state(&mut runtime, &realm, &observe_before_cleanup, &state,),
        "undefined|"
    );
    assert_eq!(
        call_state(&mut runtime, &realm, &observe_after_cleanup, &state,),
        "captured:held"
    );
}

#[test]
fn finalization_cleanup_preserves_cell_order_and_observes_pre_job_unregister() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (state, unregister, observe) = {
        let mut context = runtime.context(&realm).expect("setup context");
        let setup = dynamic_function(
            &mut context,
            "var log=[];var registry=new FinalizationRegistry(function(held){log.push(held)});\
             var first={};var second={};var cancelled={};var token={};\
             registry.register(first,'first');registry.register(second,'second');\
             registry.register(cancelled,'cancelled',token);\
             return {log:log,registry:registry,token:token};",
        );
        let state = context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("setup")
            .into_object()
            .expect("state");
        let unregister = dynamic_function(
            &mut context,
            "var state=arguments[0];return [state.registry.unregister(state.token),state.log.join(',')].join('|');",
        );
        let observe = dynamic_function(&mut context, "return arguments[0].log.join(',');");
        (state, unregister, observe)
    };

    runtime.collect_cycles().expect("target collection");
    assert_eq!(
        call_state(&mut runtime, &realm, &unregister, &state,),
        "true|"
    );
    assert_eq!(
        call_state(&mut runtime, &realm, &observe, &state),
        "first,second"
    );
}
