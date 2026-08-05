use std::sync::Arc;

use quickjs_bytecode::{FinalOpcode, FunctionTemplateId};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, JsString, Runtime, RuntimeLimits,
    ValueKind,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-test.js"))
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

fn runtime() -> Runtime {
    Runtime::try_new(RuntimeLimits::default()).expect("runtime")
}

#[test]
fn regexp_literal_opcode_allocates_a_branded_runtime_object() {
    let authority = compile("function make(){return /a+/giu;}", "make");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("RegExp literal execution");
    assert_eq!(result.kind().expect("live result"), ValueKind::Object);
}

#[test]
fn for_in_executes_ordered_own_keys_string_indices_and_getter_free_enumeration() {
    let authority = compile(
        "function run(missing){\
            let hits=0;\
            let object={2:0,1:0,b:0,get a(){hits++;return 0;}};\
            let own=\"\";\
            for(var key in object) own=own+(own?\",\":\"\")+key;\
            let string=\"\";\
            for(var key in \"A😀\") string=string+(string?\",\":\"\")+key;\
            let nullish=\"\";\
            for(var key in null) nullish=nullish+key;\
            for(var key in missing) nullish=nullish+key;\
            return own+\"|\"+hits+\"|\"+string+\"|\"+nullish;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("for-in execution");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "1,2,b,a|0|0,1,2|"
    );
}

#[test]
fn for_in_member_targets_captures_and_abrupt_control_use_verified_stack_shuffles() {
    let authority = compile(
        "function run(){\
            let source={a:1,b:1};\
            let target={key:\"\"};\
            let staticResult=\"\";\
            for(target.key in source) staticResult=staticResult+target.key;\
            let property=\"key\";\
            let computedResult=\"\";\
            for(target[property] in source) computedResult=computedResult+target[property];\
            let first;\
            let second;\
            let index=0;\
            for(const key in source){\
                if(index===0) first=function firstCapture(){return key;};\
                else second=function secondCapture(){return key;};\
                index++;\
            }\
            let nested=\"\";\
            outer:for(const left in source){\
                for(const right in {x:1,y:1}){\
                    if(left===\"a\"&&right===\"x\") continue outer;\
                    nested=nested+left+right;\
                    if(left===\"b\"&&right===\"x\") break outer;\
                }\
            }\
            let returned=\"\";\
            for(const left in {x:1}){for(const right in {y:1}) return \
                staticResult+\"|\"+computedResult+\"|\"+first()+second()+\"|\"+nested+\"|\"+left+right;}\
            return returned;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("for-in targets and control");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "ab|ab|ab|bx|xy"
    );
}

#[test]
fn for_in_scan_work_consumes_instruction_fuel_and_unwind_reclaims_private_iterators() {
    let authority = compile("function scan(){for(var key in \"\"){}return 1;}", "scan");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline_objects = runtime.usage().heap_objects();
    {
        let mut context = runtime.context(&realm).expect("context");
        let function = context.instantiate(authority).expect("function");
        let error = context
            .call(
                &function,
                &[],
                ExecutionLimits::default().with_instruction_fuel(12),
            )
            .expect_err("for-in scan must consume bounded fuel");
        assert!(matches!(
            error,
            ExecutionError::InstructionLimitExceeded {
                limit: 12,
                executed: 12,
            }
        ));
    }

    runtime.collect_cycles().expect("collect iterator");
    assert_eq!(runtime.usage().heap_objects(), baseline_objects);
    assert_eq!(runtime.usage().for_in_entries(), 0);
}

