use std::error::Error as _;

use quickjs_bytecode::{
    AssemblerError, AssemblerResource, BytecodePc, EncodeError, FinalOpcode, Operands,
    VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledLeafFunction, LeafCompilationError, UnsupportedLeafFeature,
};
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
                .expect("control-flow expression compilation must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
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
                .expect_err("unsupported expression must fail closed")
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

fn opcodes(compiled: &CompiledLeafFunction) -> Vec<FinalOpcode> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
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
fn mutable_identifier_assignment_compound_and_update_preserve_values() {
    let compiled = compile("function f(a){ let i=0; i=a; i+=2; ++i; return i++; }", "f");

    assert_eq!(
        opcodes(&compiled),
        [
            FinalOpcode::SetLocUninitialized,
            FinalOpcode::Push0,
            FinalOpcode::PutLoc0,
            FinalOpcode::GetArg0,
            FinalOpcode::SetLocCheck,
            FinalOpcode::Drop,
            FinalOpcode::GetLocCheck,
            FinalOpcode::Push2,
            FinalOpcode::Add,
            FinalOpcode::SetLocCheck,
            FinalOpcode::Drop,
            FinalOpcode::GetLocCheck,
            FinalOpcode::Inc,
            FinalOpcode::SetLocCheck,
            FinalOpcode::Drop,
            FinalOpcode::GetLocCheck,
            FinalOpcode::PostInc,
            FinalOpcode::PutLocCheck,
            FinalOpcode::Return,
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
}

#[test]
fn logical_assignment_uses_one_value_preserving_join() {
    let compiled = compile("function f(a,b){ a ||= b; return a; }", "f");
    let instructions = compiled.control_flow().instructions();
    let branch = instructions
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::IfTrue8)
        .expect("logical assignment branch");
    let target = branch
        .successors()
        .branch_target()
        .expect("taken short-circuit target");
    let target = compiled
        .control_flow()
        .instruction(target)
        .expect("verified logical assignment target");

    assert_eq!(
        opcodes(&compiled),
        [
            FinalOpcode::GetArg0,
            FinalOpcode::Dup,
            FinalOpcode::IfTrue8,
            FinalOpcode::Drop,
            FinalOpcode::GetArg1,
            FinalOpcode::SetArg0,
            FinalOpcode::Drop,
            FinalOpcode::GetArg0,
            FinalOpcode::Return,
        ]
    );
    assert_eq!(target.decoded().instruction().opcode(), FinalOpcode::Drop);
}

#[test]
fn and_and_nullish_assignment_use_their_distinct_short_circuit_tests() {
    let and = compile("function and(a,b){ a &&= b; return a; }", "and");
    assert_eq!(
        opcodes(&and),
        [
            FinalOpcode::GetArg0,
            FinalOpcode::Dup,
            FinalOpcode::IfFalse8,
            FinalOpcode::Drop,
            FinalOpcode::GetArg1,
            FinalOpcode::SetArg0,
            FinalOpcode::Drop,
            FinalOpcode::GetArg0,
            FinalOpcode::Return,
        ]
    );

    let nullish = compile("function nullish(a,b){ a ??= b; return a; }", "nullish");
    assert_eq!(
        opcodes(&nullish),
        [
            FinalOpcode::GetArg0,
            FinalOpcode::Dup,
            FinalOpcode::IsUndefinedOrNull,
            FinalOpcode::IfFalse8,
            FinalOpcode::Drop,
            FinalOpcode::GetArg1,
            FinalOpcode::SetArg0,
            FinalOpcode::Drop,
            FinalOpcode::GetArg0,
            FinalOpcode::Return,
        ]
    );
}

#[test]
fn immutable_identifier_mutation_fails_closed_at_the_target() {
    let source = "function f(){ const x=1; x++; }";
    let error = compile_error(source, "f");
    let LeafCompilationError::Unsupported { feature, span } = error else {
        panic!("const mutation must fail closed");
    };

    assert_eq!(feature, UnsupportedLeafFeature::UnsupportedReference);
    assert_eq!(&source[span.start as usize..span.end as usize], "x");
}

#[test]
fn conditional_expression_matches_the_quickjs_final_branch_oracle() {
    let compiled = compile("function f(a,b,c){ let x = a ? b : c; return x; }", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (
                BytecodePc::new(0),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(3), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(4),
                FinalOpcode::IfFalse8,
                Operands::Label8(4),
            ),
            (BytecodePc::new(6), FinalOpcode::GetArg1, Operands::NoneArg),
            (BytecodePc::new(7), FinalOpcode::Goto8, Operands::Label8(2),),
            (BytecodePc::new(9), FinalOpcode::GetArg2, Operands::NoneArg),
            (BytecodePc::new(10), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (
                BytecodePc::new(11),
                FinalOpcode::GetLocCheck,
                Operands::Loc(0),
            ),
            (BytecodePc::new(14), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn logical_operators_preserve_the_selected_operand_value() {
    let cases = [
        (
            "function f(a,b){ let x=a&&b; return x; }",
            FinalOpcode::IfFalse8,
            false,
        ),
        (
            "function f(a,b){ let x=a||b; return x; }",
            FinalOpcode::IfTrue8,
            false,
        ),
        (
            "function f(a,b){ let x=a??b; return x; }",
            FinalOpcode::IfFalse8,
            true,
        ),
    ];

    for (source, branch, nullish) in cases {
        let compiled = compile(source, "f");
        let mut expected = vec![
            FinalOpcode::SetLocUninitialized,
            FinalOpcode::GetArg0,
            FinalOpcode::Dup,
        ];
        if nullish {
            expected.push(FinalOpcode::IsUndefinedOrNull);
        }
        expected.extend([
            branch,
            FinalOpcode::Drop,
            FinalOpcode::GetArg1,
            FinalOpcode::PutLoc0,
            FinalOpcode::GetLocCheck,
            FinalOpcode::Return,
        ]);
        assert_eq!(opcodes(&compiled), expected, "{source}");
        assert_eq!(compiled.control_flow().computed_stack_size(), 2, "{source}");

        let branch = decoded(&compiled)
            .into_iter()
            .find(|(_, opcode, _)| matches!(opcode, FinalOpcode::IfFalse8 | FinalOpcode::IfTrue8))
            .expect("short-circuit branch");
        assert_eq!(branch.2, Operands::Label8(3), "{source}");
    }
}

#[test]
fn natural_same_operator_left_chains_share_one_quickjs_join() {
    let compiled = compile("function f(a,b,c){ return a&&b&&c; }", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(1), FinalOpcode::Dup, Operands::None),
            (
                BytecodePc::new(2),
                FinalOpcode::IfFalse8,
                Operands::Label8(8),
            ),
            (BytecodePc::new(4), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(5), FinalOpcode::GetArg1, Operands::NoneArg),
            (BytecodePc::new(6), FinalOpcode::Dup, Operands::None),
            (
                BytecodePc::new(7),
                FinalOpcode::IfFalse8,
                Operands::Label8(3),
            ),
            (BytecodePc::new(9), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(10), FinalOpcode::GetArg2, Operands::NoneArg),
            (BytecodePc::new(11), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
}

#[test]
fn parenthesized_left_logical_boundaries_keep_the_intermediate_test() {
    let compiled = compile("function f(a,b,c){ return (a&&b)&&c; }", "f");
    assert_eq!(
        decoded(&compiled)
            .into_iter()
            .filter_map(|(_, opcode, operands)| {
                (opcode == FinalOpcode::IfFalse8).then_some(operands)
            })
            .collect::<Vec<_>>(),
        [Operands::Label8(3), Operands::Label8(3)]
    );
}

#[test]
fn nested_control_flow_has_verified_equal_depth_joins_and_relocated_sources() {
    let source = "function f(a,b,c){ let x = a ? (b && c) : (b ?? c); return x || a; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (
                BytecodePc::new(0),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(3), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(4),
                FinalOpcode::IfFalse8,
                Operands::Label8(9),
            ),
            (BytecodePc::new(6), FinalOpcode::GetArg1, Operands::NoneArg),
            (BytecodePc::new(7), FinalOpcode::Dup, Operands::None),
            (
                BytecodePc::new(8),
                FinalOpcode::IfFalse8,
                Operands::Label8(3),
            ),
            (BytecodePc::new(10), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(11), FinalOpcode::GetArg2, Operands::NoneArg),
            (BytecodePc::new(12), FinalOpcode::Goto8, Operands::Label8(8)),
            (BytecodePc::new(14), FinalOpcode::GetArg1, Operands::NoneArg),
            (BytecodePc::new(15), FinalOpcode::Dup, Operands::None),
            (
                BytecodePc::new(16),
                FinalOpcode::IsUndefinedOrNull,
                Operands::None,
            ),
            (
                BytecodePc::new(17),
                FinalOpcode::IfFalse8,
                Operands::Label8(3),
            ),
            (BytecodePc::new(19), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(20), FinalOpcode::GetArg2, Operands::NoneArg),
            (BytecodePc::new(21), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (
                BytecodePc::new(22),
                FinalOpcode::GetLocCheck,
                Operands::Loc(0),
            ),
            (BytecodePc::new(25), FinalOpcode::Dup, Operands::None),
            (
                BytecodePc::new(26),
                FinalOpcode::IfTrue8,
                Operands::Label8(3),
            ),
            (BytecodePc::new(28), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(29), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(30), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
    for join_pc in [12, 21, 30] {
        let join = compiled
            .control_flow()
            .instructions()
            .iter()
            .find(|instruction| instruction.decoded().pc() == BytecodePc::new(join_pc))
            .expect("verified join instruction");
        assert_eq!(join.entry_stack_depth(), Some(1), "join at PC {join_pc}");
    }
    assert_eq!(
        compiled
            .source_instructions()
            .iter()
            .map(|entry| entry.pc())
            .collect::<Vec<_>>(),
        compiled
            .control_flow()
            .instructions()
            .iter()
            .map(|instruction| instruction.decoded().pc())
            .collect::<Vec<_>>()
    );
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(4)), "a");
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(8)), "b");
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(12)),
        "a ? (b && c) : (b ?? c)"
    );
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(17)), "b");
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(26)), "x");
}

