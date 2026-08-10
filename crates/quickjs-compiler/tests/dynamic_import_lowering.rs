use quickjs_bytecode::{FinalOpcode, Operands, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledFunction, CompiledLeafFunction};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_leaf(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("dynamic import lowering")
        },
    )
    .expect("frontend")
}

fn instructions(function: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
    function
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
fn dynamic_import_evaluates_specifier_then_options_before_import() {
    let compiled = compile_leaf(
        "function load(specifier, options) { return import(specifier, options); }",
        "load",
    );

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Import, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn dynamic_import_supplies_undefined_when_options_are_absent() {
    let compiled = compile_leaf(
        "function load(specifier) { return import(specifier); }",
        "load",
    );

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Import, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}
