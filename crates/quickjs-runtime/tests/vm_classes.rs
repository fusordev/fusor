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

#[test]
fn named_class_members_retain_the_inner_name_after_outer_reassignment() {
    run_with(
        "function run(){class Box{constructor(){}static self(){return Box;}}let original=Box;Box=0;return original.self()===original;}",
        |result| {
            let value = result.expect("class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_base_class_without_a_constructor_still_constructs_with_its_class_prototype() {
    run_with(
        "function run(){class Box{static answer(){return 7;}}let box=new Box(1,2,3);return box.constructor===Box&&Box.answer()===7;}",
        |result| {
            let value = result.expect("default class construction");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_named_base_class_expression_retains_its_inner_name_and_constructs() {
    run_with(
        "function run(){let Result=class Box{static self(){return Box;}};let original=Result;Result=0;return original.self()===original&&new original().constructor===original;}",
        |result| {
            let value = result.expect("named class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_direct_anonymous_base_class_initializer_infers_its_binding_name() {
    run_with(
        "function run(){let Result=class{static answer(){return 7;}};let original=Result;Result=0;return original.name==='Result'&&original.answer()===7&&new original().constructor===original;}",
        |result| {
            let value = result.expect("anonymous class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_parenthesized_anonymous_base_class_initializer_infers_its_binding_name() {
    run_with(
        "function run(){let Result=(class{static answer(){return 7;}});let original=Result;Result=0;return original.name==='Result'&&original.answer()===7&&new original().constructor===original;}",
        |result| {
            let value = result.expect("parenthesized anonymous class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn named_class_member_writes_throw_without_mutating_the_inner_name_cell() {
    run_with(
        "function run(){class Box{static replace(){Box=0;}}try{Box.replace();}catch(error){return error.name==='TypeError'&&Box.name==='Box';}return false;}",
        |result| {
            let value = result.expect("class name assignment completion");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_public_class_methods_observe_instance_and_static_targets() {
    run_with(
        "function run(){let key='instance';class Box{[key](){return 3;}static[key+'Static'](){return 7;}}return new Box()[key]()+Box[key+'Static']();}",
        |result| {
            let value = result.expect("computed class method execution");
            let number = value.as_number().expect("live Number").expect("number");
            assert!(number.strict_equals(JsNumber::from_i32(10)));
        },
    );
}

#[test]
fn computed_public_class_accessors_define_their_respective_targets() {
    run_with(
        "function run(){let key='value';class Box{get[key](){return this._value;}set[key](value){this._value=value;}static get[key+'Static'](){return 4;}}let box=new Box;box[key]=6;return box[key]+Box[key+'Static'];}",
        |result| {
            let value = result.expect("computed class accessor execution");
            let number = value.as_number().expect("live Number").expect("number");
            assert!(number.strict_equals(JsNumber::from_i32(10)));
        },
    );
}
