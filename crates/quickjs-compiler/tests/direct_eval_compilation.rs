use quickjs_bytecode::{
    CompilerClosureBinding, CompilerClosureSource, CompilerExecutableKind, FinalOpcode, Operands,
    VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, CompiledRealmGlobalSource, DeclarationKind,
    LeafCompilationError,
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

fn compile_with_capabilities(
    source: &str,
    capabilities: DirectEvalCapabilities,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    let context = DirectEvalContext::new(capabilities, DirectEvalScopeSnapshot::default());
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::DirectEval(context)),
        |unit| {
            CompilationContext::new(unit)
                .expect("contextual direct eval storage")
                .compile_direct_eval_script(VerificationLimits::default())
        },
    )
    .expect("contextual direct eval frontend")
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
fn direct_eval_keeps_named_class_binding_in_tdz_through_computed_names() {
    let tree = compile("class EvalDeclaration { [EvalDeclaration]() {} }", false)
        .expect("named class direct-eval authority");
    let root = tree.root();
    let binding = root
        .storage_plan()
        .bindings_for(root.executable())
        .expect("direct-eval root bindings")
        .iter()
        .find(|binding| binding.policy().kind() == DeclarationKind::ClassName)
        .expect("synthetic class-name binding");
    let slot = root
        .locals()
        .iter()
        .find(|local| local.binding() == binding.id())
        .expect("class-name local")
        .slot()
        .index();
    let initialization = match slot {
        0 => (FinalOpcode::PutLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::PutLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::PutLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::PutLoc3, Operands::NoneLoc),
        slot => (
            FinalOpcode::PutLoc8,
            Operands::Loc8(u8::try_from(slot).expect("small direct-eval local layout")),
        ),
    };
    let instructions = root
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();
    let computed_name_read = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::GetLocCheck, Operands::Loc(slot)))
        .expect("computed name reads the uninitialized inner binding");
    let class_name_initialization = instructions
        .iter()
        .position(|instruction| *instruction == initialization)
        .expect("class constructor initializes the inner binding");

    assert!(computed_name_read < class_name_initialization);
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
fn direct_eval_certifies_inherited_new_target_authority() {
    let tree = compile_with_capabilities(
        "new.target;",
        DirectEvalCapabilities::new().with_new_target(true),
    )
    .expect("contextual new.target direct eval authority");
    let root = tree.verified_bytecode().root();
    assert!(
        root.function()
            .control_flow()
            .function_header()
            .flags()
            .new_target_allowed()
    );
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                let instruction = instruction.decoded().instruction();
                instruction.opcode() == FinalOpcode::SpecialObject
                    && instruction.operands() == Operands::U8(3)
            })
    );
}

#[test]
fn direct_eval_certifies_inherited_super_property_authority() {
    let tree = compile_with_capabilities(
        "super.answer;",
        DirectEvalCapabilities::new().with_super_property(true),
    )
    .expect("contextual super property direct eval authority");
    let root = tree.verified_bytecode().root();
    assert!(
        root.function()
            .control_flow()
            .function_header()
            .flags()
            .super_allowed()
    );
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                let instruction = instruction.decoded().instruction();
                instruction.opcode() == FinalOpcode::SpecialObject
                    && instruction.operands() == Operands::U8(5)
            })
    );
}

#[test]
fn direct_eval_certifies_inherited_derived_constructor_authority() {
    let tree = compile_with_capabilities(
        "super();",
        DirectEvalCapabilities::new()
            .with_new_target(true)
            .with_super_call(true),
    )
    .expect("contextual super call direct eval authority");
    let root = tree.verified_bytecode().root();
    let header = root.function().control_flow().function_header();
    assert!(header.flags().new_target_allowed());
    assert!(header.flags().super_call_allowed());
    for selector in [3, 4] {
        assert!(
            root.function()
                .control_flow()
                .instructions()
                .iter()
                .any(|instruction| {
                    let instruction = instruction.decoded().instruction();
                    instruction.opcode() == FinalOpcode::SpecialObject
                        && instruction.operands() == Operands::U8(selector)
                })
        );
    }
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::CheckCtorReturn
            })
    );
}

#[test]
fn direct_eval_certifies_contextual_instance_element_initialization() {
    let tree = compile_with_capabilities(
        "super();",
        DirectEvalCapabilities::new()
            .with_new_target(true)
            .with_super_call(true)
            .with_instance_elements(true),
    )
    .expect("contextual instance-element direct eval authority");
    let root = tree.verified_bytecode().root();
    assert!(
        root.function()
            .control_flow()
            .function_header()
            .flags()
            .direct_eval_has_instance_elements()
    );
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .windows(7)
            .any(|instructions| {
                let expected = [
                    (FinalOpcode::CheckCtorReturn, Operands::None),
                    (FinalOpcode::SpecialObject, Operands::U8(6)),
                    (FinalOpcode::PushThis, Operands::None),
                    (FinalOpcode::Swap, Operands::None),
                    (
                        FinalOpcode::CallMethod,
                        Operands::NPop { argument_count: 0 },
                    ),
                    (FinalOpcode::Drop, Operands::None),
                    (FinalOpcode::Drop, Operands::None),
                ];
                instructions
                    .iter()
                    .zip(expected)
                    .all(|(instruction, (opcode, operands))| {
                        let instruction = instruction.decoded().instruction();
                        instruction.opcode() == opcode && instruction.operands() == operands
                    })
            })
    );
}

