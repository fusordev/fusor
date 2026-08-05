use quickjs_bytecode::{
    CompilerBindingKind, CompilerClosureBinding, CompilerExecutableKind, FinalOpcode,
    VerificationLimits,
};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            CompilationContext::new(unit)
                .expect("Global Script storage")
                .compile_global_script(VerificationLimits::default())
                .expect("complete Global Script authority")
        },
    )
    .expect("Global Script frontend")
}

#[test]
fn global_script_certifies_object_and_lexical_bindings_in_one_graph() {
    let tree = compile(
        "let lexical = 1; const fixed = 2;\
         function read() { return lexical + fixed; } read();",
    );
    let authority = tree.verified_bytecode();

    assert_eq!(
        authority.root().metadata().executable_kind(),
        CompilerExecutableKind::GlobalScript
    );
    assert_eq!(authority.functions().len(), 2);
    let root = authority.root();
    let policies = root
        .metadata()
        .closures()
        .iter()
        .filter_map(|definition| match definition.binding() {
            CompilerClosureBinding::RealmGlobal(policy) => Some(policy.kind()),
            CompilerClosureBinding::Captured(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(policies.contains(&CompilerBindingKind::Let));
    assert!(policies.contains(&CompilerBindingKind::Const));
    assert!(policies.contains(&CompilerBindingKind::Function));

    let opcodes = root
        .function()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert_eq!(
        opcodes
            .iter()
            .filter(|opcode| **opcode == FinalOpcode::PutVarInit)
            .count(),
        2
    );
    assert!(opcodes.contains(&FinalOpcode::PutVar));
}