#[test]
fn unsupported_short_circuit_paths_are_still_prevalidated_with_exact_spans() {
    let source = "function f(){ return false && \"constant pool\"; }";
    let error = compile_error(source, "f");
    let LeafCompilationError::Unsupported { feature, span } = error else {
        panic!("expected unsupported literal");
    };
    assert_eq!(feature, UnsupportedLeafFeature::UnsupportedLiteral);
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "\"constant pool\""
    );
}

#[test]
fn relaxed_branch_byte_limits_map_back_to_the_owning_source_span() {
    let source = "function f(a,b,c){ return a ? b : c; }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("function executable");
            context
                .compile_leaf(&executable, VerificationLimits::new(4, 20, 0, 0, 100, 20))
                .expect_err("the relaxed goto exceeds four bytes")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::BytecodeEncoding {
        span,
        source:
            EncodeError::ByteLimitExceeded {
                pc,
                instruction_size,
                encoded_bytes,
                byte_limit,
            },
    } = &error
    else {
        panic!("expected byte-limit encoding error, got {error:?}");
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "a ? b : c");
    assert_eq!(*pc, BytecodePc::new(4));
    assert_eq!(*instruction_size, 2);
    assert_eq!(*encoded_bytes, 4);
    assert_eq!(*byte_limit, 4);
    assert!(error.source().is_some());
}