#[test]
fn for_in_numeric_snapshot_charges_conservative_sort_work_before_iteration() {
    let source = "function make(){return {\
        16:0,15:0,14:0,13:0,12:0,11:0,10:0,9:0,\
        8:0,7:0,6:0,5:0,4:0,3:0,2:0,1:0};}\
        function scan(value){for(var key in value){}return 1;}";
    let make = compile(source, "make");
    let scan = compile(source, "scan");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let make = context.instantiate(make).expect("make");
        let scan = context.instantiate(scan).expect("scan");
        let object = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("numeric-key object");
        let usage_before_scan = context.runtime_usage();

        let error = context
            .call(
                &scan,
                &[object],
                ExecutionLimits::default().with_instruction_fuel(64),
            )
            .expect_err("snapshot sorting must exhaust fuel before the first key");
        assert!(matches!(
            error,
            ExecutionError::InstructionLimitExceeded {
                limit: 64,
                executed: 64,
            }
        ));
        assert_eq!(
            context.runtime_usage().for_in_entries(),
            usage_before_scan.for_in_entries(),
            "insufficient fuel must reject the snapshot preview before retaining any keys"
        );
        assert_eq!(
            context.runtime_usage().heap_objects(),
            usage_before_scan.heap_objects(),
            "insufficient fuel must reject the snapshot preview before allocating an iterator"
        );
    }

    runtime.collect_cycles().expect("collect iterator");
    assert_eq!(runtime.usage().for_in_entries(), 0);
}

