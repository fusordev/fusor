use std::fmt::Write as _;

use oxc_ast::ast::Statement;
use oxc_semantic::ScopeId;
use quickjs_bytecode::{
    AssemblerError, AssemblerResource, BytecodePc, FinalOpcode, Operands, VerificationLimits,
};
use quickjs_compiler::{CompilationContext, CompiledLeafFunction, LeafCompilationError};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    compile_with_limits(source, name, VerificationLimits::default())
        .expect("statement control-flow compilation must succeed")
}

fn compile_with_limits(
    source: &str,
    name: &str,
    limits: VerificationLimits,
) -> Result<CompiledLeafFunction, LeafCompilationError> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context.compile_leaf(&executable, limits)
        },
    )
    .expect("front-end acceptance")
}

fn compile_tree_root(source: &str, name: &str) -> CompiledLeafFunction {
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
                .compile_tree(&executable, VerificationLimits::default())
                .expect("statement control-flow tree compilation must succeed")
                .root()
                .clone()
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
fn debugger_statement_lowers_to_an_exact_source_mapped_nop() {
    let source = "function f(value){ debugger; return value; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::Nop, Operands::None),
            (BytecodePc::new(1), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(2), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(0)),
        "debugger;"
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn block_scope_tdz_entry_matches_the_quickjs_branch_oracle() {
    let source = "function f(a){ if(a){ let x=a; return x; } return a; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(10),
            ),
            (
                BytecodePc::new(3),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(6), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(7), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (
                BytecodePc::new(8),
                FinalOpcode::GetLocCheck,
                Operands::Loc(0),
            ),
            (BytecodePc::new(11), FinalOpcode::Return, Operands::None),
            (BytecodePc::new(12), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(13), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(3)), "x");
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn alternate_blocks_initialize_only_the_lexicals_in_the_entered_scope() {
    let source = "function f(a){ if(a){ let x=a; } else { let y=a; } return a; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(8),
            ),
            (
                BytecodePc::new(3),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(6), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(7), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (BytecodePc::new(8), FinalOpcode::Goto8, Operands::Label8(6)),
            (
                BytecodePc::new(10),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(1),
            ),
            (BytecodePc::new(13), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(14), FinalOpcode::PutLoc1, Operands::NoneLoc),
            (BytecodePc::new(15), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(16), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(3)), "x");
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(10)), "y");
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn each_scope_initializes_only_its_own_lexicals_in_reverse_slot_order() {
    let source = "function f(a){ let x; { let y; const z=a; } return x; }";
    let compiled = compile(source, "f");

    let entries = compiled
        .source_instructions()
        .iter()
        .filter(|entry| {
            compiled
                .control_flow()
                .instructions()
                .iter()
                .find(|instruction| instruction.decoded().pc() == entry.pc())
                .is_some_and(|instruction| {
                    instruction.decoded().instruction().opcode() == FinalOpcode::SetLocUninitialized
                })
        })
        .map(|entry| {
            let span = entry.span();
            (entry.pc(), &source[span.start as usize..span.end as usize])
        })
        .collect::<Vec<_>>();

    assert_eq!(
        entries,
        [
            (BytecodePc::new(0), "x"),
            (BytecodePc::new(5), "z"),
            (BytecodePc::new(8), "y"),
        ]
    );
}

