use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, BytecodeGraphResource, BytecodeGraphVerificationLimits,
    BytecodePc, BytecodeVerificationErrorKind, ClosureVariableDefinition, CompilerAtom,
    CompilerBindingKind, CompilerBindingPolicy, CompilerCaptureLayout, CompilerCapturedBinding,
    CompilerClosureSource, CompilerConstantKind, CompilerConstantLayout,
    CompilerInitializationPolicy, CompilerSource, CompilerString, CompilerWritePolicy,
    ExecutionRequirement, FinalOpcode, FunctionGraphVerificationLimits, FunctionIndexDomains,
    FunctionTemplateId, Operands, PcSourceSpan, ScopeLink, SourceByteSpan,
    UnverifiedCompilerBytecodeGraph, UnverifiedCompilerFunction, UnverifiedCompilerFunctionBody,
    UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader, UnverifiedFunctionMetadata,
    VariableDefinition, VerificationLimits, VerifiedBytecode, VerifiedControlFlow,
    verify_compiler_bytecode_graph, verify_compiler_control_flow, verify_compiler_function_graph,
};

fn atom(text: &str) -> CompilerAtom {
    CompilerAtom::new(
        CompilerString::try_from_code_units(text.encode_utf16().collect::<Vec<_>>().into())
            .expect("fixture atom"),
    )
}

fn encode(instructions: &[(FinalOpcode, Operands)]) -> Vec<u8> {
    let mut builder = BytecodeBuilder::new();
    for &(opcode, operands) in instructions {
        builder.push(opcode, operands).expect("fixture instruction");
    }
    builder.into_bytes()
}

fn flow(
    instructions: &[(FinalOpcode, Operands)],
    atoms: u32,
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    imported_closures: u32,
    constant_kinds: &[CompilerConstantKind],
) -> Arc<VerifiedControlFlow> {
    Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(instructions),
                FunctionIndexDomains::new(
                    atoms,
                    u32::try_from(constant_kinds.len()).expect("constant count"),
                    arguments,
                    locals,
                    imported_closures,
                ),
                UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(
                    false,
                    arguments,
                    u32::try_from(captures.len()).expect("capture count"),
                ),
            )
            .with_capture_layout(CompilerCaptureLayout::new(Arc::from(captures)))
            .with_constant_layout(CompilerConstantLayout::new(Arc::from(constant_kinds))),
            VerificationLimits::default(),
        )
        .expect("fixture body"),
    )
}

fn parameter_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Parameter,
        CompilerInitializationPolicy::Argument,
        CompilerWritePolicy::Mutable,
        false,
    )
}

fn let_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Let,
        CompilerInitializationPolicy::AtDeclaration,
        CompilerWritePolicy::Mutable,
        true,
    )
}

fn var_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Var,
        CompilerInitializationPolicy::UndefinedAtInstantiation,
        CompilerWritePolicy::Mutable,
        false,
    )
}

fn function_name_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::FunctionName,
        CompilerInitializationPolicy::FunctionName,
        CompilerWritePolicy::Immutable,
        false,
    )
}

fn const_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Const,
        CompilerInitializationPolicy::AtDeclaration,
        CompilerWritePolicy::Immutable,
        true,
    )
}

fn function_policy(initialization: CompilerInitializationPolicy) -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Function,
        initialization,
        CompilerWritePolicy::Mutable,
        false,
    )
}

fn source(
    text: &str,
    function_span: SourceByteSpan,
    name_span: Option<SourceByteSpan>,
    mappings: &[(u32, SourceByteSpan)],
) -> CompilerSource {
    CompilerSource::new(
        Arc::from("fixture.js"),
        Arc::from(text),
        function_span,
        name_span,
        Arc::from(
            mappings
                .iter()
                .map(|&(pc, span)| PcSourceSpan::new(BytecodePc::new(pc), span))
                .collect::<Vec<_>>(),
        ),
    )
}

