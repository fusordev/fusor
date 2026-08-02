use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, BytecodeGraphVerificationLimits, ClosureVariableDefinition,
    CompilerAtom, CompilerBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerCapturedBinding, CompilerClosureSource, CompilerConstantLayout, CompilerExecutableKind,
    CompilerInitializationPolicy, CompilerSource, CompilerString, CompilerWritePolicy, FinalOpcode,
    FunctionGraphVerificationLimits, FunctionIndexDomains, FunctionTemplateId, Operands,
    PcSourceSpan, ScopeLink, SourceByteSpan, UnverifiedCompilerBytecodeGraph,
    UnverifiedCompilerFunction, UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph,
    UnverifiedFunctionHeader, UnverifiedFunctionMetadata, VariableDefinition, VerificationLimits,
    VerifiedBytecode, verify_compiler_bytecode_graph, verify_compiler_control_flow,
    verify_compiler_function_graph,
};
use quickjs_runtime::{
    DynamicFunctionScriptError, ExecutionError, ExecutionLimits, JsNumber, Runtime, RuntimeLimits,
    RuntimeResource, ValueKind,
};

fn atom(text: &str) -> CompilerAtom {
    CompilerAtom::new(
        CompilerString::try_from_code_units(text.encode_utf16().collect::<Vec<_>>().into())
            .expect("fixture atom"),
    )
}

fn policy(kind: CompilerBindingKind) -> CompilerBindingPolicy {
    match kind {
        CompilerBindingKind::Var => CompilerBindingPolicy::new(
            kind,
            CompilerInitializationPolicy::UndefinedAtInstantiation,
            CompilerWritePolicy::Mutable,
            false,
        ),
        CompilerBindingKind::GlobalReference => CompilerBindingPolicy::new(
            kind,
            CompilerInitializationPolicy::ConstructorRealmLookup,
            CompilerWritePolicy::Mutable,
            false,
        ),
        CompilerBindingKind::Function => CompilerBindingPolicy::new(
            kind,
            CompilerInitializationPolicy::FunctionAtInstantiation,
            CompilerWritePolicy::Mutable,
            false,
        ),
        CompilerBindingKind::Parameter
        | CompilerBindingKind::FunctionName
        | CompilerBindingKind::Catch
        | CompilerBindingKind::Let
        | CompilerBindingKind::Const => panic!("unsupported fixture policy"),
    }
}

fn realm_global_authority(
    name: &str,
    kind: CompilerBindingKind,
    instructions: &[(FinalOpcode, Operands)],
) -> Arc<VerifiedBytecode> {
    let mut builder = BytecodeBuilder::new();
    for &(opcode, operands) in instructions {
        builder.push(opcode, operands).expect("fixture instruction");
    }
    let flow = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                builder.into_bytes(),
                FunctionIndexDomains::new(1, 0, 0, 0, 1),
                UnverifiedFunctionHeader::dynamic_function_script(0),
            )
            .with_capture_layout(CompilerCaptureLayout::new(Arc::from([])))
            .with_constant_layout(CompilerConstantLayout::new(Arc::from([]))),
            VerificationLimits::default(),
        )
        .expect("realm-global flow"),
    );
    let source = CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(0));
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([UnverifiedCompilerFunction::new(
                    Arc::clone(&flow),
                    Arc::from([]),
                    Arc::from([source]),
                )
                .with_atom_pool(Arc::from([atom(name)]))]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("realm-global graph"),
    );
    let text: Arc<str> = Arc::from(name);
    let span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mappings = Arc::from(
        flow.instructions()
            .iter()
            .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), span))
            .collect::<Vec<_>>(),
    );
    let metadata = UnverifiedFunctionMetadata::new(
        None,
        Arc::from([]),
        Arc::from([ClosureVariableDefinition::realm_global(
            Some(AtomPoolIndex::new(0)),
            policy(kind),
            source,
        )]),
        CompilerSource::new(Arc::from("realm-global.js"), text, span, None, mappings),
    )
    .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript);
    Arc::new(
        verify_compiler_bytecode_graph(
            UnverifiedCompilerBytecodeGraph::new(graph, Arc::from([metadata])),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("realm-global authority"),
    )
}

