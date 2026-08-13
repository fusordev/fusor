use fusor_bytecode::{
    CompilerBindingKind, CompilerClosureBinding, CompilerExecutableKind, VerificationLimits,
};
use fusor_compiler::{CompilationContext, CompiledFunctionTree};
use fusor_frontend::{CompilationGoal, FrontendOptions, IndirectEvalGoal, with_parsed_program};

fn compile(source: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::IndirectEval(IndirectEvalGoal::new())),
        |unit| {
            CompilationContext::new(unit)
                .expect("indirect eval storage")
                .compile_indirect_eval_script(VerificationLimits::default())
                .expect("complete indirect eval authority")
        },
    )
    .expect("indirect eval frontend")
}

#[test]
fn sloppy_indirect_eval_certifies_only_var_like_globals() {
    let tree = compile(
        "var shared; function declared() {} let local = 1; const fixed = 2; local + fixed;",
    );
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.metadata().executable_kind(),
        CompilerExecutableKind::IndirectEvalScript
    );
    let global_kinds = root
        .metadata()
        .closures()
        .iter()
        .filter_map(|definition| match definition.binding() {
            CompilerClosureBinding::RealmGlobal(policy) => Some(policy.kind()),
            CompilerClosureBinding::Captured(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(global_kinds.contains(&CompilerBindingKind::Var));
    assert!(global_kinds.contains(&CompilerBindingKind::Function));
    assert!(!global_kinds.contains(&CompilerBindingKind::Let));
    assert!(!global_kinds.contains(&CompilerBindingKind::Const));
}

#[test]
fn strict_indirect_eval_certifies_declarations_as_frame_locals() {
    let tree = compile(
        "\"use strict\"; var localVar; function localFunction() {} let localLet; const localConst = 1; localConst;",
    );
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.metadata().executable_kind(),
        CompilerExecutableKind::IndirectEvalScript
    );
    assert!(
        root.function()
            .control_flow()
            .function_header()
            .mode()
            .is_strict()
    );
    assert!(root.metadata().closures().iter().all(|definition| {
        !matches!(
            definition.binding(),
            CompilerClosureBinding::RealmGlobal(policy)
                if matches!(
                    policy.kind(),
                    CompilerBindingKind::Var
                        | CompilerBindingKind::Function
                        | CompilerBindingKind::Let
                        | CompilerBindingKind::Const
                )
        )
    }));
}