fn verified_single(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    source: CompilerSource,
) -> Result<VerifiedBytecode, quickjs_bytecode::BytecodeVerificationError> {
    verify_compiler_bytecode_graph(
        single_input(instructions, atoms, variables, source),
        BytecodeGraphVerificationLimits::default(),
    )
}

#[test]
fn final_authority_sorts_call_before_the_adjacent_abrupt_requirement() {
    let instructions = [
        (FinalOpcode::Push7, Operands::NoneInt),
        (FinalOpcode::Call0, Operands::NPopX),
        (FinalOpcode::Throw, Operands::None),
    ];
    let text = "function f(argument){var local;throw (7)();}";
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        ),
    ];
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));

    let verified = verified_single(
        &instructions,
        &[atom("f"), atom("argument"), atom("local")],
        &variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(38, 39)),
                (1, SourceByteSpan::new(37, 42)),
                (2, SourceByteSpan::new(31, 43)),
            ],
        ),
    )
    .expect("a directly called result can gain explicit-throw authority");

    assert!(
        verified
            .requirements()
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "requirements remain sorted and deduplicated"
    );
    assert!(
        verified.requirements().windows(2).any(|pair| pair
            == [
                ExecutionRequirement::Calls,
                ExecutionRequirement::AbruptCompletions,
            ]),
        "the call and abrupt-completion requirements remain adjacent and ordered"
    );
    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Numbers,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Calls,
            ExecutionRequirement::AbruptCompletions,
        ]
    );
}

#[test]
fn final_authority_keeps_throw_error_fail_closed() {
    let instructions = [(
        FinalOpcode::ThrowError,
        Operands::AtomU8 {
            atom: AtomPoolIndex::new(0),
            value: 0,
        },
    )];
    let text = "function f(argument){var local;return undefined}";
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        ),
    ];
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));

    let error = verified_single(
        &instructions,
        &[atom("f"), atom("argument"), atom("local")],
        &variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[(0, function_span)],
        ),
    )
    .expect_err("the internal throw-error shortcut remains outside final authority");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::ThrowError,
        } if *pc == BytecodePc::ZERO
    ));
}

#[test]
fn final_authority_admits_only_direct_calls_and_records_the_requirement() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Call0, Operands::NPopX),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Call1, Operands::NPopX),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Call2, Operands::NPopX),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Call3, Operands::NPopX),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Call, Operands::NPop { argument_count: 4 }),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let text = "function f(argument){var local;return undefined}";
    let mappings = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 26,
        27,
    ]
    .map(|pc| {
        (
            pc,
            SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length")),
        )
    });
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        ),
    ];

    let verified = verified_single(
        &instructions,
        &[atom("f"), atom("argument"), atom("local")],
        &variables,
        source(
            text,
            SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length")),
            Some(SourceByteSpan::new(9, 10)),
            &mappings,
        ),
    )
    .expect("all direct ordinary call encodings gain final authority");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Calls,
        ]
    );
}

