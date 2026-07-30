use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, BytecodeGraphVerificationLimits, ClosureVariableDefinition,
    CompilerAtom, CompilerBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerClosureSource, CompilerConstantLayout, CompilerExecutableKind,
    CompilerInitializationPolicy, CompilerSource, CompilerString, CompilerWritePolicy, FinalOpcode,
    FunctionGraphVerificationLimits, FunctionIndexDomains, FunctionTemplateId, Operands,
    PcSourceSpan, SourceByteSpan, UnverifiedCompilerBytecodeGraph, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader,
    UnverifiedFunctionMetadata, VerificationLimits, VerifiedBytecode,
    verify_compiler_bytecode_graph, verify_compiler_control_flow, verify_compiler_function_graph,
};
use quickjs_runtime::{
    DynamicFunctionScriptError, ExecutionLimits, JsNumber, Runtime, RuntimeLimits, RuntimeResource,
    ValueKind,
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
        CompilerBindingKind::Parameter
        | CompilerBindingKind::Function
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
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_object_properties(0)).expect("runtime");
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
            limit: 0,
            observed: 1,
        })
    ));
    assert_eq!(context.runtime_usage(), baseline);
}