#[test]
fn executes_verified_literals_arguments_locals_and_branches() {
    let authority = compile(
        "function choose(argument){\
            let local=argument;\
            if(local){return \"selected\";}\
            return 1.5;\
        }",
        "choose",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let truthy = context.boolean(true);
    let selected = context
        .call(&function, &[truthy], ExecutionLimits::default())
        .expect("truthy call");
    assert_eq!(selected.kind().expect("live value"), ValueKind::String);
    assert_eq!(
        selected
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "selected"
    );

    let falsy = context.boolean(false);
    let fallback = context
        .call(&function, &[falsy], ExecutionLimits::default())
        .expect("falsy call");
    assert_eq!(
        fallback
            .as_number()
            .expect("live value")
            .map(JsNumber::as_f64),
        Some(1.5)
    );
}

#[test]
fn labeled_loops_and_nested_switches_execute_with_the_nearest_control_target() {
    let authority = compile(
        "function run(limit){\
            let current=0;\
            let result=0;\
            outer: while(current<limit){\
                current++;\
                switch(current){\
                    case 1: result+=1; break;\
                    case 2: result+=10; continue outer;\
                    default: break outer;\
                }\
                result+=100;\
            }\
            return result;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let limit = context.number(JsNumber::from_i32(3));

    let result = context
        .call(&function, &[limit], ExecutionLimits::default())
        .expect("labeled switch execution");
    assert_eq!(
        result
            .as_number()
            .expect("live result")
            .map(JsNumber::as_f64),
        Some(111.0)
    );
}

#[test]
fn switch_case_tests_are_lazy_and_default_in_the_middle_is_only_the_fallback() {
    let authority = compile(
        "function run(value){\
            let order=0;\
            let result=0;\
            switch(value){\
                case (order=order*10+1,1): result=10;\
                default: result+=1;\
                case (order=order*10+2,2): result+=2; break;\
                case (order=order*10+3,3): result=30;\
            }\
            return order*100+result;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    for (argument, expected) in [(1, 113), (2, 1_202), (3, 12_330), (9, 12_303)] {
        let argument = context.number(JsNumber::from_i32(argument));
        let result = context
            .call(&function, &[argument], ExecutionLimits::default())
            .expect("switch execution");
        assert_eq!(
            result
                .as_number()
                .expect("live result")
                .map(JsNumber::as_f64),
            Some(f64::from(expected))
        );
    }
}

#[test]
fn directly_labeled_and_empty_switches_drop_unmatched_discriminants() {
    let authority = compile(
        "function run(value){\
            let result=1;\
            target: switch(value){\
                case 1: result=10; break target;\
            }\
            switch(value){}\
            return result+1;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    for (argument, expected) in [(1, 11), (9, 2)] {
        let argument = context.number(JsNumber::from_i32(argument));
        let result = context
            .call(&function, &[argument], ExecutionLimits::default())
            .expect("directly labeled switch execution");
        assert_eq!(
            result
                .as_number()
                .expect("live result")
                .map(JsNumber::as_f64),
            Some(f64::from(expected))
        );
    }
}

#[test]
fn switch_scope_is_tdz_initialized_before_the_first_case_test() {
    let authority = compile(
        "function run(value){ switch(value){ case local: let local=1; return local; } }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let value = context.number(JsNumber::from_i32(0));

    let error = context
        .call(&function, &[value], ExecutionLimits::default())
        .expect_err("switch case test TDZ");
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception, got {error:?}");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8 error message"),
        "local is not initialized"
    );
}

#[test]
fn regular_labeled_break_preserves_a_captured_block_cell() {
    let authority = compile(
        "function outer(value){\"use strict\";\
            let saved;\
            target:{\
                let current=value;\
                function capture(){return current;}\
                saved=capture;\
                break target;\
            }\
            return saved;\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let expected = context.number(JsNumber::from_i32(47));
    let capture = context
        .call(&outer, &[expected], ExecutionLimits::default())
        .expect("capture")
        .into_function()
        .expect("capture function");

    let result = context
        .call(&capture, &[], ExecutionLimits::default())
        .expect("captured cell");
    assert_eq!(
        result
            .as_number()
            .expect("live result")
            .map(JsNumber::as_f64),
        Some(47.0)
    );
}

#[test]
fn missing_arguments_are_undefined_and_extra_arguments_do_not_extend_formals() {
    let authority = compile("function identity(value){return value;}", "identity");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let missing = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("missing argument");
    assert_eq!(missing.kind().expect("live value"), ValueKind::Undefined);

    let first = context.number(JsNumber::from_i32(42));
    let ignored = context.string(JsString::from_utf8("ignored").expect("string"));
    let returned = context
        .call(&function, &[first, ignored], ExecutionLimits::default())
        .expect("extra argument");
    assert_eq!(
        returned
            .as_number()
            .expect("live value")
            .map(JsNumber::as_f64),
        Some(42.0)
    );
}

#[test]
fn returned_closures_share_and_forward_verified_binding_cells() {
    let authority = compile(
        "function outer(value){\"use strict\";\
            function middle(){\
                function inner(){return value;}\
                return inner;\
            }\
            return middle;\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let argument = context.number(JsNumber::from_i32(42));

    let middle = context
        .call(&outer, &[argument], ExecutionLimits::default())
        .expect("middle value")
        .into_function()
        .expect("middle function");
    let inner = context
        .call(&middle, &[], ExecutionLimits::default())
        .expect("inner value")
        .into_function()
        .expect("inner function");
    let answer = context
        .call(&inner, &[], ExecutionLimits::default())
        .expect("captured value");

    assert_eq!(
        answer
            .as_number()
            .expect("live value")
            .map(JsNumber::as_f64),
        Some(42.0)
    );
}

#[test]
fn sibling_closures_share_one_mutable_activation_cell() {
    let authority = compile(
        "function outer(value){\
            function get(){return value;}\
            function set(next){value=next;return get;}\
            return set;\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let initial = context.number(JsNumber::from_i32(1));
    let set = context
        .call(&outer, &[initial], ExecutionLimits::default())
        .expect("set value")
        .into_function()
        .expect("set function");

    let replacement = context.number(JsNumber::from_i32(9));
    let get = context
        .call(&set, &[replacement], ExecutionLimits::default())
        .expect("get value")
        .into_function()
        .expect("get function");
    let current = context
        .call(&get, &[], ExecutionLimits::default())
        .expect("shared cell value");
    assert_eq!(
        current
            .as_number()
            .expect("live value")
            .map(JsNumber::as_f64),
        Some(9.0)
    );
}

#[test]
fn scope_exit_detaches_captured_cells_without_invalidating_closures() {
    let authority = compile(
        "function outer(value){\"use strict\";\
            let saved;\
            for(let current=value;current;current=false){\
                function capture(){return current;}\
                saved=capture;\
                break;\
            }\
            return saved;\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let argument = context.string(JsString::from_utf8("cell").expect("string"));

    let capture = context
        .call(&outer, &[argument], ExecutionLimits::default())
        .expect("capture value")
        .into_function()
        .expect("capture function");
    let captured = context
        .call(&capture, &[], ExecutionLimits::default())
        .expect("captured value");
    assert_eq!(
        captured
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "cell"
    );
}

#[test]
fn classic_for_rotation_gives_each_iteration_a_distinct_captured_cell() {
    let authority = compile(
        "function outer(first,second){\"use strict\";\
            let old;\
            let latest;\
            for(\
                let current=first;\
                current;\
                current=current===first?second:false\
            ){\
                function capture(){return current;}\
                if(current===first){old=capture;}else{latest=capture;}\
            }\
            function choose(oldCell){if(oldCell){return old;}return latest;}\
            return choose;\
        }",
        "outer",
    );
    let root = authority.root();
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::CloseLoc
            })
    );

    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let first = context.string(JsString::from_utf8("first").expect("string"));
    let second = context.string(JsString::from_utf8("second").expect("string"));
    let choose = context
        .call(&outer, &[first, second], ExecutionLimits::default())
        .expect("choose value")
        .into_function()
        .expect("choose function");

    let select_old = context.boolean(true);
    let old = context
        .call(&choose, &[select_old], ExecutionLimits::default())
        .expect("old capture")
        .into_function()
        .expect("old function");
    let select_latest = context.boolean(false);
    let latest = context
        .call(&choose, &[select_latest], ExecutionLimits::default())
        .expect("latest capture")
        .into_function()
        .expect("latest function");

    let old_value = context
        .call(&old, &[], ExecutionLimits::default())
        .expect("old value");
    let latest_value = context
        .call(&latest, &[], ExecutionLimits::default())
        .expect("latest value");
    assert_eq!(
        old_value
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "first"
    );
    assert_eq!(
        latest_value
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "second"
    );
}

#[test]
fn tdz_throws_exact_reference_error_with_verified_location() {
    let authority = compile("function fail(){return lexical;let lexical=1;}", "fail");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let error = context
        .call(&function, &[], ExecutionLimits::default())
        .expect_err("TDZ");
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception, got {error:?}");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8 error message"),
        "lexical is not initialized"
    );
    assert_eq!(exception.function(), FunctionTemplateId::new(0));
    assert_eq!(exception.source_name(), "runtime-test.js");
    assert!(exception.pc().get() > 0);
    assert!(exception.source_span().end() > exception.source_span().start());
}