#[test]
fn final_authority_keeps_non_direct_call_families_fail_closed() {
    assert_final_authority_rejects_call_family(
        &[
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count: 0 },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        FinalOpcode::CallMethod,
        &[0, 1, 2, 5],
    );
    assert_final_authority_rejects_call_family(
        &[
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (
                FinalOpcode::CallConstructor,
                Operands::NPop { argument_count: 0 },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        FinalOpcode::CallConstructor,
        &[0, 1, 2, 5],
    );
    assert_final_authority_rejects_call_family(
        &[
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Apply, Operands::U16(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        FinalOpcode::Apply,
        &[0, 1, 2, 3, 6],
    );
    assert_final_authority_rejects_call_family(
        &[
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::TailCall, Operands::NPop { argument_count: 0 }),
        ],
        FinalOpcode::TailCall,
        &[0, 1],
    );
    assert_final_authority_rejects_call_family(
        &[
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (
                FinalOpcode::TailCallMethod,
                Operands::NPop { argument_count: 0 },
            ),
        ],
        FinalOpcode::TailCallMethod,
        &[0, 1, 2],
    );
}

#[track_caller]
fn assert_final_authority_rejects_call_family(
    instructions: &[(FinalOpcode, Operands)],
    rejected: FinalOpcode,
    pcs: &[u32],
) {
    let text = "function f(argument){var local;return undefined}";
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        ),
    ];
    let mappings = pcs
        .iter()
        .copied()
        .map(|pc| {
            (
                pc,
                SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length")),
            )
        })
        .collect::<Vec<_>>();
    let error = verified_single(
        instructions,
        &[atom("f"), atom("argument"), atom("local")],
        &variables,
        source(
            text,
            SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length")),
            Some(SourceByteSpan::new(9, 10)),
            &mappings,
        ),
    )
    .expect_err("non-direct call families remain outside final authority");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::UnsupportedCompilerOpcode { opcode, .. }
                if *opcode == rejected
        ),
        "{rejected}"
    );
}

fn single_input(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    source: CompilerSource,
) -> UnverifiedCompilerBytecodeGraph {
    shaped_input(instructions, atoms, variables, 1, 1, &[], source)
}

fn shaped_input(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    source: CompilerSource,
) -> UnverifiedCompilerBytecodeGraph {
    let flow = flow(
        instructions,
        u32::try_from(atoms.len()).expect("atom count"),
        arguments,
        locals,
        captures,
        0,
        &[],
    );
    let graph = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(
            FunctionTemplateId::new(0),
            Arc::from([
                UnverifiedCompilerFunction::new(flow, Arc::from([]), Arc::from([]))
                    .with_atom_pool(Arc::from(atoms)),
            ]),
        ),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("fixture graph");
    UnverifiedCompilerBytecodeGraph::new(
        Arc::new(graph),
        Arc::from([UnverifiedFunctionMetadata::new(
            Some(AtomPoolIndex::new(0)),
            Arc::from(variables),
            Arc::from([]),
            source,
        )]),
    )
}

fn function_initializer_input(
    instructions: &[(FinalOpcode, Operands)],
    definition_name: &str,
    definition: VariableDefinition,
) -> UnverifiedCompilerBytecodeGraph {
    let root_flow = flow(
        instructions,
        2,
        0,
        1,
        &[],
        0,
        &[CompilerConstantKind::Function],
    );
    let child_flow = flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        1,
        0,
        0,
        &[],
        0,
        &[],
    );
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
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("outer"), atom(definition_name)])),
                    UnverifiedCompilerFunction::new(
                        child_flow.clone(),
                        Arc::from([]),
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("inner")])),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("fixture graph"),
    );
    let text: Arc<str> = Arc::from("function outer(){function inner(){}}");
    let root_span =
        SourceByteSpan::new(0, u32::try_from(text.len()).expect("fixture source length"));
    let source_for = |flow: &VerifiedControlFlow,
                      function_span: SourceByteSpan,
                      name_span: SourceByteSpan| {
        CompilerSource::new(
            Arc::from("fixture.js"),
            Arc::clone(&text),
            function_span,
            Some(name_span),
            Arc::from(
                flow.instructions()
                    .iter()
                    .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), function_span))
                    .collect::<Vec<_>>(),
            ),
        )
    };
    UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([definition]),
                Arc::from([]),
                source_for(&root_flow, root_span, SourceByteSpan::new(9, 14)),
            ),
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([]),
                Arc::from([]),
                source_for(
                    &child_flow,
                    SourceByteSpan::new(17, 35),
                    SourceByteSpan::new(26, 31),
                ),
            ),
        ]),
    )
}