#[test]
fn a_block_var_remains_function_scoped_without_a_tdz_reset() {
    let compiled = compile("function f(a){ { var x=a; } return x; }", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(1), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (BytecodePc::new(2), FinalOpcode::GetLoc0, Operands::NoneLoc),
            (BytecodePc::new(3), FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn while_reenters_the_body_scope_before_each_iteration() {
    let source = "function f(a){ while(a){ let x=a; } return a; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(8),
            ),
            (
                BytecodePc::new(3),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(6), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(7), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (BytecodePc::new(8), FinalOpcode::Goto8, Operands::Label8(-9)),
            (BytecodePc::new(10), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(11), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(3)), "x");
}

#[test]
fn do_while_reenters_the_body_scope_before_each_iteration() {
    let source = "function f(a){ do { let x=a; } while(a); return a; }";
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
            (BytecodePc::new(4), FinalOpcode::PutLoc0, Operands::NoneLoc),
            (BytecodePc::new(5), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(6),
                FinalOpcode::IfTrue8,
                Operands::Label8(-7),
            ),
            (BytecodePc::new(8), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(9), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(0)), "x");
}

#[test]
fn unlabeled_break_and_continue_target_the_innermost_loop() {
    let source = "function f(a){ while(a){ if(a) continue; break; } return a; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(10),
            ),
            (BytecodePc::new(3), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(4),
                FinalOpcode::IfFalse8,
                Operands::Label8(3),
            ),
            (BytecodePc::new(6), FinalOpcode::Goto8, Operands::Label8(-7)),
            (BytecodePc::new(8), FinalOpcode::Goto8, Operands::Label8(3)),
            (
                BytecodePc::new(10),
                FinalOpcode::Goto8,
                Operands::Label8(-11),
            ),
            (BytecodePc::new(12), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(13), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(6)),
        "continue;"
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(8)),
        "break;"
    );
    for pc in [0, 3, 6, 8, 12] {
        let instruction = compiled
            .control_flow()
            .instructions()
            .iter()
            .find(|instruction| instruction.decoded().pc() == BytecodePc::new(pc))
            .expect("statement boundary instruction");
        assert_eq!(
            instruction.entry_stack_depth(),
            Some(0),
            "statement boundary at PC {pc} has an empty operand stack"
        );
    }
}

#[test]
fn do_while_continue_targets_the_trailing_test_and_break_targets_the_exit() {
    let source = "function f(a,b){ do { if(a) continue; break; } while(b); return b; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(3),
            ),
            (BytecodePc::new(3), FinalOpcode::Goto8, Operands::Label8(3)),
            (BytecodePc::new(5), FinalOpcode::Goto8, Operands::Label8(4)),
            (BytecodePc::new(7), FinalOpcode::GetArg1, Operands::NoneArg),
            (
                BytecodePc::new(8),
                FinalOpcode::IfTrue8,
                Operands::Label8(-9),
            ),
            (BytecodePc::new(10), FinalOpcode::GetArg1, Operands::NoneArg),
            (BytecodePc::new(11), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(3)),
        "continue;"
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(5)),
        "break;"
    );
}

#[test]
fn classic_for_uses_one_iterative_test_body_rotation_update_cycle() {
    let source = "function f(n){ for(let i=0;i<n;i++){ if(i) continue; } return n; }";
    let compiled = compile(source, "f");
    let opcodes = decoded(&compiled)
        .into_iter()
        .map(|(_, opcode, _)| opcode)
        .collect::<Vec<_>>();

    assert_eq!(
        opcodes,
        [
            FinalOpcode::SetLocUninitialized,
            FinalOpcode::Push0,
            FinalOpcode::PutLoc0,
            FinalOpcode::GetLocCheck,
            FinalOpcode::GetArg0,
            FinalOpcode::Lt,
            FinalOpcode::IfFalse8,
            FinalOpcode::GetLocCheck,
            FinalOpcode::IfFalse8,
            FinalOpcode::Goto8,
            FinalOpcode::GetLocCheck,
            FinalOpcode::PostInc,
            FinalOpcode::PutLocCheck,
            FinalOpcode::Drop,
            FinalOpcode::Goto8,
            FinalOpcode::GetArg0,
            FinalOpcode::Return,
        ]
    );

    let continue_jump = compiled
        .control_flow()
        .instructions()
        .iter()
        .find(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::Goto8
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "continue;"
        })
        .expect("continue jump");
    let update = continue_jump
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("continue reaches the shared update");
    assert_eq!(
        update.decoded().instruction().opcode(),
        FinalOpcode::GetLocCheck
    );
    assert_eq!(
        source_slice_at(&compiled, source, update.decoded().pc()),
        "i"
    );
}

#[test]
fn classic_for_supports_omitted_clauses_and_break() {
    let source = "function f(){ let i=0; for(;;i++){ if(i) break; } return i; }";
    let compiled = compile(source, "f");
    let instructions = compiled.control_flow().instructions();

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::PostInc
            })
            .count(),
        1
    );
    let break_jump = instructions
        .iter()
        .find(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::Goto8
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "break;"
        })
        .expect("break jump");
    let exit = break_jump
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("break reaches the loop exit");
    assert_eq!(
        exit.decoded().instruction().opcode(),
        FinalOpcode::GetLocCheck
    );
    assert_eq!(source_slice_at(&compiled, source, exit.decoded().pc()), "i");
}

