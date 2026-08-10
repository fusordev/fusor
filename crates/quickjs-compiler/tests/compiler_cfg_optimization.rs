use quickjs_bytecode::{FinalOpcode, VerificationLimits};
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
                .expect("named function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("verified compilation")
        },
    )
    .expect("frontend acceptance")
}

fn is_conditional(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 | FinalOpcode::IfTrue | FinalOpcode::IfTrue8
    )
}

#[test]
fn constant_pool_numbers_strings_and_bigints_fold_with_es_truthiness() {
    let source = r#"
        function choose(value) {
            if (0.5) value = 1; else value = 2;
            if ("truthy") value = 3; else value = 4;
            if (123456789012345678901234567890n) value = 5; else value = 6;
            return value;
        }
    "#;
    let compiled = compile(source, "choose");

    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .all(|verified| !is_conditional(verified.decoded().instruction().opcode()))
    );
    assert_eq!(
        compiled.source_instructions().len(),
        compiled.control_flow().instructions().len()
    );
}

#[test]
fn unknown_parameter_truthiness_remains_a_verified_two_edge_branch() {
    let compiled = compile(
        "function choose(value) { if (value) return 1; return 2; }",
        "choose",
    );

    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .any(|verified| is_conditional(verified.decoded().instruction().opcode()))
    );
}