#[allow(clippy::too_many_lines)]
fn realm_global_function_authority(
    name: &str,
    captured_lexical: Option<&str>,
) -> Arc<VerifiedBytecode> {
    let mut root_builder = BytecodeBuilder::new();
    let mut root_instructions = vec![
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutVar, Operands::VarRef(0)),
    ];
    if captured_lexical.is_some() {
        root_instructions.extend([
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::PushI8, Operands::I8(41)),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]);
    } else {
        root_instructions.extend([
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ]);
    }
    for (opcode, operands) in root_instructions {
        root_builder
            .push(opcode, operands)
            .expect("root fixture instruction");
    }
    let root_capture_layout: Arc<[CompilerCapturedBinding]> = captured_lexical.map_or_else(
        || Arc::from([]),
        |_| Arc::from([CompilerCapturedBinding::ScopedLocal(0)]),
    );
    let root_flow = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                root_builder.into_bytes(),
                FunctionIndexDomains::new(
                    1 + u32::from(captured_lexical.is_some()),
                    1,
                    0,
                    u32::from(captured_lexical.is_some()),
                    1,
                ),
                UnverifiedFunctionHeader::dynamic_function_script(u32::from(
                    captured_lexical.is_some(),
                )),
            )
            .with_capture_layout(CompilerCaptureLayout::new(root_capture_layout))
            .with_constant_layout(CompilerConstantLayout::new(Arc::from([
                quickjs_bytecode::CompilerConstantKind::Function,
            ]))),
            VerificationLimits::default(),
        )
        .expect("global function root flow"),
    );
    let mut child_builder = BytecodeBuilder::new();
    if captured_lexical.is_some() {
        child_builder
            .push(FinalOpcode::GetVarRefCheck, Operands::VarRef(0))
            .expect("child captured value");
    } else {
        child_builder
            .push(FinalOpcode::Push1, Operands::NoneInt)
            .expect("child value");
    }
    child_builder
        .push(FinalOpcode::Return, Operands::None)
        .expect("child return");
    let child_flow = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                child_builder.into_bytes(),
                FunctionIndexDomains::new(
                    1 + u32::from(captured_lexical.is_some()),
                    0,
                    0,
                    0,
                    u32::from(captured_lexical.is_some()),
                ),
                UnverifiedFunctionHeader::ordinary_source_function(false, 0),
            )
            .with_capture_layout(CompilerCaptureLayout::new(Arc::from([])))
            .with_constant_layout(CompilerConstantLayout::new(Arc::from([]))),
            VerificationLimits::default(),
        )
        .expect("global function child flow"),
    );
    let source = CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(0));
    let child_sources: Arc<[CompilerClosureSource]> = captured_lexical.map_or_else(
        || Arc::from([]),
        |_| Arc::from([CompilerClosureSource::ParentVariableReference(0)]),
    );
    let root_atoms: Arc<[CompilerAtom]> = captured_lexical.map_or_else(
        || Arc::from([atom(name)]),
        |lexical| Arc::from([atom(name), atom(lexical)]),
    );
    let child_atoms = Arc::clone(&root_atoms);
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&root_flow),
                        Arc::from([quickjs_bytecode::CompilerConstant::Function(
                            FunctionTemplateId::new(1),
                        )]),
                        Arc::from([source]),
                    )
                    .with_atom_pool(root_atoms),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        child_sources,
                    )
                    .with_atom_pool(child_atoms),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("global function graph"),
    );
    let text: Arc<str> = captured_lexical.map_or_else(
        || Arc::from(format!("function {name}(){{return 1}}")),
        |lexical| {
            Arc::from(format!(
                "function {name}(){{return {lexical}}};let {lexical}=41"
            ))
        },
    );
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mapped_source = |flow: &quickjs_bytecode::VerifiedControlFlow,
                         function_span: SourceByteSpan,
                         name_span: Option<SourceByteSpan>| {
        CompilerSource::new(
            Arc::from("realm-global-function.js"),
            Arc::clone(&text),
            function_span,
            name_span,
            Arc::from(
                flow.instructions()
                    .iter()
                    .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), function_span))
                    .collect::<Vec<_>>(),
            ),
        )
    };
    let root_variables: Arc<[VariableDefinition]> = captured_lexical.map_or_else(
        || Arc::from([]),
        |_| {
            Arc::from([VariableDefinition::new(
                Some(AtomPoolIndex::new(1)),
                ScopeLink::End,
                CompilerBindingPolicy::new(
                    CompilerBindingKind::Let,
                    CompilerInitializationPolicy::AtDeclaration,
                    CompilerWritePolicy::Mutable,
                    true,
                ),
                true,
                Some(0),
            )])
        },
    );
    let root_metadata = UnverifiedFunctionMetadata::new(
        None,
        root_variables,
        Arc::from([ClosureVariableDefinition::realm_global(
            Some(AtomPoolIndex::new(0)),
            policy(CompilerBindingKind::Function),
            source,
        )
        .with_function_initializer(0)]),
        mapped_source(&root_flow, full_span, None),
    )
    .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript);
    let child_closures: Arc<[ClosureVariableDefinition]> = captured_lexical.map_or_else(
        || Arc::from([]),
        |_| {
            Arc::from([ClosureVariableDefinition::new(
                Some(AtomPoolIndex::new(1)),
                CompilerBindingPolicy::new(
                    CompilerBindingKind::Let,
                    CompilerInitializationPolicy::AtDeclaration,
                    CompilerWritePolicy::Mutable,
                    true,
                ),
                CompilerClosureSource::ParentVariableReference(0),
            )])
        },
    );
    let child_metadata = UnverifiedFunctionMetadata::new(
        Some(AtomPoolIndex::new(0)),
        Arc::from([]),
        child_closures,
        mapped_source(
            &child_flow,
            full_span,
            Some(SourceByteSpan::new(
                9,
                9 + u32::try_from(name.len()).expect("name length"),
            )),
        ),
    );
    Arc::new(
        verify_compiler_bytecode_graph(
            UnverifiedCompilerBytecodeGraph::new(graph, Arc::from([root_metadata, child_metadata])),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("global function authority"),
    )
}

