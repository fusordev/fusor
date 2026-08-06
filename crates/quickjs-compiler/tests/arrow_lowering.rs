use quickjs_bytecode::{CompilerExecutableKind, FinalOpcode, FunctionKind, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_tree(source: &str, root_name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("named root function");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect("arrow function tree must compile")
        },
    )
    .expect("front-end acceptance")
}

#[test]
fn concise_arrow_body_has_lexical_nonconstructable_authority() {
    let tree = compile_tree(
        "function outer(){var inferred=value=>value+1;return inferred;}",
        "outer",
    );
    assert_eq!(tree.functions().len(), 2);

    let arrow = tree
        .verified_bytecode()
        .functions()
        .nth(1)
        .expect("arrow template");
    assert_eq!(
        arrow.metadata().executable_kind(),
        CompilerExecutableKind::OrdinaryArrow
    );
    assert_eq!(
        arrow.function().control_flow().function_header().kind(),
        FunctionKind::Normal
    );
    assert!(
        !arrow
            .function()
            .control_flow()
            .function_header()
            .flags()
            .has_prototype()
    );
    assert!(
        arrow
            .function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::Return
            })
    );
    assert_eq!(
        arrow.metadata().source().function_source(),
        "value=>value+1"
    );
}

#[test]
fn block_arrow_body_uses_ordinary_return_completion() {
    let tree = compile_tree(
        "function outer(){return value=>{if(value)return value;};}",
        "outer",
    );
    let arrow = tree
        .verified_bytecode()
        .functions()
        .nth(1)
        .expect("arrow template");
    let opcodes = arrow
        .function()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::Return));
    assert_eq!(opcodes.last(), Some(&FinalOpcode::ReturnUndef));
}
