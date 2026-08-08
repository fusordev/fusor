use quickjs_bytecode::{FinalOpcode, Operands, ScopeLink, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledFunction, CompiledFunctionTree};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("direct-eval storage");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("direct-eval lowering")
        },
    )
    .expect("front-end acceptance")
}

fn eval_operands(compiled: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .filter_map(|verified| {
            let instruction = verified.decoded().instruction();
            matches!(
                instruction.opcode(),
                FinalOpcode::Eval | FinalOpcode::ApplyEval
            )
            .then_some((instruction.opcode(), instruction.operands()))
        })
        .collect()
}

#[test]
fn bare_eval_uses_identity_checked_eval_even_when_lexically_shadowed() {
    let compiled = compile("function invoke(eval) { return eval('source'); }", "invoke");
    assert_eq!(
        eval_operands(compiled.root()),
        [(
            FinalOpcode::Eval,
            Operands::NPopU16 {
                argument_count: 1,
                scope_index: 1,
            },
        )]
    );
}

#[test]
fn optional_eval_call_remains_an_ordinary_indirect_call() {
    let compiled = compile(
        "function invoke(eval) { return eval?.('source'); }",
        "invoke",
    );
    assert!(eval_operands(compiled.root()).is_empty());
}

#[test]
fn eval_scope_head_selects_the_innermost_verified_lexical_chain() {
    let compiled = compile(
        "function invoke(eval) { let body = 1; { let outer = 2; { let inner = 3; return eval('inner'); } } }",
        "invoke",
    );
    assert_eq!(
        eval_operands(compiled.root()),
        [(
            FinalOpcode::Eval,
            Operands::NPopU16 {
                argument_count: 1,
                scope_index: 4,
            },
        )]
    );

    let variables = compiled.verified_bytecode().root().metadata().variables();
    assert_eq!(variables[3].scope_next(), ScopeLink::Local(1));
    assert_eq!(variables[2].scope_next(), ScopeLink::Local(0));
    assert_eq!(variables[1].scope_next(), ScopeLink::End);
}

#[test]
fn eval_in_parameter_expression_uses_argument_scope_sentinel() {
    let compiled = compile(
        "function invoke(eval, value = eval('source')) { return value; }",
        "invoke",
    );
    assert_eq!(
        eval_operands(compiled.root()),
        [(
            FinalOpcode::Eval,
            Operands::NPopU16 {
                argument_count: 1,
                scope_index: 0,
            },
        )]
    );
}

#[test]
fn spread_bare_eval_uses_apply_eval_without_a_receiver_slot() {
    let compiled = compile(
        "function invoke(eval, values) { return eval(...values); }",
        "invoke",
    );
    assert_eq!(
        eval_operands(compiled.root()),
        [(FinalOpcode::ApplyEval, Operands::U16(1))]
    );
    let opcodes = compiled
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|verified| verified.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    let apply = opcodes
        .iter()
        .position(|&opcode| opcode == FinalOpcode::ApplyEval)
        .expect("apply_eval instruction");
    assert_ne!(
        opcodes.get(apply.wrapping_sub(1)),
        Some(&FinalOpcode::Undefined)
    );
}