#[test]
fn relaxation_work_limits_fail_before_verification_with_an_exact_source_span() {
    let source = "function f(a,b,c){ return a ? b : c; }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("function executable");
            context
                .compile_leaf(&executable, VerificationLimits::new(100, 20, 0, 0, 1, 20))
                .expect_err("the six-instruction relaxation pass exceeds one")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::BytecodeAssembly {
        span: Some(span),
        source:
            AssemblerError::LimitExceeded {
                resource: AssemblerResource::RelaxationEvaluations,
                instruction_index,
                limit,
                observed,
            },
    } = &error
    else {
        panic!("expected relaxation work-limit error, got {error:?}");
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "a");
    assert_eq!(*instruction_index, 1);
    assert_eq!(*limit, 1);
    assert_eq!(*observed, 2);
    assert!(error.source().is_some());
}

#[test]
fn deeply_nested_logical_lowering_is_iterative() {
    const OPERATOR_COUNT: usize = 20_000;
    let mut source =
        String::with_capacity("function f(operand){return operand;}".len() + 10 * OPERATOR_COUNT);
    source.push_str("function f(operand){return operand");
    for _ in 0..OPERATOR_COUNT {
        source.push_str("&&operand");
    }
    source.push_str(";}");

    let compiled = compile(&source, "f");
    assert_eq!(
        compiled.control_flow().instructions().len(),
        4 * OPERATOR_COUNT + 2
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
}
