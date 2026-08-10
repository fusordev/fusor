use quickjs_bytecode::{FinalOpcode, Operands, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledLeafFunction};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("tail-call lowering")
        },
    )
    .expect("front-end acceptance")
}

fn instructions(compiled: &CompiledLeafFunction) -> Vec<(FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn tail_count(compiled: &CompiledLeafFunction) -> usize {
    instructions(compiled)
        .iter()
        .filter(|(opcode, _)| {
            matches!(
                opcode,
                FinalOpcode::TailCall
                    | FinalOpcode::TailCallMethod
                    | FinalOpcode::TailApply
                    | FinalOpcode::TailEval
                    | FinalOpcode::TailApplyEval
            )
        })
        .count()
}

#[test]
fn strict_calls_use_terminal_direct_method_tagged_and_spread_forms() {
    let cases = [
        (
            "function invoke(fn){'use strict';return fn();}",
            FinalOpcode::TailCall,
            Operands::NPop { argument_count: 0 },
        ),
        (
            "function invoke(holder,value){'use strict';return holder.fn(value);}",
            FinalOpcode::TailCallMethod,
            Operands::NPop { argument_count: 1 },
        ),
        (
            "function invoke(tag){'use strict';return tag`value`;}",
            FinalOpcode::TailCall,
            Operands::NPop { argument_count: 1 },
        ),
        (
            "function invoke(fn,values){'use strict';return fn(...values);}",
            FinalOpcode::TailApply,
            Operands::U16(0),
        ),
        (
            "function invoke(){'use strict';return eval(1);}",
            FinalOpcode::TailEval,
            Operands::NPopU16 {
                argument_count: 1,
                scope_index: 1,
            },
        ),
        (
            "function invoke(values){'use strict';return eval(...values);}",
            FinalOpcode::TailApplyEval,
            Operands::U16(1),
        ),
    ];

    for (source, opcode, operands) in cases {
        let compiled = compile(source, "invoke");
        assert!(
            instructions(&compiled).contains(&(opcode, operands)),
            "{source}"
        );
        assert_eq!(tail_count(&compiled), 1, "{source}");
    }
}

#[test]
fn tail_position_propagates_only_through_the_spec_expression_shapes() {
    let cases = [
        ("function invoke(a,b){'use strict';return (a(),b());}", 1),
        (
            "function invoke(test,a,b){'use strict';return test?a():b();}",
            2,
        ),
        ("function invoke(test,a){'use strict';return test&&a();}", 1),
        (
            "function invoke(holder){'use strict';return holder?.fn();}",
            1,
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(tail_count(&compile(source, "invoke")), expected, "{source}");
    }

    let nested = compile(
        "function invoke(outer,inner){'use strict';return outer(inner());}",
        "invoke",
    );
    let opcodes = instructions(&nested);
    assert_eq!(tail_count(&nested), 1);
    assert!(
        opcodes
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::Call0)
    );
}

#[test]
fn sloppy_async_catch_and_for_of_positions_remain_ordinary_calls() {
    let cases = [
        "function invoke(fn){return fn();}",
        "async function invoke(fn){'use strict';return fn();}",
        "function invoke(fn){'use strict';try{return fn();}catch(error){return 0;}}",
        "function invoke(fn,values){'use strict';for(let value of values)return fn();return 0;}",
    ];
    for source in cases {
        assert_eq!(tail_count(&compile(source, "invoke")), 0, "{source}");
    }
}

#[test]
fn catch_handlers_finalizers_and_for_in_bodies_admit_tail_transfers() {
    let cases = [
        "function invoke(fn){'use strict';try{throw 0;}catch(error){return fn();}}",
        "function invoke(fn){'use strict';try{}finally{return fn();}}",
        "function invoke(fn,object){'use strict';for(let key in object)return fn();return 0;}",
    ];
    for source in cases {
        assert_eq!(tail_count(&compile(source, "invoke")), 1, "{source}");
    }
}