#[test]
fn tdz_write_throws_before_consuming_or_initializing_the_binding() {
    let authority = compile(
        "function fail(){lexical=1;let lexical;return lexical;}",
        "fail",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");

    let error = context
        .call(&function, &[], ExecutionLimits::default())
        .expect_err("TDZ write");
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception, got {error:?}");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8 error message"),
        "lexical is not initialized"
    );
}

#[test]
fn captured_tdz_state_survives_frame_teardown() {
    let authority = compile(
        "function outer(){\"use strict\";\
            function inner(){return lexical;}\
            return inner;\
            let lexical=1;\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let inner = context
        .call(&outer, &[], ExecutionLimits::default())
        .expect("inner value")
        .into_function()
        .expect("inner function");

    let error = context
        .call(&inner, &[], ExecutionLimits::default())
        .expect_err("captured TDZ");
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception, got {error:?}");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8 error message"),
        "lexical is not initialized"
    );
    assert_eq!(exception.function(), FunctionTemplateId::new(1));
}

#[test]
fn whole_graph_feature_admission_accepts_supported_unreachable_object_operators() {
    let authority = compile(
        "function fail(){\
            if(false){return \"key\" in {};}\
            function child(left,right){return left instanceof right;}\
            return 0;\
        }",
        "fail",
    );
    let opcodes = authority
        .functions()
        .flat_map(|function| {
            function
                .function()
                .control_flow()
                .instructions()
                .iter()
                .copied()
                .map(|instruction| instruction.decoded().instruction().opcode())
        })
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::In));
    assert!(opcodes.contains(&FinalOpcode::InstanceOf));

    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    runtime
        .context(&realm)
        .expect("context")
        .instantiate(authority)
        .expect("supported object operators across the complete graph");
}

