use quickjs_bytecode::{FinalOpcode, Operands, VerificationLimits};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, LeafCompilationError, UnsupportedLeafFeature,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("ordinary synchronous for-of lowering")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect_err("unsupported for-of head must fail closed")
        },
    )
    .expect("front-end acceptance")
}

fn opcodes(compiled: &CompiledFunctionTree) -> Vec<(FinalOpcode, Operands)> {
    compiled
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

#[test]
fn ordinary_head_families_use_the_exact_three_slot_protocol() {
    let compiled = compile(
        "function heads(values,target,key,assigned){\
            for(var variable of values){}\
            for(let lexical of values){}\
            for(const constant of values){}\
            for(assigned of values){}\
            for(target.value of values){}\
            for(target[key] of values){}\
        }",
        "heads",
    );
    let instructions = opcodes(&compiled);

    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
            .count(),
        6
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| **instruction == (FinalOpcode::ForOfNext, Operands::U8(0)))
            .count(),
        6
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::IteratorClose)
            .count(),
        6
    );
    assert!(compiled.root().control_flow().computed_stack_size() >= 6);
}

#[test]
fn return_closes_nested_iterators_inner_first_without_losing_the_value() {
    let compiled = compile(
        "function returned(outer,inner){\
            for(const left of outer){\
                for(const right of inner)return left+right;\
            }\
        }",
        "returned",
    );
    let instructions = opcodes(&compiled);
    let returned = instructions
        .iter()
        .position(|(opcode, _)| *opcode == FinalOpcode::Return)
        .expect("return opcode");
    assert!(returned >= 8);
    assert_eq!(
        instructions[returned - 8..returned],
        [
            (FinalOpcode::NipCatch, Operands::None),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::IteratorClose, Operands::None),
            (FinalOpcode::NipCatch, Operands::None),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::IteratorClose, Operands::None),
        ]
    );
}

#[test]
fn captured_head_keeps_quickjs_scope_close_edge_ordering() {
    let returned = compile(
        "function returned(values,hooks){\
            for(let value of values){\
                hooks.close=function close(){return value;};\
                return value;\
            }\
        }",
        "returned",
    );
    let returned = opcodes(&returned);
    let start = returned
        .iter()
        .position(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
        .expect("for-of start");
    let return_index = returned
        .iter()
        .position(|(opcode, _)| *opcode == FinalOpcode::Return)
        .expect("captured return");
    assert_eq!(
        returned[return_index - 4..return_index],
        [
            (FinalOpcode::NipCatch, Operands::None),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::IteratorClose, Operands::None),
        ]
    );
    assert!(
        returned[start + 1..return_index - 4]
            .iter()
            .all(|(opcode, _)| *opcode != FinalOpcode::CloseLoc),
        "return must keep the captured iteration cell attached while IteratorClose runs"
    );
    let exhausted_close = returned
        .iter()
        .enumerate()
        .skip(return_index + 1)
        .find(|(_, (opcode, _))| *opcode == FinalOpcode::IteratorClose)
        .map(|(index, _)| index)
        .expect("shared exhaustion close");
    assert_eq!(
        returned[exhausted_close + 1].0,
        FinalOpcode::CloseLoc,
        "natural exhaustion detaches the captured cell after IteratorClose"
    );

    let broken = opcodes(&compile(
        "function broken(values,hooks){\
            for(let value of values){\
                hooks.close=function close(){return value;};\
                break;\
            }\
        }",
        "broken",
    ));
    assert!(broken.windows(2).any(|window| {
        window[0].0 == FinalOpcode::CloseLoc
            && matches!(
                window[1].0,
                FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
            )
    }));
    assert!(broken.windows(2).any(|window| {
        window[0].0 == FinalOpcode::IteratorClose && window[1].0 == FinalOpcode::CloseLoc
    }));

    let thrown = opcodes(&compile(
        "function thrown(values,hooks){\
            for(let value of values){\
                hooks.close=function close(){return value;};\
                throw hooks.close();\
            }\
        }",
        "thrown",
    ));
    let start = thrown
        .iter()
        .position(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
        .expect("throwing for-of start");
    let thrown_at = thrown
        .iter()
        .position(|(opcode, _)| *opcode == FinalOpcode::Throw)
        .expect("throw opcode");
    assert!(thrown[start + 1..thrown_at].iter().all(|(opcode, _)| {
        !matches!(opcode, FinalOpcode::CloseLoc | FinalOpcode::IteratorClose)
    }));
}

#[test]
fn labels_captures_and_finally_share_the_existing_cleanup_stack() {
    let compiled = compile(
        "function controlled(outer,inner,stop){\
            let saved;\
            outerLoop:for(let left of outer){\
                saved=function capture(){return left;};\
                try{\
                    for(const right of inner){\
                        if(stop)return left+right;\
                        if(right)continue outerLoop;\
                        break outerLoop;\
                    }\
                }finally{stop;}\
            }\
            return saved;\
        }",
        "controlled",
    );
    let instructions = opcodes(&compiled);

    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
            .count(),
        2
    );
    assert!(
        instructions
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::CloseLoc)
    );
    assert!(
        instructions
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::Gosub)
    );
    assert!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::IteratorClose)
            .count()
            >= 4
    );
}

#[test]
fn destructuring_heads_emit_the_nested_verified_record_shape() {
    let compiled = compile(
        "function declared(values){\
            for(const [value] of values){}\
            for(const {x} of values){}\
        }",
        "declared",
    );
    let instructions = opcodes(&compiled);
    // Each destructuring head opens its own nested iterator record on the
    // loop value: two loops plus the array-pattern head's nested record.
    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
            .count(),
        3
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::ForOfNext)
            .count(),
        3
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::ToObject)
            .count(),
        1
    );

    let assigned = compile(
        "function assigned(values,value){for([value] of values){}}",
        "assigned",
    );
    let instructions = opcodes(&assigned);
    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
            .count(),
        2
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::IteratorClose)
            .count(),
        2
    );
}

#[test]
fn for_await_remains_typed_fail_closed_at_the_async_function() {
    let source = "async function awaited(values){for await(const value of values){}}";
    let LeafCompilationError::Unsupported { feature, span } = compile_error(source, "awaited")
    else {
        panic!("for-await must remain outside the ordinary synchronous function family");
    };
    assert_eq!(feature, UnsupportedLeafFeature::UnsupportedBody);
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "for await(const value of values){}"
    );
}
