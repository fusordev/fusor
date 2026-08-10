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
                scope_index: 5,
            },
        )]
    );

    let variables = compiled.verified_bytecode().root().metadata().variables();
    assert!(variables[1].is_arguments_object());
    assert_eq!(variables[4].scope_next(), ScopeLink::Local(2));
    assert_eq!(variables[3].scope_next(), ScopeLink::Local(1));
    assert_eq!(variables[2].scope_next(), ScopeLink::End);
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
    let eval_instruction = compiled
        .root()
        .control_flow()
        .instructions()
        .iter()
        .position(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::Eval)
        .expect("parameter eval instruction");
    let boundary = compiled
        .root()
        .parameter_initialization_end()
        .expect("parameter/body boundary");
    assert!(u32::try_from(eval_instruction).expect("instruction index") < boundary);
    assert_eq!(
        compiled
            .verified_bytecode()
            .root()
            .function()
            .parameter_initialization_end(),
        Some(boundary)
    );
}

#[test]
fn direct_eval_arguments_prelude_precedes_function_instantiation_initializers() {
    let compiled = compile(
        "function invoke(eval){function nested(){}eval('nested');return nested;}",
        "invoke",
    );
    let root = compiled.root();
    let boundary = root.function_initializer_prefix_start();
    assert_eq!(root.parameter_initialization_end(), None);
    assert_eq!(
        compiled
            .verified_bytecode()
            .root()
            .function()
            .function_initializer_prefix_start(),
        boundary
    );
    let instructions = root.control_flow().instructions();
    let arguments_object = instructions
        .iter()
        .position(|instruction| {
            let instruction = instruction.decoded().instruction();
            instruction.opcode() == FinalOpcode::SpecialObject
                && instruction.operands() == Operands::U8(1)
        })
        .expect("direct eval retains an arguments object");
    let function_initializer = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::FClosure8 | FinalOpcode::FClosure
            )
        })
        .expect("nested declaration initializer");

    assert!(u32::try_from(arguments_object).expect("instruction index") < boundary);
    assert!(boundary <= u32::try_from(function_initializer).expect("instruction index"));
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

#[test]
fn eval_resolved_through_with_retains_its_reference_receiver() {
    let compiled = compile(
        "function invoke(object, eval) { with (object) return eval('value'); }",
        "invoke",
    );
    let instructions = compiled.root().control_flow().instructions();
    let eval = instructions
        .iter()
        .position(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::Eval)
        .expect("eval instruction");

    assert_eq!(
        instructions[eval].decoded().instruction().operands(),
        Operands::NPopU16 {
            argument_count: 1,
            scope_index: 3,
        }
    );
    assert_eq!(
        instructions[eval + 1].decoded().instruction().opcode(),
        FinalOpcode::Swap
    );
    assert_eq!(
        instructions[eval + 2].decoded().instruction().opcode(),
        FinalOpcode::Drop
    );
    assert_eq!(
        compiled
            .verified_bytecode()
            .root()
            .function()
            .eval_reference_call_instructions(),
        [u32::try_from(eval).expect("instruction index")]
    );
}

#[test]
fn captured_compound_assignment_retains_its_reference_across_direct_eval() {
    let compiled = compile(
        "function testCompoundAssignment(eval){var x=3;return function(){x*=(eval('var x=2;'),4);return x;}();}",
        "testCompoundAssignment",
    );
    let opcodes = compiled
        .verified_bytecode()
        .functions()
        .map(|function| {
            function
                .function()
                .control_flow()
                .instructions()
                .iter()
                .map(|instruction| instruction.decoded().instruction().opcode())
                .collect::<Vec<_>>()
        })
        .find(|opcodes| opcodes.contains(&FinalOpcode::MakeVarRefRef))
        .expect("nested assignment retains a captured reference");

    let position = |opcode| {
        opcodes
            .iter()
            .position(|&candidate| candidate == opcode)
            .expect("reference transaction opcode")
    };
    let make = position(FinalOpcode::MakeVarRefRef);
    let get = position(FinalOpcode::GetRefValue);
    let insert = position(FinalOpcode::Insert3);
    let put = position(FinalOpcode::PutRefValue);

    assert_eq!(get, make + 1);
    assert!(get < insert);
    assert_eq!(put, insert + 1);
    assert!(!opcodes.iter().any(|opcode| matches!(
        opcode,
        FinalOpcode::PutVarRef
            | FinalOpcode::PutVarRef0
            | FinalOpcode::PutVarRef1
            | FinalOpcode::PutVarRef2
            | FinalOpcode::PutVarRef3
    )));
}

#[test]
fn captured_lexical_compound_reference_retains_tdz_metadata() {
    let compiled = compile(
        "function outer(eval){let value=1;return function(){value+=(eval(''),2);return value;}();}",
        "outer",
    );

    assert!(compiled.verified_bytecode().functions().any(|function| {
        function
            .function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::MakeVarRefRef
            })
    }));
}