#[test]
fn sloppy_direct_eval_certifies_a_new_function_variable_binding() {
    let tree = compile("var answer = 42; answer;", false)
        .expect("new caller variable-environment binding authority");
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.function().closure_sources(),
        [CompilerClosureSource::DirectEvalVariable {
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
fn sloppy_direct_eval_certifies_a_new_function_declaration_binding() {
    let tree = compile("function answer() {} answer();", false)
        .expect("new caller function declaration binding authority");
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.function().closure_sources(),
        [CompilerClosureSource::DirectEvalVariable {
            index: 0,
            environment_size: 1,
        }]
    );
    assert!(
        root.function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::PutVarRef)
    );
    assert!(root.metadata().closures()[0].is_deletable_eval_variable());
}

#[test]
fn sloppy_direct_eval_certifies_deletable_variables_through_child_closures() {
    let tree = compile(
        "delete answer;let read=function(){answer;};var answer;",
        false,
    )
    .expect("deletable direct-eval binding authority");
    assert!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::DeleteVar
            })
    );
    for function in [
        tree.verified_bytecode().root(),
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("child closure"),
    ] {
        assert!(function.metadata().closures()[0].is_deletable_eval_variable());
    }
}

#[test]
fn sloppy_direct_eval_certifies_a_parameter_initializer_variable() {
    let tree = compile_in_variable_environment(
        "var answer = 42; answer;",
        false,
        DirectEvalVariableEnvironment::FunctionParameterInitializer,
    )
    .expect("parameter-initializer eval targets the caller callee environment");
    assert_eq!(
        tree.verified_bytecode().root().function().closure_sources(),
        [CompilerClosureSource::DirectEvalVariable {
            index: 0,
            environment_size: 1,
        }]
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
fn sloppy_direct_eval_crosses_a_matching_catch_parameter() {
    let bindings = [DirectEvalBinding::new(
        "err",
        DirectEvalBindingKind::Catch,
        true,
        false,
        DirectEvalBindingLocation::Local { index: 0 },
    )
    .with_scope(DirectEvalBindingScope::Lexical)];
    let tree = compile_with_bindings_in_variable_environment(
        "function err() {}\
         function* err() {}\
         async function err() {}\
         async function* err() {}\
         var err;\
         for (var err; false; ) {}\
         for (var err in []) {}\
         for (var err of []) {}",
        false,
        &bindings,
        DirectEvalVariableEnvironment::Global,
    )
    .expect("Annex B catch parameters do not reject matching eval declarations");
    let globals = tree.root().realm_globals();

    assert!(globals.iter().any(|global| {
        global.name() == "err" && global.source() == CompiledRealmGlobalSource::ConstructorRealm
    }));
    assert!(globals.iter().any(|global| {
        global.name() == "err"
            && global.source()
                == CompiledRealmGlobalSource::DirectEvalBinding {
                    index: 0,
                    environment_size: 1,
                }
    }));
}

#[test]
fn matching_catch_parameter_does_not_hide_an_outer_lexical_eval_conflict() {
    let bindings = [
        DirectEvalBinding::new(
            "err",
            DirectEvalBindingKind::Catch,
            true,
            false,
            DirectEvalBindingLocation::Local { index: 0 },
        )
        .with_scope(DirectEvalBindingScope::Lexical),
        DirectEvalBinding::new(
            "err",
            DirectEvalBindingKind::Normal,
            true,
            false,
            DirectEvalBindingLocation::Local { index: 1 },
        )
        .with_scope(DirectEvalBindingScope::Lexical),
    ];
    let error = compile_with_bindings("var err;", false, &bindings)
        .expect_err("an outer lexical environment still rejects the eval var");

    assert!(matches!(
        error,
        LeafCompilationError::EvalDeclarationConflict { ref name, .. }
            if name.as_ref() == "err"
    ));
}

#[test]
fn sloppy_direct_eval_catch_collision_imports_distinct_write_and_declaration_cells() {
    let bindings = [DirectEvalBinding::new(
        "err",
        DirectEvalBindingKind::Catch,
        true,
        false,
        DirectEvalBindingLocation::Local { index: 0 },
    )
    .with_scope(DirectEvalBindingScope::Lexical)];
    let tree = compile_with_bindings("var err = 42; err;", false, &bindings)
        .expect("catch-visible eval var initializer");
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.function().closure_sources(),
        [
            CompilerClosureSource::DirectEvalVariable {
                index: 1,
                environment_size: 2,
            },
            CompilerClosureSource::DirectEvalBinding {
                index: 0,
                environment_size: 2,
            },
        ]
    );
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
fn sloppy_direct_eval_combines_existing_and_new_function_variable_bindings() {
    let bindings = [DirectEvalBinding::new(
        "existing",
        DirectEvalBindingKind::Normal,
        false,
        false,
        DirectEvalBindingLocation::Local { index: 2 },
    )
    .with_scope(DirectEvalBindingScope::Variable)];
    let tree = compile_with_bindings_in_variable_environment(
        "var existing = 1; var created = 2; existing + created;",
        false,
        &bindings,
        DirectEvalVariableEnvironment::Function,
    )
    .expect("combined caller and created variable environment");
    let root = tree.verified_bytecode().root();

    assert_eq!(
        root.function().closure_sources(),
        [
            CompilerClosureSource::DirectEvalBinding {
                index: 0,
                environment_size: 2,
            },
            CompilerClosureSource::DirectEvalVariable {
                index: 1,
                environment_size: 2,
            },
        ]
    );
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
                == FinalOpcode::PutVarRef)
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