fn runtime() -> (Runtime, quickjs_runtime::Realm) {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    (runtime, realm)
}

fn assert_number(value: &quickjs_runtime::JsValue, expected: f64) {
    assert_eq!(
        value.as_number().expect("live value").map(JsNumber::as_f64),
        Some(expected)
    );
}

#[test]
fn global_function_is_installed_before_script_and_keeps_code_alive_through_the_realm() {
    let declaration = realm_global_function_authority("declared", None);
    let lookup = realm_global_authority(
        "declared",
        CompilerBindingKind::GlobalReference,
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let (mut runtime, realm) = runtime();
    let baseline = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let declared = context
            .execute_dynamic_function_script(declaration, ExecutionLimits::default())
            .expect("global function declaration")
            .into_function()
            .expect("declaration completion");
        let result = context
            .call(&declared, &[], ExecutionLimits::default())
            .expect("declared function call");
        assert_number(&result, 1.0);
    }

    let collection = runtime
        .collect_cycles()
        .expect("realm global function remains reachable");
    assert_eq!(collection.functions(), 0);
    let live = runtime.usage();
    assert_eq!(live.installed_code(), baseline.installed_code() + 1);
    assert_eq!(live.heap_functions(), baseline.heap_functions() + 1);
    assert_eq!(
        live.realm_global_bindings(),
        baseline.realm_global_bindings() + 1
    );
    assert_eq!(live.object_properties(), baseline.object_properties() + 5);

    let mut context = runtime.context(&realm).expect("lookup context");
    let declared = context
        .execute_dynamic_function_script(lookup, ExecutionLimits::default())
        .expect("global lookup")
        .into_function()
        .expect("global function");
    let result = context
        .call(&declared, &[], ExecutionLimits::default())
        .expect("globally rooted function call");
    assert_number(&result, 1.0);
}

#[test]
fn global_function_keeps_its_evaluation_local_lexical_cell_alive() {
    let declaration = realm_global_function_authority("readCell", Some("cell"));
    let lookup = realm_global_authority(
        "readCell",
        CompilerBindingKind::GlobalReference,
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let (mut runtime, realm) = runtime();
    let baseline = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let completion = context
            .execute_dynamic_function_script(declaration, ExecutionLimits::default())
            .expect("capturing global function declaration");
        assert_eq!(
            completion.kind().expect("live completion"),
            ValueKind::Undefined
        );
    }

    let collection = runtime
        .collect_cycles()
        .expect("realm global capture remains reachable");
    assert_eq!(collection.functions(), 0);
    assert_eq!(collection.binding_cells(), 0);
    let live = runtime.usage();
    assert_eq!(live.installed_code(), baseline.installed_code() + 1);
    assert_eq!(live.heap_functions(), baseline.heap_functions() + 1);
    assert_eq!(live.binding_cells(), baseline.binding_cells() + 1);

    let mut context = runtime.context(&realm).expect("lookup context");
    let function = context
        .execute_dynamic_function_script(lookup, ExecutionLimits::default())
        .expect("capturing global lookup")
        .into_function()
        .expect("capturing global function");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("capturing global function call");
    assert_number(&result, 41.0);
}

