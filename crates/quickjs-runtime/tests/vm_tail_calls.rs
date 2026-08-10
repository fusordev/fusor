use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{ExecutionLimits, JsNumber, Runtime, RuntimeLimits};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-tail-calls.js"))
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

fn compile_global(source: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-tail-calls.js"))
                    .expect("storage plan");
            let tree = context
                .compile_global_script(quickjs_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn run_number(source: &str, arguments: &[i32], frame_limit: u32, fuel: u64) -> JsNumber {
    let authority = compile(source, "run");
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(frame_limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let arguments = arguments
        .iter()
        .map(|value| context.number(JsNumber::from_i32(*value)))
        .collect::<Vec<_>>();
    let result = context
        .call(
            &function,
            &arguments,
            ExecutionLimits::default().with_instruction_fuel(fuel),
        )
        .expect("tail call completed");
    result.as_number().expect("live result").expect("number")
}

fn run_global_number(source: &str, frame_limit: u32, fuel: u64) -> JsNumber {
    let authority = compile_global(source);
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(frame_limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    context
        .execute_global_script(
            authority,
            ExecutionLimits::default().with_instruction_fuel(fuel),
        )
        .expect("Global Script completed")
        .as_number()
        .expect("live result")
        .expect("number")
}

#[test]
fn strict_self_tail_recursion_reuses_one_execution_frame() {
    let result = run_number(
        "function run(count,total){'use strict';\
            function loop(count,total){'use strict';\
                if(count===0)return total;\
                return loop(count-1,total+1);\
            }\
            return loop(count,total);\
        }",
        &[100_000, 0],
        1,
        2_000_000,
    );
    assert!(result.strict_equals(JsNumber::from_i32(100_000)));
}

#[test]
fn tail_method_calls_preserve_the_receiver() {
    let result = run_number(
        "function run(count){'use strict';\
            let holder={\
                value:41,\
                step(n){if(n===0)return this.value;return this.step(n-1);}\
            };\
            return holder.step(count);\
        }",
        &[10_000],
        1,
        500_000,
    );
    assert!(result.strict_equals(JsNumber::from_i32(41)));
}

#[test]
fn non_intrinsic_eval_identifier_uses_a_tail_transfer() {
    let result = run_number(
        "function run(count){\
            function loop(count){'use strict';\
                if(count===0)return 19;\
                return eval(count-1);\
            }\
            var eval=loop;\
            return loop(count);\
        }",
        &[10_000],
        2,
        500_000,
    );
    assert!(result.strict_equals(JsNumber::from_i32(19)));

    let spread = run_number(
        "function run(count){\
            function loop(count){'use strict';\
                if(count===0)return 23;\
                return eval(...[count-1]);\
            }\
            var eval=loop;\
            return loop(count);\
        }",
        &[1_000],
        3,
        3_000_000,
    );
    assert!(spread.strict_equals(JsNumber::from_i32(23)));
}

#[test]
fn intrinsic_eval_non_string_completion_is_returned_from_tail_position() {
    let result = run_global_number("(function(){'use strict';return eval(83);})();", 2, 10_000);
    assert!(result.strict_equals(JsNumber::from_i32(83)));
}

#[test]
fn native_and_bound_tail_targets_preserve_constant_frame_usage() {
    let native = run_global_number(
        "(function(){'use strict';return Math.abs(-31);})();",
        2,
        10_000,
    );
    assert!(native.strict_equals(JsNumber::from_i32(31)));

    let bound = run_global_number(
        "(function(){'use strict';\
            function loop(count){'use strict';\
                if(count===0)return 29;\
                return bound(count-1);\
            }\
            const bound=loop.bind(null);\
            return loop(10000);\
        })();",
        2,
        1_000_000,
    );
    assert!(bound.strict_equals(JsNumber::from_i32(29)));
}

#[test]
fn transparent_proxy_tail_calls_do_not_retain_each_caller() {
    let result = run_global_number(
        "(function(){'use strict';\
            function target(count){'use strict';\
                if(count===0)return 37;\
                return proxy(count-1);\
            }\
            const proxy=new Proxy(target,{});\
            return proxy(10000);\
        })();",
        3,
        2_000_000,
    );
    assert!(result.strict_equals(JsNumber::from_i32(37)));
}

#[test]
fn tail_spread_calls_transfer_without_growing_frames() {
    // Spread evaluation invokes the iterator protocol before PrepareForTailCall,
    // so it temporarily needs one continuation in addition to the caller. The
    // subsequent transfer still replaces that caller and remains bounded at two
    // frames regardless of recursion depth.
    let result = run_number(
        "function run(count){'use strict';\
            function loop(count){'use strict';\
                if(count===0)return 7;\
                return loop(...[count-1]);\
            }\
            return loop(count);\
        }",
        &[10_000],
        2,
        20_000_000,
    );
    assert!(result.strict_equals(JsNumber::from_i32(7)));
}

#[test]
fn constructor_completion_is_preserved_across_tail_transfers() {
    let result = run_number(
        "function run(){\
            function replacement(){'use strict';return {value:73};}\
            function missing(){'use strict';}\
            function primitive(){'use strict';return 1;}\
            class TailConstructor{constructor(){return replacement();}}\
            class ReceiverConstructor{constructor(){this.value=11;return missing();}}\
            class Base{}\
            class DerivedObject extends Base{constructor(){return replacement();}}\
            class DerivedReceiver extends Base{\
                constructor(){super();this.value=17;return missing();}\
            }\
            class DerivedPrimitive extends Base{constructor(){return primitive();}}\
            let invalid=0;\
            try{new DerivedPrimitive();}catch(error){invalid=error.name==='TypeError'?1:0;}\
            return new TailConstructor().value\
                +new ReceiverConstructor().value\
                +new DerivedObject().value\
                +new DerivedReceiver().value\
                +invalid;\
        }",
        &[],
        4,
        100_000,
    );
    assert!(result.strict_equals(JsNumber::from_i32(175)));
}