#[test]
fn instruction_fuel_interrupts_an_infinite_verified_loop() {
    let authority = compile("function spin(value){while(value){}return 0;}", "spin");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let truthy = context.boolean(true);

    let error = context
        .call(
            &function,
            &[truthy],
            ExecutionLimits::default().with_instruction_fuel(64),
        )
        .expect_err("fuel");
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded {
            limit: 64,
            executed: 64
        }
    ));
}

#[test]
fn values_and_functions_cannot_cross_runtime_boundaries() {
    let authority = compile("function identity(value){return value;}", "identity");
    let mut first = runtime();
    let first_realm = first.create_realm().expect("first realm");
    let mut first_context = first.context(&first_realm).expect("first context");
    let function = first_context
        .instantiate(Arc::clone(&authority))
        .expect("first function");

    let mut second = runtime();
    let second_realm = second.create_realm().expect("second realm");
    let mut second_context = second.context(&second_realm).expect("second context");
    let foreign_argument = second_context.number(JsNumber::from_i32(1));

    let error = first_context
        .call(&function, &[foreign_argument], ExecutionLimits::default())
        .expect_err("foreign value");
    assert!(matches!(error, ExecutionError::Handle(_)));

    let error = second_context
        .call(&function, &[], ExecutionLimits::default())
        .expect_err("foreign function");
    assert!(matches!(error, ExecutionError::Handle(_)));
}

#[test]
fn orphaned_and_wrong_kind_handles_fail_without_runtime_access() {
    let value = {
        let mut runtime = runtime();
        let realm = runtime.create_realm().expect("realm");
        let context = runtime.context(&realm).expect("context");
        let value = context.number(JsNumber::from_i32(1));
        assert!(matches!(
            value.clone().into_function(),
            Err(quickjs_runtime::HandleError::WrongValueKind {
                expected: ValueKind::Function,
                actual: ValueKind::Number,
            })
        ));
        value
    };

    assert!(matches!(
        value.kind(),
        Err(quickjs_runtime::HandleError::Orphaned {
            kind: quickjs_runtime::HandleKind::Value,
        })
    ));
}

#[test]
fn closure_cycles_are_reclaimed_after_last_public_root_drops() {
    let authority = compile(
        "function outer(){function recursive(){return recursive;}return recursive;}",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let outer = context.instantiate(authority).expect("outer");
        let recursive = context
            .call(&outer, &[], ExecutionLimits::default())
            .expect("recursive value")
            .into_function()
            .expect("recursive function");
        let same = context
            .call(&recursive, &[], ExecutionLimits::default())
            .expect("self value")
            .into_function()
            .expect("self function");
        assert!(recursive.same_identity(&same).expect("live functions"));
    }

    runtime.collect_cycles().expect("cycle collection");
    let collected = runtime.usage();
    assert_eq!(collected, baseline);
}

