use quickjs_bytecode::{CompilerExecutableKind, VerificationLimits};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, LeafCompilationError, UnsupportedLeafFeature,
};
use quickjs_frontend::{
    CompilationGoal, DirectEvalCapabilities, DirectEvalContext, DirectEvalScopeSnapshot,
    FrontendOptions, with_parsed_program,
};

fn compile(
    source: &str,
    caller_strict: bool,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    let context = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_strict(caller_strict),
        DirectEvalScopeSnapshot::default(),
    );
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::DirectEval(context)),
        |unit| {
            CompilationContext::new(unit)
                .expect("direct eval storage")
                .compile_direct_eval_script(VerificationLimits::default())
        },
    )
    .expect("direct eval frontend")
}

#[test]
fn closed_sloppy_direct_eval_certifies_eval_local_lexicals() {
    let tree = compile("let answer = 40 + 2; answer;", false)
        .expect("closed sloppy direct eval authority");
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.metadata().executable_kind(),
        CompilerExecutableKind::IndirectEvalScript
    );
    assert!(
        !root
            .function()
            .control_flow()
            .function_header()
            .mode()
            .is_strict()
    );
    assert!(root.metadata().closures().is_empty());
}

#[test]
fn caller_strictness_makes_direct_eval_var_declarations_local() {
    let tree =
        compile("var answer = 40 + 2; answer;", true).expect("closed strict direct eval authority");
    let root = tree.verified_bytecode().root();

    assert!(
        root.function()
            .control_flow()
            .function_header()
            .mode()
            .is_strict()
    );
    assert!(root.metadata().closures().is_empty());
}

#[test]
fn sloppy_direct_eval_var_environment_remains_fail_closed() {
    let error = compile("var answer = 42; answer;", false)
        .expect_err("caller variable-environment mutation needs an external environment");
    assert!(
        matches!(
            error,
            LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::DirectEvalVariableEnvironment,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn direct_eval_caller_and_global_resolution_remain_fail_closed() {
    let error = compile("answer;", false)
        .expect_err("caller and global resolution need an external environment");
    assert!(matches!(
        error,
        LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::UnresolvedReference,
            ..
        }
    ));
}
