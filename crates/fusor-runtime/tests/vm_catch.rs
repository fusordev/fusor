use std::sync::Arc;

use fusor_bytecode::VerificationLimits;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{ExecutionLimits, JsNumber, JsValue, Runtime, RuntimeLimits};

const SOURCE_NAME: &str = "catch-regressions.js";

fn compile(source: &str, root_name: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from(SOURCE_NAME))
                .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, VerificationLimits::default())
                .expect("verified catch function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn assert_number_result(source: &str, root_name: &str, expected: i32) {
    let authority = compile(source, root_name);
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("caught completion");

    assert!(
        result
            .as_number()
            .expect("live result")
            .expect("Number")
            .strict_equals(JsNumber::from_i32(expected))
    );
}

fn assert_string_result(source: &str, root_name: &str, expected: &str) {
    let authority = compile(source, root_name);
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("caught completion");

    assert_eq!(string_value(&result), expected);
}

fn string_value(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live result")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn same_frame_throw_enters_the_nearest_catch_with_the_exact_value() {
    assert_number_result(
        "function run(){try{throw 17;}catch(error){return error;}}",
        "run",
        17,
    );
}

#[test]
fn child_frame_throw_unwinds_into_the_callers_catch() {
    assert_string_result(
        "function run(){\
            function fail(){throw \"child\";}\
            try{fail();return \"wrong\";}catch(error){return error+\"-caught\";}\
        }",
        "run",
        "child-caught",
    );
}

#[test]
fn engine_reference_error_is_materialized_for_catch() {
    assert_string_result(
        "function run(){\
            try{return hidden;let hidden;}\
            catch(error){return error.name+\":\"+error.message;}\
        }",
        "run",
        "ReferenceError:hidden is not initialized",
    );
}

#[test]
fn engine_type_error_is_materialized_for_catch() {
    assert_string_result(
        "function run(){\
            try{let value=0;value();}\
            catch(error){return error.name+\":\"+error.message;}\
        }",
        "run",
        "TypeError:not a function",
    );
}

#[test]
fn materialized_engine_error_has_the_error_object_tag() {
    assert_string_result(
        "function run(){\
            try{let value=0;value();}\
            catch(error){return ({}).toString.call(error);}\
        }",
        "run",
        "[object Error]",
    );
}

#[test]
fn rethrow_uses_the_next_outer_catch() {
    assert_string_result(
        "function run(){\
            try{\
                try{throw \"x\";}\
                catch(inner){throw inner+\"y\";}\
            }catch(outer){return outer+\"z\";}\
        }",
        "run",
        "xyz",
    );
}

#[test]
fn closure_keeps_the_catch_binding_alive_after_the_handler() {
    assert_string_result(
        "function run(){\
            let saved;\
            try{throw \"held\";}\
            catch(error){saved=function capture(){return error;};}\
            return saved();\
        }",
        "run",
        "held",
    );
}

#[test]
fn caught_throw_cleans_the_enclosing_for_in_marker() {
    assert_string_result(
        "function run(){\
            try{for(let key in {a:1}){throw key;}}\
            catch(error){return error;}\
        }",
        "run",
        "a",
    );
}