#[test]
fn nested_loops_select_the_nearest_break_and_continue_targets() {
    let source = "function f(a,b){ while(a){ while(b){ break; } continue; } return a; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(12),
            ),
            (BytecodePc::new(3), FinalOpcode::GetArg1, Operands::NoneArg),
            (
                BytecodePc::new(4),
                FinalOpcode::IfFalse8,
                Operands::Label8(5),
            ),
            (BytecodePc::new(6), FinalOpcode::Goto8, Operands::Label8(3)),
            (BytecodePc::new(8), FinalOpcode::Goto8, Operands::Label8(-6)),
            (
                BytecodePc::new(10),
                FinalOpcode::Goto8,
                Operands::Label8(-11),
            ),
            (
                BytecodePc::new(12),
                FinalOpcode::Goto8,
                Operands::Label8(-13),
            ),
            (BytecodePc::new(14), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(15), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(6)),
        "break;"
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(10)),
        "continue;"
    );
}

#[test]
fn a_late_lexical_is_tdz_initialized_when_its_block_is_entered() {
    let source = "function f(a){ if(a){ return x; let x=a; } return a; }";
    let compiled = compile(source, "f");
    let instructions = decoded(&compiled);

    let initialization = instructions
        .iter()
        .position(|(_, opcode, _)| *opcode == FinalOpcode::SetLocUninitialized)
        .expect("block TDZ initialization");
    let read = instructions
        .iter()
        .position(|(_, opcode, _)| *opcode == FinalOpcode::GetLocCheck)
        .expect("checked lexical read");
    assert!(initialization < read);
    assert_eq!(
        source_slice_at(&compiled, source, instructions[initialization].0),
        "x"
    );
    assert_eq!(
        source_slice_at(&compiled, source, instructions[read].0),
        "x"
    );
}

#[test]
fn unreachable_statements_are_validated_and_receive_a_structural_terminal() {
    let compiled = compile("function f(a){ return a; a; }", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(1), FinalOpcode::Return, Operands::None),
            (BytecodePc::new(2), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(3), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(4), FinalOpcode::ReturnUndef, Operands::None),
        ]
    );
    assert_eq!(
        compiled.control_flow().instructions()[2].entry_stack_depth(),
        None
    );
}

#[test]
fn a_join_after_two_returning_branches_is_backed_by_a_real_terminal() {
    let compiled = compile("function f(a,b){ if(a) return b; else return a; }", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(5),
            ),
            (BytecodePc::new(3), FinalOpcode::GetArg1, Operands::NoneArg),
            (BytecodePc::new(4), FinalOpcode::Return, Operands::None),
            (BytecodePc::new(5), FinalOpcode::Goto8, Operands::Label8(3)),
            (BytecodePc::new(7), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(8), FinalOpcode::Return, Operands::None),
            (BytecodePc::new(9), FinalOpcode::ReturnUndef, Operands::None),
        ]
    );
    assert_eq!(
        compiled.control_flow().instructions()[4].entry_stack_depth(),
        None
    );
    assert_eq!(
        compiled.control_flow().instructions()[7].entry_stack_depth(),
        None
    );
}

#[test]
fn labeled_break_and_continue_select_the_named_iteration() {
    let source = "function f(a,b){ outer: while(a){ while(b){ if(a) continue outer; break outer; } } return b; }";
    let compiled = compile(source, "f");
    let instructions = compiled.control_flow().instructions();

    let continue_jump = instructions
        .iter()
        .find(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "continue outer;"
        })
        .expect("labeled continue jump");
    assert_eq!(
        continue_jump.decoded().instruction().opcode(),
        FinalOpcode::Goto8
    );
    let continue_target = continue_jump
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("labeled continue target");
    assert_eq!(
        source_slice_at(&compiled, source, continue_target.decoded().pc()),
        "a"
    );

    let break_jump = instructions
        .iter()
        .find(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "break outer;"
        })
        .expect("labeled break jump");
    assert_eq!(
        break_jump.decoded().instruction().opcode(),
        FinalOpcode::Goto8
    );
    let break_target = break_jump
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("labeled break target");
    assert_eq!(
        source_slice_at(&compiled, source, break_target.decoded().pc()),
        "b"
    );
}