#[test]
fn failed_global_function_allocation_commits_the_declaration_without_leaking_code() {
    let declaration = realm_global_function_authority("limited", None);
    let lookup = realm_global_authority(
        "limited",
        CompilerBindingKind::GlobalReference,
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_functions(137)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");

    let error = context
        .execute_dynamic_function_script(declaration, ExecutionLimits::default())
        .expect_err("child allocation exceeds the heap-function limit");
    assert!(matches!(
        error,
        DynamicFunctionScriptError::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::HeapFunctions,
            limit: 137,
            observed: 138,
        })
    ));
    let committed = context.runtime_usage();
    assert_eq!(committed.installed_code(), baseline.installed_code());
    assert_eq!(committed.heap_functions(), baseline.heap_functions());
    assert_eq!(committed.binding_cells(), baseline.binding_cells());
    assert_eq!(
        committed.realm_global_bindings(),
        baseline.realm_global_bindings() + 1
    );
    assert_eq!(
        committed.object_properties(),
        baseline.object_properties() + 1
    );

    let value = context
        .execute_dynamic_function_script(lookup, ExecutionLimits::default())
        .expect("committed declaration lookup");
    assert_eq!(value.kind().expect("live value"), ValueKind::Undefined);
}

#[test]
fn var_declaration_and_unresolved_references_share_one_global_object_property() {
    let placeholder = realm_global_authority(
        "realmVar",
        CompilerBindingKind::GlobalReference,
        &[
            (FinalOpcode::GetVarUndef, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let declaration = realm_global_authority(
        "realmVar",
        CompilerBindingKind::Var,
        &[
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutVar, Operands::VarRef(0)),
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let redeclaration = realm_global_authority(
        "realmVar",
        CompilerBindingKind::Var,
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let lookup = realm_global_authority(
        "realmVar",
        CompilerBindingKind::GlobalReference,
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let update = realm_global_authority(
        "realmVar",
        CompilerBindingKind::GlobalReference,
        &[
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::PutVar, Operands::VarRef(0)),
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let (mut runtime, realm) = runtime();
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");

    let absent = context
        .execute_dynamic_function_script(placeholder, ExecutionLimits::default())
        .expect("unresolved placeholder");
    assert_eq!(absent.kind().expect("live value"), ValueKind::Undefined);
    let after_placeholder = context.runtime_usage();
    assert_eq!(
        after_placeholder.realm_global_bindings(),
        baseline.realm_global_bindings() + 1
    );
    assert_eq!(
        after_placeholder.object_properties(),
        baseline.object_properties()
    );

    let declared = context
        .execute_dynamic_function_script(declaration, ExecutionLimits::default())
        .expect("var declaration upgrades placeholder");
    assert_number(&declared, 1.0);
    let after_declaration = context.runtime_usage();
    assert_eq!(
        after_declaration.realm_global_bindings(),
        baseline.realm_global_bindings() + 1
    );
    assert_eq!(
        after_declaration.object_properties(),
        baseline.object_properties() + 1
    );

    let read = context
        .execute_dynamic_function_script(Arc::clone(&lookup), ExecutionLimits::default())
        .expect("unresolved read of var property");
    assert_number(&read, 1.0);
    let written = context
        .execute_dynamic_function_script(update, ExecutionLimits::default())
        .expect("unresolved write of var property");
    assert_number(&written, 2.0);
    let reread = context
        .execute_dynamic_function_script(lookup, ExecutionLimits::default())
        .expect("coherent unresolved reread");
    assert_number(&reread, 2.0);
    let redeclared = context
        .execute_dynamic_function_script(redeclaration, ExecutionLimits::default())
        .expect("var redeclaration");
    assert_number(&redeclared, 2.0);
    assert_eq!(context.runtime_usage(), after_declaration);
}

#[test]
fn global_var_property_limit_failure_is_atomic() {
    let declaration = realm_global_authority(
        "realmVar",
        CompilerBindingKind::Var,
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_object_properties(454))
        .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");

    let error = context
        .execute_dynamic_function_script(declaration, ExecutionLimits::default())
        .expect_err("global var property budget");
    assert!(matches!(
        error,
        DynamicFunctionScriptError::Install(quickjs_runtime::InstallError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit: 454,
            observed: 455,
        })
    ));
    assert_eq!(context.runtime_usage(), baseline);
}
