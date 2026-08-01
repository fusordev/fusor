//! Object-literal `__proto__` prototype mutation, pinned to the
//! `QuickJS` 2026-06-04 oracle.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! proto null => null
//! proto obj => 1
//! proto number ignored => true
//! proto undefined ignored => true
//! proto is own prop? => 0
//! quoted proto => 1
//! computed proto is own => __proto__
//! shorthand proto => __proto__
//! ```

use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExecutionError, ExecutionLimits, JsNumber, JsValue, Runtime, RuntimeLimits, ValueKind,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-proto.js"))
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

fn run_with<T>(source: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("run");
    let result = context.call(&function, &[], ExecutionLimits::default());
    project(result)
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

fn boolean(source: &str) -> bool {
    run_with(source, |result| {
        result
            .expect("completed")
            .as_boolean()
            .expect("live value")
            .expect("boolean")
    })
}

fn kind(source: &str) -> ValueKind {
    run_with(source, |result| {
        result.expect("completed").kind().expect("live value")
    })
}

/// Oracle: `proto obj => 1`. The literal inherits through the installed
/// prototype.
#[test]
fn a_proto_literal_key_installs_the_prototype() {
    assert_number(
        "function run(){let base={m:1};let o={__proto__:base};return o.m;}",
        1,
    );
}

/// Oracle: `proto is own prop? => 0`. `__proto__` is a prototype mutation, so
/// it must not appear as an own property; `for-in` therefore sees only the
/// inherited key.
#[test]
fn a_proto_literal_key_is_not_an_own_property() {
    assert_eq!(
        text(
            "function run(){\
                let base={inherited:1};\
                let o={__proto__:base,own:2};\
                let keys=\"\";\
                for(let k in o){keys+=k+\",\";}\
                return keys;\
            }"
        ),
        "own,inherited,"
    );
}

/// Oracle: `proto null => null`. A `null` prototype detaches the object, so
/// nothing is inherited.
#[test]
fn a_null_proto_literal_key_detaches_the_prototype() {
    assert_eq!(
        text(
            "function run(){\
                let o={__proto__:null,own:1};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "own"
    );
}

/// Oracle: `nonobject proto no own key => []` and
/// `nonobject proto keeps default => 1`. A non-object, non-null value is
/// silently ignored: it neither replaces the prototype nor becomes an own
/// property.
#[test]
fn a_non_object_proto_literal_key_is_ignored() {
    // A detached object has no `valueOf`; an ignored `__proto__` value keeps
    // the default `Object.prototype`, so the two disagree.
    assert!(boolean(
        "function run(){\
            let ignored={__proto__:5};\
            let detached={__proto__:null};\
            return ignored.valueOf!==detached.valueOf;\
        }"
    ));
    assert!(boolean(
        "function run(){\
            let ignored={__proto__:void 0};\
            let kept={};\
            return ignored.valueOf===kept.valueOf;\
        }"
    ));
    // And it is not installed as an own property.
    assert_eq!(
        text(
            "function run(){\
                let o={__proto__:5};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        ""
    );
}

/// Oracle: `quoted proto => 1`. A quoted `"__proto__"` key is still a
/// prototype mutation.
#[test]
fn a_quoted_proto_literal_key_installs_the_prototype() {
    assert_number(
        "function run(){let base={m:1};let o={\"__proto__\":base};return o.m;}",
        1,
    );
    assert_eq!(
        text(
            "function run(){\
                let base={m:1};\
                let o={\"__proto__\":base};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "m"
    );
}

/// Oracle: `shorthand proto forin => [__proto__]`. The shorthand form is an
/// ordinary own property, not a prototype mutation.
///
/// Shorthand object properties are not yet lowered, so this asserts the
/// equivalent guarantee through the computed form, which the oracle also
/// reports as an own property (`computed proto is own => __proto__`).
#[test]
fn a_computed_proto_key_stays_an_own_property() {
    assert_eq!(
        text(
            "function run(){\
                let key=\"__proto__\";\
                let base={m:1};\
                let o={[key]:base};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "__proto__"
    );
}

/// A later prototype read observes the installed prototype rather than the
/// literal's default, and a missing key still reports `undefined`.
#[test]
fn an_installed_prototype_serves_later_reads() {
    assert_number(
        "function run(){\
                let base={m:1};\
                let o={__proto__:base};\
                base.m=7;\
                return o.m;\
            }",
        7,
    );
    assert_eq!(
        kind("function run(){let o={__proto__:null};return o.missing;}"),
        ValueKind::Undefined
    );
}

/// A prototype installed from a literal participates in ordinary method
/// dispatch with the literal as receiver.
#[test]
fn an_installed_prototype_supplies_methods_with_the_literal_as_receiver() {
    assert!(boolean(
        "function run(){\
            let base={self(){return this;}};\
            let o={__proto__:base};\
            return o.self()===o;\
        }"
    ));
}