fn assert_limit(
    input: &UnverifiedCompilerBytecodeGraph,
    accepted: BytecodeGraphVerificationLimits,
    rejected: BytecodeGraphVerificationLimits,
    resource: BytecodeGraphResource,
    limit: u64,
    observed: u64,
) {
    verify_compiler_bytecode_graph(input.clone(), accepted)
        .expect("an inclusive final-verifier limit accepts exact usage");
    let error = verify_compiler_bytecode_graph(input.clone(), rejected)
        .expect_err("one less than exact usage must fail closed");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::LimitExceeded {
            resource,
            limit,
            observed,
        }
    );
}

#[test]
fn complete_ordinary_metadata_grants_send_sync_execution_authority() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VerifiedBytecode>();

    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::GetLocCheck, Operands::Loc(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
    ];
    let verified = verified_single(
        &instructions,
        &[atom("f"), atom("a"), atom("x")],
        &variables,
        source(
            "function f(a){let x=1;return x}",
            SourceByteSpan::new(0, 31),
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(14, 21)),
                (3, SourceByteSpan::new(20, 21)),
                (4, SourceByteSpan::new(18, 19)),
                (5, SourceByteSpan::new(29, 30)),
                (8, SourceByteSpan::new(22, 30)),
            ],
        ),
    )
    .expect("complete compiler metadata must verify");

    assert_eq!(verified.root_id(), FunctionTemplateId::new(0));
    let function = verified
        .function(FunctionTemplateId::new(0))
        .expect("root function");
    assert_eq!(
        function
            .metadata()
            .function_name()
            .expect("named function")
            .get(),
        0
    );
    assert_eq!(function.metadata().variables(), variables);
    let source = function.metadata().source();
    assert_eq!(source.display_name(), "fixture.js");
    assert_eq!(source.function_source(), "function f(a){let x=1;return x}");
    assert!(Arc::ptr_eq(
        &source.display_name_arc(),
        &source.display_name_arc()
    ));
    assert!(Arc::ptr_eq(&source.text_arc(), &source.text_arc()));
}

