//! ES2025 synchronous arrow-function semantics.

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionLimits, Function, JsNumber, JsString,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime arrow>"))
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

#[test]
fn synchronous_arrows_execute_concise_and_block_bodies() {
    assert_eq!(
        rendered(
            "var add=(left,right)=>left+right;
             var twice=value=>{return value*2;};
             return [add(3,4),twice(5)].join('|');"
        ),
        "7|10"
    );
}

#[test]
fn arrows_capture_lexical_this_arguments_and_new_target() {
    assert_eq!(
        rendered(
            "function Outer(value){
               this.value=value;
               this.arrow=()=>[this.value,arguments[0],new.target===Outer].join('|');
             }
             var instance=new Outer(7);
             return instance.arrow.call({value:9},11);"
        ),
        "7|7|true"
    );
}

#[test]
fn nested_arrows_forward_the_nearest_non_arrow_environment() {
    assert_eq!(
        rendered(
            "function outer(value){return ()=>()=>this.tag+'|'+arguments[0];}
             return outer.call({tag:'lexical'},13)()();"
        ),
        "lexical|13"
    );
}

#[test]
fn arrows_infer_names_and_are_not_constructors() {
    assert_eq!(
        rendered(
            "var inferred=()=>0;
             var threw=false;
             try{new inferred();}catch(error){threw=error instanceof TypeError;}
             return [inferred.name,threw,
               Object.prototype.hasOwnProperty.call(inferred,'prototype')].join('|');"
        ),
        "inferred|true|false"
    );
}

#[test]
fn rooted_arrow_keeps_its_lexical_receiver_live_through_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let arrow = {
        let mut context = runtime.context(&realm).expect("context");
        let run = dynamic_function(
            &mut context,
            "function make(){return ()=>this.value;}
             return make.call({value:29});",
        );
        context
            .call(&run, &[], ExecutionLimits::default())
            .expect("arrow completion")
            .into_function()
            .expect("arrow function")
    };

    runtime
        .collect_cycles()
        .expect("rooted arrow and lexical receiver survive");

    let mut context = runtime.context(&realm).expect("context");
    let result = context
        .call(&arrow, &[], ExecutionLimits::default())
        .expect("arrow remains callable");
    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(29))
    );
}

#[test]
fn rooted_arrow_keeps_its_lexical_new_target_live_through_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let arrow = {
        let mut context = runtime.context(&realm).expect("context");
        let run = dynamic_function(
            &mut context,
            "return new (function(){return ()=>new.target;})();",
        );
        context
            .call(&run, &[], ExecutionLimits::default())
            .expect("arrow completion")
            .into_function()
            .expect("arrow function")
    };

    runtime
        .collect_cycles()
        .expect("rooted arrow and lexical new.target survive");

    let mut context = runtime.context(&realm).expect("context");
    let _target = context
        .call(&arrow, &[], ExecutionLimits::default())
        .expect("arrow remains callable")
        .into_function()
        .expect("captured new.target remains a function");
}