#[test]
fn chained_labels_share_the_iteration_continue_target() {
    let source = "function f(a){ first: second: while(a){ if(a) continue first; \
                  break second; } return false; }";
    let compiled = compile(source, "f");
    let instructions = compiled.control_flow().instructions();

    let continue_jump = instructions
        .iter()
        .find(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "continue first;"
        })
        .expect("outer chained continue jump");
    let continue_target = continue_jump
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("shared loop continue target");
    assert_eq!(
        source_slice_at(&compiled, source, continue_target.decoded().pc()),
        "a"
    );

    let break_jump = instructions
        .iter()
        .find(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "break second;"
        })
        .expect("inner chained break jump");
    let break_target = break_jump
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("shared loop break target");
    assert_eq!(
        source_slice_at(&compiled, source, break_target.decoded().pc()),
        "false"
    );
    assert_eq!(
        break_target.decoded().instruction().opcode(),
        FinalOpcode::PushFalse
    );
    assert_ne!(continue_target.decoded().pc(), break_target.decoded().pc());
}

#[test]
fn direct_labels_attach_to_do_while_and_classic_for_iterations() {
    for (source, jump_source) in [
        (
            "function f(a){ repeat: do { if(a) continue repeat; break repeat; } while(a); return a; }",
            "continue repeat;",
        ),
        (
            "function f(a){ repeat: for(;a;a=false){ continue repeat; } return a; }",
            "continue repeat;",
        ),
    ] {
        let compiled = compile(source, "f");
        let jump = compiled
            .control_flow()
            .instructions()
            .iter()
            .find(|instruction| {
                source_slice_at(&compiled, source, instruction.decoded().pc()) == jump_source
            })
            .expect("labeled iteration jump");
        assert_eq!(jump.decoded().instruction().opcode(), FinalOpcode::Goto8);
        assert!(jump.successors().jump_target().is_some());
    }
}

#[test]
fn breaking_a_regular_label_closes_nested_captured_scopes() {
    let source = "function outer(value){ \"use strict\"; let saved; target: { let current=value; function capture(){return current;} saved=capture; break target; } return saved; }";
    let compiled = compile_tree_root(source, "outer");
    let instructions = compiled.control_flow().instructions();

    let break_index = instructions
        .iter()
        .position(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "break target;"
        })
        .expect("regular labeled break");
    assert_eq!(
        instructions[break_index - 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::CloseLoc
    );
    assert_eq!(
        instructions[break_index].decoded().instruction().opcode(),
        FinalOpcode::Goto8
    );
    assert_eq!(
        source_slice_at(
            &compiled,
            source,
            instructions[break_index - 1].decoded().pc()
        ),
        "current"
    );
    let exit = instructions[break_index]
        .successors()
        .jump_target()
        .and_then(|target| compiled.control_flow().instruction(target))
        .expect("regular label exit");
    assert_eq!(
        source_slice_at(&compiled, source, exit.decoded().pc()),
        "saved"
    );
}

#[test]
fn switch_dispatch_uses_strict_equality_and_enters_bodies_at_depth_zero() {
    let source = "function f(value){ switch(value){ case 1: return 10; default: return 30; case 2: return 20; } }";
    let compiled = compile(source, "f");
    let instructions = compiled.control_flow().instructions();

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::Dup
            })
            .count(),
        2
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::StrictEq
            })
            .count(),
        2
    );
    for expected in ["10", "30", "20"] {
        let entry = instructions
            .iter()
            .find(|instruction| {
                source_slice_at(&compiled, source, instruction.decoded().pc()) == expected
            })
            .expect("case body instruction");
        assert_eq!(entry.entry_stack_depth(), Some(0));
    }
    assert_eq!(compiled.control_flow().computed_stack_size(), 3);
}

#[test]
fn switch_scope_is_entered_after_the_discriminant_and_before_case_tests() {
    let source = "function f(value){ switch(value){ case local: let local=1; return local; } }";
    let compiled = compile(source, "f");
    let instructions = compiled.control_flow().instructions();

    let discriminant = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::GetArg0
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "value"
        })
        .expect("switch discriminant");
    let tdz = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::SetLocUninitialized
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "local"
        })
        .expect("switch lexical TDZ initialization");
    let case_test = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::GetLocCheck
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "local"
        })
        .expect("case test lexical read");

    assert!(discriminant < tdz);
    assert!(tdz < case_test);
}

