use quickjs_bytecode::{BytecodePc, FinalOpcode, Operands, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledLeafFunction};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("direct-call compilation must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn decoded(compiled: &CompiledLeafFunction) -> Vec<(BytecodePc, FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let decoded = instruction.decoded();
            (
                decoded.pc(),
                decoded.instruction().opcode(),
                decoded.instruction().operands(),
            )
        })
        .collect()
}

fn source_slice_at<'source>(
    compiled: &CompiledLeafFunction,
    source: &'source str,
    pc: BytecodePc,
) -> &'source str {
    let span = compiled
        .source_instructions()
        .iter()
        .find(|entry| entry.pc() == pc)
        .expect("source entry at final instruction PC")
        .span();
    &source[span.start as usize..span.end as usize]
}

#[test]
fn direct_calls_use_quickjs_short_and_wide_argument_forms() {
    let cases = [
        (
            "function invoke(fn){return fn();}",
            FinalOpcode::Call0,
            Operands::NPopX,
            1,
        ),
        (
            "function invoke(fn,a){return fn(a);}",
            FinalOpcode::Call1,
            Operands::NPopX,
            2,
        ),
        (
            "function invoke(fn,a,b){return fn(a,b);}",
            FinalOpcode::Call2,
            Operands::NPopX,
            3,
        ),
        (
            "function invoke(fn,a,b,c){return fn(a,b,c);}",
            FinalOpcode::Call3,
            Operands::NPopX,
            4,
        ),
        (
            "function invoke(fn,a,b,c,d){return fn(a,b,c,d);}",
            FinalOpcode::Call,
            Operands::NPop { argument_count: 4 },
            5,
        ),
    ];

    for (source, expected_opcode, expected_operands, expected_stack_size) in cases {
        let compiled = compile(source, "invoke");
        let calls = decoded(&compiled)
            .into_iter()
            .filter(|(_, opcode, _)| {
                matches!(
                    opcode,
                    FinalOpcode::Call
                        | FinalOpcode::Call0
                        | FinalOpcode::Call1
                        | FinalOpcode::Call2
                        | FinalOpcode::Call3
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(calls.len(), 1, "{source}");
        assert_eq!(calls[0].1, expected_opcode, "{source}");
        assert_eq!(calls[0].2, expected_operands, "{source}");
        assert_eq!(
            compiled.control_flow().computed_stack_size(),
            expected_stack_size,
            "{source}"
        );
        let call_start = source.rfind("fn(").expect("call expression start");
        let call_end = source.rfind(';').expect("return statement terminator");
        assert_eq!(
            source_slice_at(&compiled, source, calls[0].0),
            &source[call_start..call_end]
        );
    }
}

#[test]
fn nested_calls_evaluate_each_callee_before_arguments_from_left_to_right() {
    let source = "function invoke(target,first,second,third,fourth){\
        return target(first(),second(third()),fourth);\
    }";
    let compiled = compile(source, "invoke");
    let instructions = decoded(&compiled);

    assert_eq!(
        instructions
            .iter()
            .map(|(_, opcode, operands)| (*opcode, *operands))
            .collect::<Vec<_>>(),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::GetArg2, Operands::NoneArg),
            (FinalOpcode::GetArg3, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::Call1, Operands::NPopX),
            (FinalOpcode::GetArg, Operands::Arg(4)),
            (FinalOpcode::Call3, Operands::NPopX),
            (FinalOpcode::Return, Operands::None),
        ]
    );

    let call_sources = instructions
        .iter()
        .filter(|(_, opcode, _)| {
            matches!(
                opcode,
                FinalOpcode::Call
                    | FinalOpcode::Call0
                    | FinalOpcode::Call1
                    | FinalOpcode::Call2
                    | FinalOpcode::Call3
            )
        })
        .map(|(pc, _, _)| source_slice_at(&compiled, source, *pc))
        .collect::<Vec<_>>();
    assert_eq!(
        call_sources,
        [
            "first()",
            "third()",
            "second(third())",
            "target(first(),second(third()),fourth)",
        ]
    );
}

#[test]
fn supported_parenthesized_and_conditional_callees_remain_expressions() {
    let source =
        "function invoke(condition,left,right,value){return (condition?left:right)(value);}";
    let compiled = compile(source, "invoke");
    let instructions = decoded(&compiled);

    assert!(
        instructions
            .iter()
            .any(|(_, opcode, operands)| *opcode == FinalOpcode::Call1
                && *operands == Operands::NPopX)
    );
    let call = instructions
        .iter()
        .find(|(_, opcode, _)| *opcode == FinalOpcode::Call1)
        .expect("direct call");
    assert_eq!(
        source_slice_at(&compiled, source, call.0),
        "(condition?left:right)(value)"
    );
}

#[test]
fn constructor_calls_duplicate_the_callee_as_new_target_before_arguments() {
    let source = "function construct(Ctor,first,second){return new Ctor(first(),second);}";
    let compiled = compile(source, "construct");
    let instructions = decoded(&compiled);

    assert_eq!(
        instructions
            .iter()
            .map(|(_, opcode, operands)| (*opcode, *operands))
            .collect::<Vec<_>>(),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::GetArg2, Operands::NoneArg),
            (
                FinalOpcode::CallConstructor,
                Operands::NPop { argument_count: 2 },
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 4);

    let constructor = instructions
        .iter()
        .find(|(_, opcode, _)| *opcode == FinalOpcode::CallConstructor)
        .expect("constructor call");
    assert_eq!(
        source_slice_at(&compiled, source, constructor.0),
        "new Ctor(first(),second)"
    );
}

#[test]
fn constructor_calls_support_empty_arguments_and_static_member_callees() {
    let source = "function construct(holder,value){new holder.Ctor;return new holder.Ctor(value);}";
    let compiled = compile(source, "construct");
    let instructions = decoded(&compiled);

    assert_eq!(
        instructions
            .iter()
            .filter(|(_, opcode, _)| *opcode == FinalOpcode::CallConstructor)
            .map(|(_, opcode, operands)| (*opcode, *operands))
            .collect::<Vec<_>>(),
        [
            (
                FinalOpcode::CallConstructor,
                Operands::NPop { argument_count: 0 },
            ),
            (
                FinalOpcode::CallConstructor,
                Operands::NPop { argument_count: 1 },
            ),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 3);

    let constructor_sources = instructions
        .iter()
        .filter(|(_, opcode, _)| *opcode == FinalOpcode::CallConstructor)
        .map(|(pc, _, _)| source_slice_at(&compiled, source, *pc))
        .collect::<Vec<_>>();
    assert_eq!(
        constructor_sources,
        ["new holder.Ctor", "new holder.Ctor(value)"]
    );
}

#[test]
fn computed_member_calls_keep_the_receiver_and_evaluate_arguments_after_lookup() {
    let source = "function invoke(holder,key,first,second){return (holder[key])(first(),second);}";
    let compiled = compile(source, "invoke");
    let instructions = decoded(&compiled);

    assert_eq!(
        instructions
            .iter()
            .map(|(_, opcode, operands)| (*opcode, *operands))
            .collect::<Vec<_>>(),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::GetArrayEl2, Operands::None),
            (FinalOpcode::GetArg2, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::GetArg3, Operands::NoneArg),
            (
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count: 2 },
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 4);

    let lookup = instructions
        .iter()
        .find(|(_, opcode, _)| *opcode == FinalOpcode::GetArrayEl2)
        .expect("computed receiver lookup");
    assert_eq!(source_slice_at(&compiled, source, lookup.0), "holder[key]");
    let call = instructions
        .iter()
        .find(|(_, opcode, _)| *opcode == FinalOpcode::CallMethod)
        .expect("receiver-aware call");
    assert_eq!(
        source_slice_at(&compiled, source, call.0),
        "(holder[key])(first(),second)"
    );
}

#[test]
fn spread_calls_and_constructors_lower_to_the_pinned_apply_stack_program() {
    let cases = [
        (
            "function invoke(fn,values){return fn(...values);}",
            vec![
                (FinalOpcode::GetArg0, Operands::NoneArg),
                (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::GetArg1, Operands::NoneArg),
                (FinalOpcode::Append, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::Swap, Operands::None),
                (FinalOpcode::Apply, Operands::U16(0)),
            ],
        ),
        (
            "function invoke(fn,values){return new fn(...values);}",
            vec![
                (FinalOpcode::GetArg0, Operands::NoneArg),
                (FinalOpcode::Dup, Operands::None),
                (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::GetArg1, Operands::NoneArg),
                (FinalOpcode::Append, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Perm3, Operands::None),
                (FinalOpcode::Apply, Operands::U16(1)),
            ],
        ),
        (
            "function invoke(fn,first,rest){return fn(first,...rest);}",
            vec![
                (FinalOpcode::GetArg0, Operands::NoneArg),
                (FinalOpcode::GetArg1, Operands::NoneArg),
                (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::GetArg2, Operands::NoneArg),
                (FinalOpcode::Append, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::Swap, Operands::None),
                (FinalOpcode::Apply, Operands::U16(0)),
            ],
        ),
    ];

    for (source, expected) in cases {
        let compiled = with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage planning must succeed");
                let executable = context
                    .executables()
                    .find(|executable| executable.metadata().name() == Some("invoke"))
                    .expect("named function executable");
                context
                    .compile_leaf(&executable, VerificationLimits::default())
                    .expect("spread call must lower")
            },
        )
        .expect("front-end acceptance");
        let actual: Vec<(FinalOpcode, Operands)> = decoded(&compiled)
            .into_iter()
            .map(|(_, opcode, operands)| (opcode, operands))
            .filter(|(opcode, _)| *opcode != FinalOpcode::Return)
            .collect();
        assert_eq!(actual, expected, "{source}");
    }
}