#[test]
fn final_graph_limits_are_inclusive_and_bound_all_state_work() {
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::GetLocCheck, Operands::Loc(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
    ];
    let input = single_input(
        &instructions,
        &[atom("f"), atom("a"), atom("x")],
        &variables,
        source(
            "function f(a){let x=1;return x}",
            SourceByteSpan::new(0, 31),
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(14, 21)),
                (3, SourceByteSpan::new(20, 21)),
                (4, SourceByteSpan::new(18, 19)),
                (5, SourceByteSpan::new(29, 30)),
                (8, SourceByteSpan::new(22, 30)),
            ],
        ),
    );
    let usage =
        verify_compiler_bytecode_graph(input.clone(), BytecodeGraphVerificationLimits::default())
            .expect("baseline authority")
            .usage();

    assert_limit(
        &input,
        BytecodeGraphVerificationLimits::default()
            .with_max_variable_definitions(usage.variable_definitions()),
        BytecodeGraphVerificationLimits::default()
            .with_max_variable_definitions(usage.variable_definitions() - 1),
        BytecodeGraphResource::VariableDefinitions,
        usage.variable_definitions() - 1,
        usage.variable_definitions(),
    );
    assert_limit(
        &input,
        BytecodeGraphVerificationLimits::default().with_max_source_bytes(usage.source_bytes()),
        BytecodeGraphVerificationLimits::default().with_max_source_bytes(usage.source_bytes() - 1),
        BytecodeGraphResource::SourceBytes,
        usage.source_bytes() - 1,
        usage.source_bytes(),
    );
    assert_limit(
        &input,
        BytecodeGraphVerificationLimits::default()
            .with_max_source_mappings(usage.source_mappings()),
        BytecodeGraphVerificationLimits::default()
            .with_max_source_mappings(usage.source_mappings() - 1),
        BytecodeGraphResource::SourceMappings,
        usage.source_mappings() - 1,
        usage.source_mappings(),
    );
    assert_limit(
        &input,
        BytecodeGraphVerificationLimits::default()
            .with_max_frame_state_entries(usage.frame_state_entries()),
        BytecodeGraphVerificationLimits::default()
            .with_max_frame_state_entries(usage.frame_state_entries() - 1),
        BytecodeGraphResource::FrameStateEntries,
        usage.frame_state_entries() - 1,
        usage.frame_state_entries(),
    );
    assert_limit(
        &input,
        BytecodeGraphVerificationLimits::default()
            .with_max_policy_transfers(usage.policy_transfers()),
        BytecodeGraphVerificationLimits::default()
            .with_max_policy_transfers(usage.policy_transfers() - 1),
        BytecodeGraphResource::PolicyTransfers,
        usage.policy_transfers() - 1,
        usage.policy_transfers(),
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn metadata_names_counts_scope_links_and_source_pcs_are_fail_closed() {
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let valid_source = || {
        source(
            "function f(a){let x=1}",
            SourceByteSpan::new(0, 22),
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(14, 21)),
                (3, SourceByteSpan::new(20, 21)),
                (4, SourceByteSpan::new(18, 19)),
                (5, SourceByteSpan::new(13, 22)),
            ],
        )
    };

    let only_argument = [VariableDefinition::new(
        Some(AtomPoolIndex::new(1)),
        ScopeLink::End,
        parameter_policy(),
        false,
        None,
    )];
    let error = verified_single(
        &instructions,
        &[atom("f"), atom("a"), atom("x")],
        &only_argument,
        valid_source(),
    )
    .expect_err("vardef count mismatch");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::VariableDefinitionCountMismatch { .. }
    ));

    let bad_name = [
        only_argument[0].clone(),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(3)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
    ];
    let error = verified_single(
        &instructions,
        &[atom("f"), atom("a"), atom("x")],
        &bad_name,
        valid_source(),
    )
    .expect_err("metadata atom index must be checked");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::MetadataAtomOutOfBounds { .. }
    ));

    let cyclic = [
        only_argument[0].clone(),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::Local(0),
            let_policy(),
            true,
            None,
        ),
    ];
    let error = verified_single(
        &instructions,
        &[atom("f"), atom("a"), atom("x")],
        &cyclic,
        valid_source(),
    )
    .expect_err("self-linked scope chain must fail");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ScopeLinkCycle { .. }
    ));

    let valid_variables = [
        only_argument[0].clone(),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
    ];
    let error = verified_single(
        &instructions,
        &[atom("f"), atom("a"), atom("x")],
        &valid_variables,
        source(
            "function f(a){let x=1}",
            SourceByteSpan::new(0, 22),
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(14, 21)),
                (3, SourceByteSpan::new(20, 21)),
                (4, SourceByteSpan::new(18, 19)),
                (6, SourceByteSpan::new(13, 24)),
            ],
        ),
    )
    .expect_err("source PC must match the verified instruction");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SourcePcMismatch { .. }
    ));

    let scoped_to_function = [
        only_argument[0].clone(),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(3)),
            ScopeLink::Local(0),
            let_policy(),
            true,
            None,
        ),
    ];
    let error = verify_compiler_bytecode_graph(
        shaped_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            &[atom("f"), atom("a"), atom("v"), atom("x")],
            &scoped_to_function,
            1,
            2,
            &[],
            source(
                "function f(a){var v;let x}",
                SourceByteSpan::new(0, 26),
                Some(SourceByteSpan::new(9, 10)),
                &[(0, SourceByteSpan::new(0, 26))],
            ),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a lexical scope chain cannot enter a function-scoped vardef");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ScopeLinkKindMismatch { .. }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn policy_analysis_rejects_unchecked_tdz_reads_and_noncompiler_opcodes() {
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
    ];
    let error = verified_single(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::GetLoc0, Operands::NoneLoc),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("x")],
        &variables,
        source(
            "function f(a){return x;let x}",
            SourceByteSpan::new(0, 29),
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(23, 28)),
                (3, SourceByteSpan::new(21, 22)),
                (4, SourceByteSpan::new(14, 22)),
            ],
        ),
    )
    .expect_err("unchecked lexical read must not gain authority");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation { .. }
    ));

    let immutable_variables = [
        variables[0].clone(),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            function_name_policy(),
            false,
            None,
        ),
    ];
    let error = verified_single(
        &[
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("self")],
        &immutable_variables,
        source(
            "function f(a){self=1}",
            SourceByteSpan::new(0, 21),
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(19, 20)),
                (1, SourceByteSpan::new(14, 20)),
                (2, SourceByteSpan::new(20, 21)),
            ],
        ),
    )
    .expect_err("named-function self bindings remain outside the current authority profile");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            reason: quickjs_bytecode::BindingPolicyViolationReason::InvalidDeclarationPolicy,
            ..
        }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn frame_state_distinguishes_proven_let_access_from_repeated_const_initialization() {
    let let_variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
    ];
    let text = "function f(a){let x}";
    let function_span =
        SourceByteSpan::new(0, u32::try_from(text.len()).expect("fixture source length"));
    let verified = verified_single(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::GetLoc0, Operands::NoneLoc),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("x")],
        &let_variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, function_span),
                (3, function_span),
                (4, function_span),
                (5, function_span),
                (6, function_span),
            ],
        ),
    )
    .expect("unchecked access is valid after every incoming path initializes a mutable let");
    assert!(
        verified
            .function(FunctionTemplateId::new(0))
            .is_some_and(|function| function.metadata().variables() == let_variables)
    );

    let const_variables = [
        let_variables[0].clone(),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            const_policy(),
            true,
            None,
        ),
    ];
    let error = verified_single(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("x")],
        &const_variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, function_span),
                (3, function_span),
                (4, function_span),
                (5, function_span),
                (6, function_span),
                (7, function_span),
            ],
        ),
    )
    .expect_err("an immutable lexical binding has exactly one initialization");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation { .. }
    ));

    let error = verified_single(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("x")],
        &const_variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, function_span),
                (3, function_span),
                (4, function_span),
                (5, function_span),
                (8, function_span),
                (9, function_span),
                (10, function_span),
            ],
        ),
    )
    .expect_err("one lexical slot has one static scope-entry initialization site");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation { .. }
    ));
}

