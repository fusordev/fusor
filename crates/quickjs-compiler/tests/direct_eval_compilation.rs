use quickjs_bytecode::{
    CompilerClosureBinding, CompilerClosureSource, CompilerExecutableKind, VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, CompiledRealmGlobalSource, LeafCompilationError,
    UnsupportedLeafFeature,
};
use quickjs_frontend::{
    CompilationGoal, DirectEvalBinding, DirectEvalBindingKind, DirectEvalBindingLocation,
    DirectEvalBindingScope, DirectEvalCapabilities, DirectEvalContext, DirectEvalScopeFrame,
    DirectEvalScopeKind, DirectEvalScopeSnapshot, DirectEvalVariableEnvironment, FrontendOptions,
    with_parsed_program,
};

fn compile(
    source: &str,
    caller_strict: bool,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    compile_in_variable_environment(
        source,
        caller_strict,
        DirectEvalVariableEnvironment::Function,
    )
}

fn compile_in_variable_environment(
    source: &str,
    caller_strict: bool,
    variable_environment: DirectEvalVariableEnvironment,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    let context = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_strict(caller_strict),
        DirectEvalScopeSnapshot::default(),
    )
    .with_variable_environment(variable_environment);
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

fn compile_with_bindings(
    source: &str,
    caller_strict: bool,
    bindings: &[DirectEvalBinding<'_>],
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    compile_with_bindings_in_variable_environment(
        source,
        caller_strict,
        bindings,
        DirectEvalVariableEnvironment::Function,
    )
}

fn compile_with_bindings_in_variable_environment(
    source: &str,
    caller_strict: bool,
    bindings: &[DirectEvalBinding<'_>],
    variable_environment: DirectEvalVariableEnvironment,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    let frames = [DirectEvalScopeFrame::new(
        DirectEvalScopeKind::Pseudo,
        bindings,
        &[],
    )];
    let context = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_strict(caller_strict),
        DirectEvalScopeSnapshot::new(&frames),
    )
    .with_variable_environment(variable_environment);
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
        CompilerExecutableKind::DirectEvalScript
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
fn sloppy_direct_eval_function_environment_remains_fail_closed() {
    let error = compile("function answer() {} answer();", false)
        .expect_err("caller function declaration instantiation needs a shared environment");
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
fn sloppy_direct_eval_certifies_global_variable_environment_declarations() {
    let tree = compile_in_variable_environment(
        "var answer = 42; function read() { return answer; } read();",
        false,
        DirectEvalVariableEnvironment::Global,
    )
    .expect("global direct-eval declarations");
    let globals = tree.root().realm_globals();

    assert!(globals.iter().any(|global| {
        global.name() == "answer" && global.source() == CompiledRealmGlobalSource::ConstructorRealm
    }));
    assert!(globals.iter().any(|global| {
        global.name() == "read" && global.source() == CompiledRealmGlobalSource::ConstructorRealm
    }));
}

#[test]
fn sloppy_direct_eval_rejects_an_intervening_lexical_collision() {
    let bindings = [DirectEvalBinding::new(
        "collision",
        DirectEvalBindingKind::Normal,
        true,
        false,
        DirectEvalBindingLocation::Local { index: 0 },
    )
    .with_scope(DirectEvalBindingScope::Lexical)];
    let error = compile_with_bindings_in_variable_environment(
        "var collision;",
        false,
        &bindings,
        DirectEvalVariableEnvironment::Global,
    )
    .expect_err("caller lexical bindings reject sloppy eval var declarations");

    assert!(matches!(
        error,
        LeafCompilationError::EvalDeclarationConflict { ref name, .. }
            if name.as_ref() == "collision"
    ));
}

#[test]
fn sloppy_direct_eval_reuses_an_existing_function_variable_binding() {
    let bindings = [DirectEvalBinding::new(
        "answer",
        DirectEvalBindingKind::Normal,
        false,
        false,
        DirectEvalBindingLocation::Local { index: 2 },
    )
    .with_scope(DirectEvalBindingScope::Variable)];
    let tree = compile_with_bindings_in_variable_environment(
        "var answer = 42; answer;",
        false,
        &bindings,
        DirectEvalVariableEnvironment::Function,
    )
    .expect("existing function variable cell is the eval declaration target");
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.function().closure_sources(),
        [CompilerClosureSource::DirectEvalBinding {
            index: 0,
            environment_size: 1,
        }]
    );
    assert!(matches!(
        root.metadata().closures()[0].binding(),
        CompilerClosureBinding::Captured(_)
    ));
}

#[test]
fn sloppy_direct_eval_function_declaration_can_replace_an_existing_var_binding() {
    let bindings = [DirectEvalBinding::new(
        "answer",
        DirectEvalBindingKind::Normal,
        false,
        false,
        DirectEvalBindingLocation::Local { index: 2 },
    )
    .with_scope(DirectEvalBindingScope::Variable)];
    let tree = compile_with_bindings_in_variable_environment(
        "function answer() { return 42; } answer();",
        false,
        &bindings,
        DirectEvalVariableEnvironment::Function,
    )
    .expect("function declaration initializes the existing variable cell");
    let root = tree.verified_bytecode().root();

    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == quickjs_bytecode::FinalOpcode::PutVarRef)
    );
}

#[test]
fn direct_eval_resolves_caller_bindings_before_realm_globals() {
    let bindings = [
        DirectEvalBinding::new(
            "answer",
            DirectEvalBindingKind::Normal,
            false,
            false,
            DirectEvalBindingLocation::Local { index: 4 },
        ),
        DirectEvalBinding::new(
            "answer",
            DirectEvalBindingKind::Normal,
            true,
            true,
            DirectEvalBindingLocation::Closure { index: 1 },
        ),
    ];
    let tree = compile_with_bindings("answer + realmValue;", false, &bindings)
        .expect("caller capture and Realm-global fallback authority");
    let root = tree.verified_bytecode().root();
    assert_eq!(
        root.function().closure_sources(),
        [
            CompilerClosureSource::DirectEvalBinding {
                index: 0,
                environment_size: 2,
            },
            CompilerClosureSource::ConstructorRealmGlobal(
                root.metadata().closures()[1]
                    .name()
                    .expect("global name atom")
            ),
        ]
    );
    assert!(matches!(
        root.metadata().closures()[0].binding(),
        CompilerClosureBinding::Captured(_)
    ));
    assert!(matches!(
        root.metadata().closures()[1].binding(),
        CompilerClosureBinding::RealmGlobal(_)
    ));
}