#[test]
fn cloned_function_handles_release_one_logical_root_on_last_drop() {
    let authority = compile("function value(){return 1;}", "value");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let clone = {
        let mut context = runtime.context(&realm).expect("context");
        let function = context.instantiate(authority).expect("function");
        let clone = function.clone();
        assert_eq!(context.runtime_usage().public_roots(), 1);

        drop(function);
        assert_eq!(context.runtime_usage().public_roots(), 1);
        clone
    };
    assert_eq!(runtime.usage().pending_releases(), 0);

    drop(clone);
    assert_eq!(runtime.usage().pending_releases(), 1);
    runtime.collect_cycles().expect("collection");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn long_lived_context_drains_dropped_return_roots_before_call_limits() {
    let authority = compile(
        "function factory(){function result(){return 1;}return result;}",
        "factory",
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_public_roots(2)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let factory = context.instantiate(authority).expect("factory");

    let first = context
        .call(&factory, &[], ExecutionLimits::default())
        .expect("first result");
    drop(first);
    assert_eq!(context.runtime_usage().pending_releases(), 1);

    let second = context
        .call(&factory, &[], ExecutionLimits::default())
        .expect("the dropped return root must be drained before execution");
    assert_eq!(context.runtime_usage().public_roots(), 2);
    assert_eq!(context.runtime_usage().pending_releases(), 0);
    drop(second);
}

#[test]
fn failed_multi_capture_closure_creation_is_failure_atomic() {
    let authority = compile(
        "function outer(first,second){\
            function inner(selectFirst){\
                if(selectFirst){return first;}\
                return second;\
            }\
            return inner;\
        }",
        "outer",
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_binding_cells(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let before = context.runtime_usage();
    let first = context.number(JsNumber::from_i32(1));
    let second = context.number(JsNumber::from_i32(2));

    let error = context
        .call(&outer, &[first, second], ExecutionLimits::default())
        .expect_err("two captures exceed the binding-cell ceiling");
    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: quickjs_runtime::RuntimeResource::BindingCells,
            limit: 1,
            observed: 2,
        }
    ));
    assert_eq!(context.runtime_usage(), before);
}

#[test]
fn ignored_extra_arguments_are_validated_without_becoming_frame_values() {
    let authority = compile("function constant(){return 1;}", "constant");
    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_active_frame_values(2))
        .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let constant = context.instantiate(authority).expect("constant");
    let extras = (0..1_024)
        .map(|value| context.number(JsNumber::from_i32(value)))
        .collect::<Vec<_>>();

    let result = context
        .call(&constant, &extras, ExecutionLimits::default())
        .expect("extra arguments are not materialized in the frame");
    assert_eq!(
        result
            .as_number()
            .expect("live value")
            .map(JsNumber::as_f64),
        Some(1.0)
    );
}

#[test]
fn safe_points_reclaim_transient_acyclic_closures_before_heap_limits() {
    let authority = compile(
        "function outer(){function transient(){return 1;}return 0;}",
        "outer",
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_functions(348)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");

    for _ in 0..8 {
        let value = context
            .call(&outer, &[], ExecutionLimits::default())
            .expect("transient closure call");
        assert_eq!(
            value.as_number().expect("live value").map(JsNumber::as_f64),
            Some(0.0)
        );
        assert_eq!(context.runtime_usage().heap_functions(), 348);
    }
}

#[test]
fn captured_cell_writes_dirty_the_safe_point_collector() {
    let outer = compile(
        "function outer(){\
            let held;\
            function set(value){held=value;return 0;}\
            return set;\
        }",
        "outer",
    );
    let maker = compile(
        "function maker(){function made(){return 1;}return made;}",
        "maker",
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_functions(349)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(outer).expect("outer");
    let maker = context.instantiate(maker).expect("maker");
    let setter = context
        .call(&outer, &[], ExecutionLimits::default())
        .expect("setter value")
        .into_function()
        .expect("setter");
    drop(outer);

    let displaced = context
        .call(&maker, &[], ExecutionLimits::default())
        .expect("displaced value")
        .into_function()
        .expect("displaced closure");
    context
        .call(&setter, &[displaced.as_value()], ExecutionLimits::default())
        .expect("store closure");
    drop(displaced);

    let undefined = context.undefined();
    context
        .call(&setter, &[undefined], ExecutionLimits::default())
        .expect("displace closure");
    let replacement = context
        .call(&maker, &[], ExecutionLimits::default())
        .expect("the next safe point must reclaim the displaced closure")
        .into_function()
        .expect("replacement closure");
    assert_eq!(context.runtime_usage().heap_functions(), 349);
    drop(replacement);
}
