use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, Runtime, RuntimeLimits,
};

fn compile(source: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-class.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("run"))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("class bytecode");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn run_with<T>(
    source: &str,
    project: impl FnOnce(Result<quickjs_runtime::JsValue, ExecutionError>) -> T,
) -> T {
    let authority = compile(source);
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("run function");
    project(context.call(&function, &[], ExecutionLimits::default()))
}

#[test]
fn base_class_construction_public_methods_and_accessors_obey_the_class_topology() {
    run_with(
        "function run(){class Box{constructor(value){this.value=value;}get doubled(){return this.value*2;}static answer(){return 7;}}let box=new Box(5);return box.doubled+Box.answer();}",
        |result| {
            let value = result.expect("class execution");
            let number = value.as_number().expect("live value").expect("number");
            assert!(number.strict_equals(JsNumber::from_i32(17)));
        },
    );
}

#[test]
fn a_class_constructor_rejects_direct_calls_but_remains_constructable() {
    run_with(
        "function run(){class Box{constructor(){}}Box();}",
        |result| {
            let error = result.expect_err("class direct call");
            let ExecutionError::Exception(exception) = error else {
                panic!("expected JavaScript exception");
            };
            assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
        },
    );
}
