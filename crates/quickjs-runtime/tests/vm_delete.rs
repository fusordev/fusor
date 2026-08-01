//! `delete` operator semantics, pinned to the `QuickJS` 2026-06-04 oracle.
//!
//! Every expectation in this file was produced by running the equivalent
//! source through the pinned `qjs` binary. The oracle transcript is recorded
//! per test so a future change cannot silently redefine the behavior.

use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsException, JsNumber, JsValue, Runtime,
    RuntimeLimits, ValueKind,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-delete.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

/// Runs `function run(){...}` and projects its result while the owning
/// runtime is still alive, since a `JsValue` is only readable through the
/// runtime that produced it.
fn run_with<T>(source: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("run");
    let result = context.call(&function, &[], ExecutionLimits::default());
    project(result)
}

fn boolean(source: &str) -> bool {
    run_with(source, |result| {
        result
            .expect("completed")
            .as_boolean()
            .expect("live value")
            .expect("boolean")
    })
}

/// Asserts the body evaluates to the exact Number `expected`.
fn assert_number(source: &str, expected: i32) {
    run_with(source, |result| {
        let actual = result
            .expect("completed")
            .as_number()
            .expect("live value")
            .expect("number");
        assert!(
            actual.strict_equals(JsNumber::from_i32(expected)),
            "{source} produced {actual:?}, expected {expected}"
        );
    });
}

fn text(source: &str) -> String {
    run_with(source, |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("utf-8")
    })
}

fn kind(source: &str) -> ValueKind {
    run_with(source, |result| {
        result.expect("completed").kind().expect("live value")
    })
}

fn thrown(source: &str) -> JsException {
    run_with(source, |result| match result {
        Err(ExecutionError::Exception(exception)) => exception,
        Err(error) => panic!("expected a JavaScript throw, found {error:?}"),
        Ok(value) => panic!("expected a JavaScript throw, returned {value:?}"),
    })
}

fn assert_type_error(exception: &JsException, expected: &str) {
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("message"),
        expected
    );
}

/// Oracle: `delete missing sloppy => true`.
#[test]
fn deleting_an_absent_property_reports_success() {
    assert!(boolean("function run(){let o={};return delete o.x;}"));
}

/// Oracle: `delete configurable => true`.
#[test]
fn deleting_a_configurable_property_removes_it() {
    assert!(boolean("function run(){let o={x:1};return delete o.x;}"));
    assert_eq!(
        kind("function run(){let o={x:1};delete o.x;return o.x;}"),
        ValueKind::Undefined
    );
}

/// Oracle: `delete on number => true`. A primitive base is boxed by
/// `ToObject`, and the discarded wrapper has no own property.
#[test]
fn deleting_from_a_primitive_base_reports_success() {
    assert!(boolean("function run(){let n=1;return delete n.x;}"));
    assert!(boolean("function run(){let b=true;return delete b.x;}"));
}

/// Oracle: `delete on string index => false` and
/// `delete on string index oob => true`. A `String` wrapper's own index
/// properties are non-configurable.
#[test]
fn deleting_a_string_index_refuses_only_within_range() {
    assert!(!boolean("function run(){let s=\"ab\";return delete s[0];}"));
    assert!(!boolean("function run(){let s=\"ab\";return delete s[1];}"));
    assert!(boolean("function run(){let s=\"ab\";return delete s[5];}"));
}

/// Oracle: `delete on null !! TypeError: cannot convert to object`. The base
/// is coerced with `ToObject` before the key is consulted, so the message is
/// the conversion failure rather than a property-read failure.
#[test]
fn deleting_from_a_nullish_base_throws_the_conversion_type_error() {
    assert_type_error(
        &thrown("function run(){let o=null;return delete o.x;}"),
        "cannot convert to object",
    );
    assert_type_error(
        &thrown("function run(){let o;return delete o.x;}"),
        "cannot convert to object",
    );
}

/// Oracle: `delete array element keeps length => true len=3 has1=false`.
/// `[[Delete]]` creates a hole; it never shortens the array.
#[test]
fn deleting_an_array_element_keeps_the_length() {
    assert!(boolean("function run(){let a=[1,2,3];return delete a[1];}"));
    assert_number(
        "function run(){let a=[1,2,3];delete a[1];return a.length;}",
        3,
    );
    assert_eq!(
        kind("function run(){let a=[1,2,3];delete a[1];return a[1];}"),
        ValueKind::Undefined
    );
    // The surviving elements keep their own indices.
    assert_number("function run(){let a=[1,2,3];delete a[1];return a[2];}", 3);
}

/// Oracle: `delete array length => false`. An array's `length` is
/// non-configurable.
#[test]
fn deleting_an_array_length_is_refused() {
    assert!(!boolean(
        "function run(){let a=[1];return delete a.length;}"
    ));
    assert_number(
        "function run(){let a=[1];delete a.length;return a.length;}",
        1,
    );
}

/// Oracle: `delete computed key order => true log=base,key`. The base is
/// evaluated before the computed key.
#[test]
fn a_computed_delete_evaluates_its_base_before_its_key() {
    assert_eq!(
        text(
            "function run(){\
                let log=\"\";\
                let o={x:1};\
                let base={get b(){log+=\"base,\";return o;}};\
                delete base.b[(log+=\"key\",\"x\")];\
                return log;\
            }"
        ),
        "base,key"
    );
}

/// Oracle: `delete returns true for non-reference => true`. The operand is
/// still evaluated for its effects.
#[test]
fn deleting_a_non_reference_evaluates_the_operand_and_reports_success() {
    assert!(boolean("function run(){return delete (1+1);}"));
    assert_number(
        "function run(){\
                let seen=0;\
                let o={m(){seen=7;return 1;}};\
                delete o.m();\
                return seen;\
            }",
        7,
    );
}

/// Oracle: `delete then re-add ordering => b,c,a`. Deleting compacts the
/// property order, so a re-added key goes to the end.
#[test]
fn deleting_then_re_adding_moves_the_key_to_the_end() {
    assert_eq!(
        text(
            "function run(){\
                let o={a:1,b:2,c:3};\
                delete o.a;\
                o.a=4;\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "bca"
    );
}

/// A deleted property stops appearing in `for-in`.
#[test]
fn a_deleted_property_leaves_the_enumeration() {
    assert_eq!(
        text(
            "function run(){\
                let o={a:1,b:2,c:3};\
                delete o.b;\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "ac"
    );
}

/// Deleting is observable through a later write, which must create a fresh
/// property rather than reuse the removed slot.
#[test]
fn a_deleted_slot_is_not_reused_by_a_later_write() {
    assert_number(
        "function run(){\
                let o={a:1,b:2};\
                delete o.a;\
                o.c=3;\
                return o.b+o.c;\
            }",
        5,
    );
}
