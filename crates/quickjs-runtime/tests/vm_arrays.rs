use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    Context, ExecutionError, ExecutionLimits, JsNumber, JsValue, Realm, Runtime, RuntimeLimits,
    RuntimeResource,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-arrays.js"))
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

fn with_context<T>(
    runtime: &mut Runtime,
    realm: &Realm,
    operation: impl FnOnce(&mut Context<'_>) -> T,
) -> T {
    let mut context = runtime.context(realm).expect("context");
    operation(&mut context)
}

fn assert_number(value: &JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

#[test]
fn empty_and_dense_array_literals_preserve_length_and_left_to_right_values() {
    let empty = compile("function empty(){return [].length;}", "empty");
    let dense = compile(
        "function dense(){\
            let next=0;\
            let values=[next=next+1,next=next+1,next=next+1];\
            return values.length*1000+values[0]*100+values[1]*10+values[2];\
        }",
        "dense",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let empty = context.instantiate(empty).expect("empty");
    let dense = context.instantiate(dense).expect("dense");

    let result = context
        .call(&empty, &[], ExecutionLimits::default())
        .expect("empty array");
    assert_number(&result, 0);

    let result = context
        .call(&dense, &[], ExecutionLimits::default())
        .expect("dense array");
    assert_number(&result, 3123);
}

#[test]
fn array_index_and_length_writes_follow_array_exotic_semantics() {
    let authority = compile(
        "function mutate(){\
            let values=[1,2,3];\
            values.length=1;\
            if(values.length!==1){return -1;}\
            if(values[1]!==void 0){return -2;}\
            values[2]=9;\
            return values.length*100+values[0]*10+values[2];\
        }",
        "mutate",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mutate = context.instantiate(authority).expect("mutate");

    let result = context
        .call(&mutate, &[], ExecutionLimits::default())
        .expect("array mutation");
    assert_number(&result, 319);
}

#[test]
fn non_number_array_lengths_run_the_quickjs_two_pass_conversion() {
    let authority = compile(
        "function convert(){\
            let out=\"\";\
            let values=[1,2];\
            let length={valueOf(){out=out+\"v\";return 1;}};\
            values.length=length;\
            return out+\"|\"+values.length+\"|\"+values[1];\
        }",
        "convert",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let convert = context.instantiate(authority).expect("convert");

    let result = context
        .call(&convert, &[], ExecutionLimits::default())
        .expect("resumable two-pass conversion");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "vv|1|undefined"
    );
}

#[test]
fn disagreeing_array_length_conversions_throw_before_mutating_length() {
    let authority = compile(
        "function mismatch(){\
            let calls=0;\
            let values=[1,2];\
            let length={valueOf(){calls=calls+1;return calls;}};\
            try{values.length=length;}catch(error){\
                return calls+\"|\"+error.name+\":\"+error.message+\"|\"+values.length;\
            }\
            return \"missing RangeError\";\
        }",
        "mismatch",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mismatch = context.instantiate(authority).expect("mismatch");

    let result = context
        .call(&mismatch, &[], ExecutionLimits::default())
        .expect("caught RangeError");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "2|RangeError:invalid array length|2"
    );
}

#[test]
fn object_prototype_to_string_uses_the_array_default_tag() {
    let authority = compile("function tag(){return ({}).toString.call([]);}", "tag");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let tag = context.instantiate(authority).expect("tag");

    let result = context
        .call(&tag, &[], ExecutionLimits::default())
        .expect("array tag");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "[object Array]"
    );
}

#[test]
fn array_allocation_limit_failures_roll_back_heap_and_properties() {
    let authority = compile(
        "function run(make){if(make){return [1,2,3].length;}return 7;}",
        "run",
    );

    let baseline = {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("probe runtime");
        let realm = runtime.create_realm().expect("probe realm");
        with_context(&mut runtime, &realm, |context| {
            let _run = context
                .instantiate(Arc::clone(&authority))
                .expect("probe function");
            context.runtime_usage()
        })
    };

    for (limits, expected_resource) in [
        (
            RuntimeLimits::default().with_max_heap_objects(baseline.heap_objects()),
            RuntimeResource::HeapObjects,
        ),
        (
            RuntimeLimits::default()
                .with_max_object_properties(baseline.object_properties().saturating_add(2)),
            RuntimeResource::ObjectProperties,
        ),
    ] {
        let mut runtime = Runtime::try_new(limits).expect("limited runtime");
        let realm = runtime.create_realm().expect("limited realm");
        let mut context = runtime.context(&realm).expect("context");
        let run = context.instantiate(Arc::clone(&authority)).expect("run");
        let before = context.runtime_usage();
        assert_eq!(before, baseline);

        let make = context.boolean(true);
        let error = context
            .call(&run, &[make], ExecutionLimits::default())
            .expect_err("array allocation must exceed its configured limit");
        assert!(
            matches!(
                error,
                ExecutionError::LimitExceeded { resource, .. } if resource == expected_resource
            ),
            "{error:?}"
        );
        assert_eq!(context.runtime_usage(), before);

        let skip = context.boolean(false);
        let result = context
            .call(&run, &[skip], ExecutionLimits::default())
            .expect("runtime remains reusable after rollback");
        assert_number(&result, 7);
    }
}
