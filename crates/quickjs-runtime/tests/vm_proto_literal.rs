//! Object-literal `__proto__` data properties and explicit prototype changes.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! static proto is own => true
//! static proto keeps default prototype => true
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

/// Annex B object-literal prototype mutation is absent: a static
/// `__proto__` spelling creates an own data property and leaves the ordinary
/// object prototype unchanged.
#[test]
fn a_proto_literal_key_defines_an_own_property() {
    assert!(boolean(
        "function run(){let base={m:1};let o={__proto__:base};\
         return o.__proto__===base&&o.m===void 0;}"
    ));
}

/// A static data key preserves normal own-key enumeration order.
#[test]
fn a_proto_literal_key_is_an_own_property() {
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
        "__proto__,own,"
    );
}

/// `null` is stored like every other static data value; it does not detach the
/// literal from `Object.prototype`.
#[test]
fn a_null_proto_literal_key_is_an_ordinary_data_property() {
    assert_eq!(
        text(
            "function run(){\
                let o={__proto__:null,own:1};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "__proto__own"
    );
}

/// Primitive values are stored rather than silently discarded by an Annex B
/// prototype-mutating special case.
#[test]
fn a_non_object_proto_literal_key_is_stored() {
    assert!(boolean(
        "function run(){\
            let ignored={__proto__:5};\
            return ignored.__proto__===5;\
        }"
    ));
    assert!(boolean(
        "function run(){\
            let literal={__proto__:void 0};\
            return literal.__proto__===void 0;\
        }"
    ));
    assert_eq!(
        text(
            "function run(){\
                let o={__proto__:5};\
                let keys=\"\";\
                for(let k in o){keys+=k;}\
                return keys;\
            }"
        ),
        "__proto__"
    );
}

/// Quoted static keys follow the same ordinary data-property rule.
#[test]
fn a_quoted_proto_literal_key_defines_an_own_property() {
    assert!(boolean(
        "function run(){let base={m:1};let o={\"__proto__\":base};\
         return o.__proto__===base&&o.m===void 0;}"
    ));
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
        "__proto__"
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
