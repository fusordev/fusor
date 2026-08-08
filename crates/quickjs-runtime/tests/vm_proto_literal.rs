//! Normative object-initializer `ProtoSetter` semantics.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! static proto changes prototype => true
//! static proto is not own => true
//! explicit prototype => 1
//! computed proto is own => __proto__
//! ```

use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{ExecutionError, ExecutionLimits, JsValue, Runtime, RuntimeLimits};

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

/// A static `ProtoSetter` changes the fresh literal's prototype without defining
/// an own `__proto__` property.
#[test]
fn a_proto_literal_key_changes_the_literal_prototype() {
    assert!(boolean(
        "function run(){let base={m:1};let o={__proto__:base};\
         return o.m===1&&o.__proto__===void 0;}"
    ));
}

/// A `ProtoSetter` does not enter the literal's own-key enumeration order.
#[test]
fn a_proto_literal_key_is_not_an_own_property() {
    assert_eq!(
        text(
            "function run(){let base={inherited:1};let o={__proto__:base,own:2};\
             let keys='';for(let key in o){keys+=key+',';}return keys;}"
        ),
        "own,inherited,"
    );
}

/// `null` detaches the fresh literal from `Object.prototype`.
#[test]
fn a_null_proto_literal_key_creates_a_null_prototype() {
    assert_eq!(
        text(
            "function run(){let o={__proto__:null,own:1};let keys='';\
             for(let key in o){keys+=key;}return keys+':'+(o.toString===void 0);}"
        ),
        "own:true"
    );
}

/// Primitive `ProtoSetter` values are evaluated and ignored without defining a
/// data property.
#[test]
fn a_non_object_proto_literal_value_is_ignored() {
    assert!(boolean(
        "function run(){\
            let ignored={__proto__:5};\
            return typeof ignored.toString==='function'&&ignored.__proto__===void 0;\
        }"
    ));
    assert!(boolean(
        "function run(){\
            let literal={__proto__:void 0};\
            return typeof literal.toString==='function'&&literal.__proto__===void 0;\
        }"
    ));
}

/// Quoted static keys have the same `ProtoSetter` semantics.
#[test]
fn a_quoted_proto_literal_key_changes_the_literal_prototype() {
    assert!(boolean(
        "function run(){let base={m:1};let o={\"__proto__\":base};\
         return o.m===1&&o.__proto__===void 0;}"
    ));
}

/// A shorthand `__proto__` is an ordinary own data property, never an object
/// literal prototype mutation.
#[test]
fn a_shorthand_proto_key_stays_an_own_property() {
    assert_eq!(
        text(
            "function run(){\
                let __proto__=7;\
                let o={__proto__};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return o.__proto__+\":\"+keys;\
            }"
        ),
        "7:__proto__"
    );
}