#[test]
fn captured_scope_reentry_requires_closing_the_previous_cell() {
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            let_policy(),
            true,
            Some(0),
        ),
    ];
    let text = "function f(a){let x}";
    let function_span =
        SourceByteSpan::new(0, u32::try_from(text.len()).expect("fixture source length"));
    let make_source = |pcs: &[u32]| {
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &pcs.iter()
                .map(|&pc| (pc, function_span))
                .collect::<Vec<_>>(),
        )
    };
    let captures = [CompilerCapturedBinding::ScopedLocal(0)];

    let error = verify_compiler_bytecode_graph(
        shaped_input(
            &[
                (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
                (FinalOpcode::Goto8, Operands::Label8(-6)),
            ],
            &[atom("f"), atom("a"), atom("x")],
            &variables,
            1,
            1,
            &captures,
            make_source(&[0, 3, 4, 5]),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a captured scope cannot reopen while its previous cell is active");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation { .. }
    ));

    verify_compiler_bytecode_graph(
        shaped_input(
            &[
                (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
                (FinalOpcode::CloseLoc, Operands::Loc(0)),
                (FinalOpcode::Goto8, Operands::Label8(-9)),
            ],
            &[atom("f"), atom("a"), atom("x")],
            &variables,
            1,
            1,
            &captures,
            make_source(&[0, 3, 4, 5, 8]),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("closing the captured cell makes one scope-entry PC safely reentrant");
}

#[test]
#[allow(clippy::too_many_lines)]
fn function_initializers_require_exact_metadata_entry_placement_and_child_name() {
    let missing = VariableDefinition::new(
        Some(AtomPoolIndex::new(1)),
        ScopeLink::End,
        function_policy(CompilerInitializationPolicy::FunctionAtInstantiation),
        false,
        None,
    );
    let error = verify_compiler_bytecode_graph(
        function_initializer_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            "inner",
            missing,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a function binding must name its initializing child constant");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
            definition: 0,
            constant: None,
        }
    );

    let initialized = VariableDefinition::new(
        Some(AtomPoolIndex::new(1)),
        ScopeLink::End,
        function_policy(CompilerInitializationPolicy::FunctionAtInstantiation),
        false,
        None,
    )
    .with_function_initializer(0);
    let error = verify_compiler_bytecode_graph(
        function_initializer_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            "inner",
            initialized.clone(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("metadata alone cannot stand in for FClosure plus PutLoc");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::FunctionInitializerOpcodeMismatch {
            definition: 0,
            constant: 0,
            matches: 0,
        }
    );

    let misplaced = [
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        function_initializer_input(&misplaced, "inner", initialized.clone()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("function-instantiation pairs must dominate from the entry prelude");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch { definition: 0, .. }
    ));

    let valid = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        function_initializer_input(&valid, "inner", initialized.clone()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the exact entry pair and declaration child name grant authority");
    let error = verify_compiler_bytecode_graph(
        function_initializer_input(&valid, "sibling", initialized),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the selected child template must have the declaration binding name");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
            definition: 0,
            constant: Some(0),
        }
    );

    let scoped = VariableDefinition::new(
        Some(AtomPoolIndex::new(1)),
        ScopeLink::End,
        function_policy(CompilerInitializationPolicy::FunctionAtScopeEntry),
        true,
        None,
    )
    .with_function_initializer(0);
    let scoped_group = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        function_initializer_input(&scoped_group, "inner", scoped.clone()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a scope-entry activation and initializer pair form one atomic prelude");
    let split_group = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        function_initializer_input(&split_group, "inner", scoped),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("user bytecode cannot split scope activation from declaration initialization");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch { definition: 0, .. }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn final_authority_requires_tree_owned_templates() {
    let root_flow = flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        1,
        0,
        0,
        &[],
        0,
        &[
            CompilerConstantKind::Function,
            CompilerConstantKind::Function,
        ],
    );
    let child_flow = flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        1,
        0,
        0,
        &[],
        0,
        &[],
    );
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([
                    UnverifiedCompilerFunction::new(
                        root_flow,
                        Arc::from([
                            quickjs_bytecode::CompilerConstant::Function(FunctionTemplateId::new(
                                1,
                            )),
                            quickjs_bytecode::CompilerConstant::Function(FunctionTemplateId::new(
                                1,
                            )),
                        ]),
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("outer")])),
                    UnverifiedCompilerFunction::new(child_flow, Arc::from([]), Arc::from([]))
                        .with_atom_pool(Arc::from([atom("inner")])),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("the staged DAG permits duplicate edges"),
    );
    let text = "function outer(){} function inner(){}";
    let function_span = SourceByteSpan::new(0, 37);
    let metadata = Arc::from([
        UnverifiedFunctionMetadata::new(
            Some(AtomPoolIndex::new(0)),
            Arc::from([]),
            Arc::from([]),
            source(
                text,
                function_span,
                Some(SourceByteSpan::new(9, 14)),
                &[(0, function_span)],
            ),
        ),
        UnverifiedFunctionMetadata::new(
            Some(AtomPoolIndex::new(0)),
            Arc::from([]),
            Arc::from([]),
            source(
                text,
                function_span,
                Some(SourceByteSpan::new(28, 33)),
                &[(0, function_span)],
            ),
        ),
    ]);
    let error = verify_compiler_bytecode_graph(
        UnverifiedCompilerBytecodeGraph::new(graph, metadata),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("every non-root template must have one compiler ownership edge");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
            child: FunctionTemplateId::new(1),
            incoming: 2,
        }
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn closure_descriptors_must_preserve_parent_name_policy_and_source() {
    let root_flow = flow(
        &[
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        2,
        1,
        0,
        &[CompilerCapturedBinding::Argument(0)],
        0,
        &[CompilerConstantKind::Function],
    );
    let child_flow = flow(
        &[
            (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
            (FinalOpcode::Return, Operands::None),
        ],
        2,
        0,
        0,
        &[],
        1,
        &[],
    );
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([
                    UnverifiedCompilerFunction::new(
                        root_flow,
                        Arc::from([quickjs_bytecode::CompilerConstant::Function(
                            FunctionTemplateId::new(1),
                        )]),
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("outer"), atom("value")])),
                    UnverifiedCompilerFunction::new(
                        child_flow,
                        Arc::from([]),
                        Arc::from([CompilerClosureSource::ParentVariableReference(0)]),
                    )
                    .with_atom_pool(Arc::from([atom("inner"), atom("value")])),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("staged graph"),
    );
    let root_metadata = UnverifiedFunctionMetadata::new(
        Some(AtomPoolIndex::new(0)),
        Arc::from([VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            parameter_policy(),
            false,
            Some(0),
        )]),
        Arc::from([]),
        source(
            "function outer(value){return function inner(){return value}}",
            SourceByteSpan::new(0, 59),
            Some(SourceByteSpan::new(9, 14)),
            &[
                (0, SourceByteSpan::new(29, 59)),
                (2, SourceByteSpan::new(22, 59)),
            ],
        ),
    );
    let child_source = || {
        source(
            "function outer(value){return function inner(){return value}}",
            SourceByteSpan::new(29, 59),
            Some(SourceByteSpan::new(38, 43)),
            &[
                (0, SourceByteSpan::new(53, 58)),
                (1, SourceByteSpan::new(46, 58)),
            ],
        )
    };
    let child_metadata = UnverifiedFunctionMetadata::new(
        Some(AtomPoolIndex::new(0)),
        Arc::from([]),
        Arc::from([ClosureVariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            parameter_policy(),
            CompilerClosureSource::ParentVariableReference(0),
        )]),
        child_source(),
    );

    let valid = UnverifiedCompilerBytecodeGraph::new(
        Arc::clone(&graph),
        Arc::from([root_metadata.clone(), child_metadata.clone()]),
    );
    let accepted = verify_compiler_bytecode_graph(
        valid.clone(),
        BytecodeGraphVerificationLimits::default().with_max_closure_definitions(1),
    )
    .expect("the inclusive closure-definition limit accepts exact usage");
    assert_eq!(accepted.usage().closure_definitions(), 1);
    assert_eq!(accepted.usage().policy_transfers(), 1);
    let error = verify_compiler_bytecode_graph(
        valid.clone(),
        BytecodeGraphVerificationLimits::default().with_max_closure_definitions(0),
    )
    .expect_err("closure-definition usage must be aggregate-bounded");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::ClosureDefinitions,
            limit: 0,
            observed: 1,
        }
    );
    let error = verify_compiler_bytecode_graph(
        valid,
        BytecodeGraphVerificationLimits::default().with_max_policy_transfers(0),
    )
    .expect_err("retained parent-edge checks consume the final policy-work budget");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::PolicyTransfers,
            limit: 0,
            observed: 1,
        }
    );

    let bad_child_metadata = UnverifiedFunctionMetadata::new(
        Some(AtomPoolIndex::new(0)),
        Arc::from([]),
        Arc::from([ClosureVariableDefinition::new(
            Some(AtomPoolIndex::new(0)),
            parameter_policy(),
            CompilerClosureSource::ParentVariableReference(0),
        )]),
        child_source(),
    );
    let error = verify_compiler_bytecode_graph(
        UnverifiedCompilerBytecodeGraph::new(graph, Arc::from([root_metadata, bad_child_metadata])),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("closure name mismatch must fail");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ClosureMetadataMismatch { .. }
    ));
}