#[test]
fn switch_scaffold_checks_the_instruction_budget_before_label_planning() {
    let source = "function f(value){ switch(value){ case 0: case 1: } }";
    let error = compile_with_limits(
        source,
        "f",
        VerificationLimits::new(1_000, 1, 0, 0, 100, 10),
    )
    .expect_err("switch scaffold must exceed the one-instruction budget");

    let LeafCompilationError::BytecodeAssembly {
        span: Some(span),
        source:
            AssemblerError::LimitExceeded {
                resource,
                instruction_index,
                limit,
                observed,
            },
    } = error
    else {
        panic!("switch scaffold must fail with the assembler instruction limit");
    };
    assert_eq!(resource, AssemblerResource::Instructions);
    assert_eq!(instruction_index, 1);
    assert_eq!(limit, 1);
    assert_eq!(observed, 2);
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "switch(value){ case 0: case 1: }"
    );
}

#[test]
fn a_mutated_out_of_range_block_scope_fails_as_a_typed_invariant() {
    let source = "function f(a){ { let x=a; } return a; }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("named function executable");
            let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                panic!("function declaration");
            };
            let body = function.body.as_ref().expect("function body");
            let Statement::BlockStatement(block) = &body.statements[0] else {
                panic!("nested block");
            };
            block.scope_id.set(Some(ScopeId::new(
                unit.semantic().scoping().scopes_len() + 1,
            )));

            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect_err("mutated scope identity must fail closed")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::SemanticInvariant { invariant, span } = error else {
        panic!("mutated scope must produce a semantic invariant");
    };
    assert_eq!(invariant, "Oxc scope identity indexes retained semantics");
    let span = span.expect("block span");
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "{ let x=a; }"
    );
}

#[test]
fn deeply_nested_blocks_lower_without_recursive_statement_planning() {
    const BLOCK_COUNT: usize = 1_024;
    let mut source = String::with_capacity(32 + 2 * BLOCK_COUNT);
    source.push_str("function f(a){");
    for _ in 0..BLOCK_COUNT {
        source.push('{');
    }
    source.push_str("return a;");
    for _ in 0..BLOCK_COUNT {
        source.push('}');
    }
    source.push('}');

    let compiled = compile(&source, "f");
    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(1), FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn deeply_chained_labels_lower_without_recursive_statement_planning() {
    const LABEL_COUNT: usize = 1_024;
    let mut source = String::from("function f(a){");
    for index in 0..LABEL_COUNT {
        write!(&mut source, "label{index}:").expect("writing to a string is infallible");
    }
    source.push_str("while(a){break label0;}return a;}");

    let compiled = compile(&source, "f");
    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                source_slice_at(&compiled, &source, instruction.decoded().pc()) == "break label0;"
            })
    );
}

#[test]
fn repeated_jumps_to_the_innermost_chained_label_use_indexed_resolution() {
    const LABEL_COUNT: usize = 2_048;
    const JUMP_COUNT: usize = 2_048;
    let mut source = String::from("function f(a){");
    for index in 0..LABEL_COUNT {
        write!(&mut source, "label{index}:").expect("writing to a string is infallible");
    }
    source.push_str("while(a){");
    for _ in 0..JUMP_COUNT {
        write!(&mut source, "if(a)continue label{};", LABEL_COUNT - 1)
            .expect("writing to a string is infallible");
    }
    source.push_str("break;}return false;}");

    let compiled = compile(&source, "f");
    assert_eq!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| {
                source_slice_at(&compiled, &source, instruction.decoded().pc())
                    .starts_with("continue label")
            })
            .count(),
        JUMP_COUNT
    );
}

#[test]
fn deeply_nested_switches_lower_without_recursive_statement_planning() {
    const SWITCH_COUNT: usize = 256;
    let mut source = String::from("function f(a){");
    for _ in 0..SWITCH_COUNT {
        source.push_str("switch(a){case 0:");
    }
    source.push_str("return a;");
    for _ in 0..SWITCH_COUNT {
        source.push('}');
    }
    source.push('}');

    let compiled = compile(&source, "f");
    assert_eq!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::StrictEq
            })
            .count(),
        SWITCH_COUNT
    );
}
