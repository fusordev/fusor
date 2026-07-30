use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, JsString, JsValue, Runtime,
    RuntimeLimits,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(
                unit,
                Arc::from("runtime-computed-properties.js"),
            )
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

fn assert_boolean(value: &JsValue, expected: bool) {
    assert_eq!(
        value.as_boolean().expect("live value"),
        Some(expected),
        "unexpected Boolean completion"
    );
}

fn assert_number(value: &JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("Number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

fn assert_type_error(error: ExecutionError, expected: &str) {
    let ExecutionError::Exception(exception) = error else {
        panic!("expected a JavaScript exception");
    };
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

#[test]
fn computed_read_and_call_coerce_before_arguments_and_keep_the_receiver() {
    let source = "function run(){\
        let argumentRan=false;\
        let coercedBeforeArgument=false;\
        let key={toString(){coercedBeforeArgument=!argumentRan;return \"method\";}};\
        let object={method(){return this;}};\
        function argument(){argumentRan=true;return 1;}\
        let receiver=object[key](argument());\
        return coercedBeforeArgument&&argumentRan&&receiver===object;\
    }";
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let completion = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("computed method call");
    assert_boolean(&completion, true);
}

#[test]
fn computed_assignment_coerces_after_rhs_and_preserves_the_rhs_completion() {
    let source = "function run(){\
        let rhsRan=false;\
        let coercionSawRhs=false;\
        let key={toString(){coercionSawRhs=rhsRan;return \"value\";}};\
        let object={value:1};\
        function rhs(){rhsRan=true;return 9;}\
        let completion=(object[key]=rhs());\
        return coercionSawRhs&&completion===9&&object.value===9;\
    }";
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let completion = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("computed assignment");
    assert_boolean(&completion, true);
}

#[test]
fn computed_data_definition_coerces_before_evaluating_its_value() {
    let source = "function run(){\
        let rhsRan=false;\
        let coercionBeforeRhs=false;\
        let key={toString(){coercionBeforeRhs=!rhsRan;return \"value\";}};\
        function rhs(){rhsRan=true;return 7;}\
        let object={[key]:rhs()};\
        return coercionBeforeRhs&&rhsRan&&object.value===7;\
    }";
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let completion = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("computed data definition");
    assert_boolean(&completion, true);
}

#[test]
fn nullish_computed_read_never_coerces_the_key_object() {
    let source = "function run(){\
        let key={toString(){throw 9;}};\
        return null[key];\
    }";
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let Err(ExecutionError::Exception(exception)) =
        context.call(&run, &[], ExecutionLimits::default())
    else {
        panic!("nullish computed read must throw");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("message"),
        "cannot read property of null"
    );
}

#[test]
fn distinct_symbols_are_distinct_computed_keys() {
    let authority = compile(
        "function run(left,right){\
            let object={[left]:1,[right]:2};\
            return object[left]===1&&object[right]===2;\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let description = JsString::from_utf8("same").expect("description");
    let left = context.symbol(Some(&description)).expect("left symbol");
    let right = context.symbol(Some(&description)).expect("right symbol");

    let completion = context
        .call(&run, &[left, right], ExecutionLimits::default())
        .expect("symbol-keyed object");
    assert_boolean(&completion, true);
}

#[test]
fn computed_methods_accessors_and_symbol_names_execute() {
    let authority = compile(
        "function run(key,emptyKey){\
            let stored=1;\
            let object={\
                [key](){return this;},\
                [emptyKey](){return 2;},\
                get [\"read\"](){return stored;},\
                set [\"write\"](next){stored=next;}\
            };\
            object[\"write\"]=8;\
            return object[key]()===object&&object[\"read\"]===8&&\
                object[key].name===\"[token]\"&&object[emptyKey].name===\"\";\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let description = JsString::from_utf8("token").expect("description");
    let key = context.symbol(Some(&description)).expect("symbol");
    let empty_key = context.symbol(None).expect("description-less symbol");

    let completion = context
        .call(&run, &[key, empty_key], ExecutionLimits::default())
        .expect("computed methods and accessors");
    assert_boolean(&completion, true);
}

#[test]
fn computed_primitive_keys_use_javascript_property_key_strings() {
    let authority = compile(
        "function run(negativeZero,nan,infinity){\
            let object={\
                [void 0]:1,\
                [null]:2,\
                [true]:3,\
                [negativeZero]:4,\
                [nan]:5,\
                [infinity]:6\
            };\
            return object[void 0]===1&&object[null]===2&&object[true]===3&&\
                object[0]===4&&object[nan]===5&&object[infinity]===6;\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let negative_zero = context.number(JsNumber::from_f64(-0.0));
    let nan = context.number(JsNumber::from_f64(f64::NAN));
    let infinity = context.number(JsNumber::from_f64(f64::INFINITY));
    let completion = context
        .call(
            &run,
            &[negative_zero, nan, infinity],
            ExecutionLimits::default(),
        )
        .expect("primitive computed keys");
    assert_boolean(&completion, true);
}

#[test]
fn computed_proto_spelling_is_an_ordinary_own_property() {
    let authority = compile(
        "function run(){\
            let object={[\"__proto__\"]:7,[\"__proto__\"]:9};\
            return object[\"__proto__\"];\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let completion = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("computed __proto__ property");
    assert_number(&completion, 9);
}

#[test]
fn computed_reads_cover_string_indices_and_symbol_descriptions() {
    let string_authority = compile("function stringRead(){return \"abc\"[1];}", "stringRead");
    let symbol_authority = compile(
        "function symbolRead(symbol){return symbol[\"description\"]===symbol.description;}",
        "symbolRead",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let string_read = context
        .instantiate(string_authority)
        .expect("string reader");
    let string = context
        .call(&string_read, &[], ExecutionLimits::default())
        .expect("string index");
    assert_eq!(
        string
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "b"
    );

    let symbol_read = context
        .instantiate(symbol_authority)
        .expect("symbol reader");
    let description = JsString::from_utf8("token").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");
    let completion = context
        .call(&symbol_read, &[symbol], ExecutionLimits::default())
        .expect("symbol description");
    assert_boolean(&completion, true);
}

#[test]
fn nullish_computed_writes_coerce_before_the_exact_named_error() {
    let object_authority = compile(
        "function objectKey(){let key={toString(){return \"x\";}};null[key]=1;}",
        "objectKey",
    );
    let symbol_authority = compile("function symbolKey(key){null[key]=1;}", "symbolKey");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let object_key = context
        .instantiate(object_authority)
        .expect("object-key writer");
    assert_type_error(
        context
            .call(&object_key, &[], ExecutionLimits::default())
            .expect_err("null write"),
        "cannot set property 'x' of null",
    );

    let symbol_key = context
        .instantiate(symbol_authority)
        .expect("symbol-key writer");
    let description = JsString::from_utf8("token").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");
    assert_type_error(
        context
            .call(&symbol_key, &[symbol], ExecutionLimits::default())
            .expect_err("null Symbol write"),
        "cannot set property 'token' of null",
    );
}

#[test]
fn exceptions_from_every_parked_computed_operation_escape_as_javascript_values() {
    let cases = [
        (
            "function run(){let key={toString(){throw 31;}};return ({value:1})[key];}",
            31,
        ),
        (
            "function run(){let key={toString(){throw 32;}};return ({value(){}})[key]();}",
            32,
        ),
        (
            "function run(){let key={toString(){return \"value\";}};\
             let object={set value(next){throw next;}};object[key]=33;}",
            33,
        ),
        (
            "function run(){let key={toString(){throw 34;}};return {[key]:1};}",
            34,
        ),
        (
            "function run(){let key={toString(){throw 35;}};return {[key](){}};}",
            35,
        ),
    ];

    for (source, expected) in cases {
        let authority = compile(source, "run");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let run = context.instantiate(authority).expect("run");
        let Err(ExecutionError::Exception(exception)) =
            context.call(&run, &[], ExecutionLimits::default())
        else {
            panic!("computed operation must preserve its JavaScript throw");
        };
        let thrown = exception
            .thrown_value()
            .expect("explicit throw")
            .as_number()
            .expect("live value")
            .expect("Number");
        assert!(thrown.strict_equals(JsNumber::from_i32(expected)));
        assert_eq!(exception.caller_frames().len(), 1);
    }
}
