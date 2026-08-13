use fusor_bytecode::{
    CompilerBindingKind, CompilerClosureBinding, CompilerExecutableKind, FinalOpcode,
    VerificationLimits,
};
use fusor_compiler::{CompilationContext, CompiledFunctionTree};
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

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

#[test]
fn sloppy_delete_distinguishes_lexical_and_object_global_names() {
    let lexical = compile("let fixed; delete fixed;");
    let lexical_opcodes = lexical
        .verified_bytecode()
        .root()
        .function()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(lexical_opcodes.contains(&FinalOpcode::PushFalse));
    assert!(!lexical_opcodes.contains(&FinalOpcode::DeleteVar));

    for (source, expected_kind) in [
        ("var present; delete present;", CompilerBindingKind::Var),
        ("delete missing;", CompilerBindingKind::GlobalReference),
    ] {
        let compiled = compile(source);
        let root = compiled.verified_bytecode().root();
        let opcodes = root
            .function()
            .control_flow()
            .instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>();
        assert!(opcodes.contains(&FinalOpcode::DeleteVar));
        assert!(root.metadata().closures().iter().any(|definition| {
            matches!(
                definition.binding(),
                CompilerClosureBinding::RealmGlobal(policy)
                    if policy.kind() == expected_kind
            )
        }));
    }
}

#[test]
fn global_script_certifies_const_for_of_destructuring_iteration_heads() {
    let tree = compile(
        "const entries = [[1, 2], [3, 4]];\
         for (const [first] of entries) { (() => first)(); }",
    );
    let root = tree.verified_bytecode().root();
    assert_eq!(
        root.metadata().executable_kind(),
        CompilerExecutableKind::GlobalScript
    );
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::ForOfStart)
    );
}

#[test]
fn global_script_allows_looped_destructuring_reassignment_of_a_block_let() {
    let tree = compile(
        "for (const pair of [[[1], [2]]]) {\
             let [value] = pair[0];\
             [value] = pair[1];\
             value;\
         }",
    );
    assert_eq!(
        tree.verified_bytecode().root().metadata().executable_kind(),
        CompilerExecutableKind::GlobalScript
    );
}
