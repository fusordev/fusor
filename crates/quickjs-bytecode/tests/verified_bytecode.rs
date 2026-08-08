use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BindingPolicyViolationReason, BindingSlot, BytecodeBuilder,
    BytecodeGraphResource, BytecodeGraphVerificationLimits, BytecodePc,
    BytecodeVerificationErrorKind, ClosureVariableDefinition, CompilerAtom, CompilerBindingKind,
    CompilerBindingPolicy, CompilerCaptureLayout, CompilerCapturedBinding, CompilerClosureBinding,
    CompilerClosureSource, CompilerConstantKind, CompilerConstantLayout, CompilerExecutableKind,
    CompilerInitializationPolicy, CompilerSource, CompilerString, CompilerWritePolicy,
    EXECUTION_REQUIREMENT_COUNT, ExecutionRequirement, FinalOpcode,
    FunctionGraphVerificationLimits, FunctionIndexDomains, FunctionTemplateId,
    MAX_GOSUB_SITES_PER_FUNCTION, MetadataAtomField, Operands, PcSourceSpan, ScopeLink,
    SourceByteSpan, UnverifiedCompilerBytecodeGraph, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader,
    UnverifiedFunctionMetadata, VariableDefinition, VerificationErrorKind, VerificationLimits,
    VerifiedBytecode, VerifiedControlFlow, verify_compiler_bytecode_graph,
    verify_compiler_control_flow, verify_compiler_function_graph,
};

fn atom(text: &str) -> CompilerAtom {
    CompilerAtom::new(
        CompilerString::try_from_code_units(text.encode_utf16().collect::<Vec<_>>().into())
            .expect("fixture atom"),
    )
}

fn static_property_only_atom(text: &str) -> CompilerAtom {
    CompilerAtom::new_static_property_only(
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
    flow_with_strict(
        instructions,
        atoms,
        arguments,
        locals,
        captures,
        imported_closures,
        constant_kinds,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn flow_with_strict(
    instructions: &[(FinalOpcode, Operands)],
    atoms: u32,
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    imported_closures: u32,
    constant_kinds: &[CompilerConstantKind],
    strict: bool,
) -> Arc<VerifiedControlFlow> {
    flow_with_header(
        instructions,
        atoms,
        arguments,
        locals,
        captures,
        imported_closures,
        constant_kinds,
        UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(
            strict,
            arguments,
            u32::try_from(captures.len()).expect("capture count"),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn flow_with_header(
    instructions: &[(FinalOpcode, Operands)],
    atoms: u32,
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    imported_closures: u32,
    constant_kinds: &[CompilerConstantKind],
    header: UnverifiedFunctionHeader,
) -> Arc<VerifiedControlFlow> {
    flow_with_header_and_capture_layout(
        instructions,
        atoms,
        arguments,
        locals,
        imported_closures,
        constant_kinds,
        header,
        CompilerCaptureLayout::new(Arc::from(captures)),
    )
}

#[allow(clippy::too_many_arguments)]
fn flow_with_header_and_capture_layout(
    instructions: &[(FinalOpcode, Operands)],
    atoms: u32,
    arguments: u32,
    locals: u32,
    imported_closures: u32,
    constant_kinds: &[CompilerConstantKind],
    header: UnverifiedFunctionHeader,
    capture_layout: CompilerCaptureLayout,
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
                header,
            )
            .with_capture_layout(capture_layout)
            .with_constant_layout(CompilerConstantLayout::new(Arc::from(constant_kinds))),
            VerificationLimits::default(),
        )
        .expect("fixture body"),
    )
}

fn mapped_arguments_flow(
    instructions: &[(FinalOpcode, Operands)],
    arguments: u32,
    mapped_arguments: &[u32],
    strict: bool,
) -> Arc<VerifiedControlFlow> {
    flow_with_header_and_capture_layout(
        instructions,
        1,
        arguments,
        0,
        0,
        &[],
        UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(
            strict, arguments, 0,
        ),
        CompilerCaptureLayout::default().with_mapped_arguments(Arc::from(mapped_arguments)),
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

fn parameter_tdz_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Parameter,
        CompilerInitializationPolicy::Argument,
        CompilerWritePolicy::Mutable,
        true,
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

fn catch_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::Catch,
        CompilerInitializationPolicy::Catch,
        CompilerWritePolicy::Mutable,
        false,
    )
}

fn function_name_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::FunctionName,
        CompilerInitializationPolicy::FunctionName,
        CompilerWritePolicy::ImmutableInStrictCode,
        false,
    )
}

fn strict_function_name_policy() -> CompilerBindingPolicy {
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

fn global_reference_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        CompilerBindingKind::GlobalReference,
        CompilerInitializationPolicy::ConstructorRealmLookup,
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

fn source_for_flow(
    text: &Arc<str>,
    flow: &VerifiedControlFlow,
    function_span: SourceByteSpan,
    name_span: SourceByteSpan,
) -> CompilerSource {
    CompilerSource::new(
        Arc::from("fixture.js"),
        Arc::clone(text),
        function_span,
        Some(name_span),
        Arc::from(
            flow.instructions()
                .iter()
                .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), function_span))
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
fn final_authority_rejects_static_property_only_atoms_in_metadata() {
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::PushI32, Operands::I32(1)),
        (
            FinalOpcode::DefineField,
            Operands::Atom(AtomPoolIndex::new(0)),
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let flow = flow(&instructions, 1, 0, 0, &[], 0, &[]);
    let graph = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(
            FunctionTemplateId::new(0),
            Arc::from([UnverifiedCompilerFunction::new(
                Arc::clone(&flow),
                Arc::from([]),
                Arc::from([]),
            )
            .with_atom_pool(Arc::from([static_property_only_atom("")]))]),
        ),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("the exceptional atom has one certified static-property use");
    let text: Arc<str> = Arc::from(r#"function f(){return {"":1};}"#);
    let metadata = UnverifiedFunctionMetadata::new(
        Some(AtomPoolIndex::new(0)),
        Arc::from([]),
        Arc::from([]),
        source_for_flow(
            &text,
            &flow,
            SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length")),
            SourceByteSpan::new(9, 10),
        ),
    );

    let error = verify_compiler_bytecode_graph(
        UnverifiedCompilerBytecodeGraph::new(Arc::new(graph), Arc::from([metadata])),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a property-only atom cannot become a function name");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::StaticPropertyOnlyMetadataAtom {
            field: MetadataAtomField::FunctionName,
            index: 0,
        }
    );
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
fn final_authority_admits_array_from_with_a_sorted_array_requirement() {
    let instructions = [
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 2 }),
        (FinalOpcode::Return, Operands::None),
    ];

    let verified = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a dense array consumes its exact input values and gains array authority");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Numbers,
            ExecutionRequirement::Arrays,
        ]
    );
    assert!(
        verified
            .requirements()
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "requirements remain sorted and deduplicated"
    );
}

#[test]
fn final_authority_admits_only_compiler_shaped_array_append_pairs() {
    let instructions = [
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
        (FinalOpcode::Push2, Operands::NoneInt),
        (
            FinalOpcode::DefineField,
            Operands::Atom(AtomPoolIndex::new(0)),
        ),
        (FinalOpcode::Push4, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Inc, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];

    let verified = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[static_property_only_atom("2")], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a sparse prefix and dynamic suffix retain one checked array append pair");

    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::Iterators)
    );
}

#[test]
fn final_authority_admits_only_the_trailing_elision_dup1_length_shape() {
    let instructions = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Inc, Operands::None),
        (FinalOpcode::Dup1, Operands::None),
        (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(0))),
        (FinalOpcode::Return, Operands::None),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("length")], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("dup1 may only finalize the exact trailing-elision array length shape");

    let hole_before_initial_spread = [
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
        (FinalOpcode::Push3, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Dup1, Operands::None),
        (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(0))),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&hole_before_initial_spread, &[atom("length")], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("an initial cursor gap retains the pending trailing elision across append");

    let hole_before_later_spread = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Inc, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Dup1, Operands::None),
        (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(0))),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&hole_before_later_spread, &[atom("length")], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a pending trailing elision survives every following spread");

    let forged = [
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Dup1, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&forged, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("dup1 remains closed outside trailing append length finalization");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::AppendOperandStackMismatch {
            opcode: FinalOpcode::Dup1,
            ..
        }
    ));

    let no_trailing_elision = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Dup1, Operands::None),
        (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(0))),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&no_trailing_elision, &[atom("length")], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("dup1 requires a cursor advanced by an actual trailing elision");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::AppendOperandStackMismatch {
            opcode: FinalOpcode::Dup1,
            ..
        }
    ));
}

#[test]
fn final_authority_admits_pair_duplication_with_nonescaping_values() {
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::Dup2, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("dup2 duplicates an ordinary operand pair without exposing authority");
}

#[test]
fn final_authority_rejects_forged_moved_or_aliased_array_append_pairs() {
    let cases = [
        vec![
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Swap, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Add, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
    ];

    for instructions in cases {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("append requires one exact unaliased destination/cursor pair");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::AppendOperandStackMismatch {
                    opcode: FinalOpcode::Append,
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn final_authority_admits_pure_swap_rotations_preserving_the_append_pair() {
    // `swap` is a pure stack rotation (the object-rest exclude-list lowering
    // rotates the destination/source pair through it), so a double swap that
    // restores the exact destination/cursor order remains an unaliased append
    // pair. A single swap, which separates the pair, is rejected above.
    let instructions = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a double swap is an identity rotation of the append pair");
}

#[test]
fn final_authority_rejects_append_cursor_without_its_required_increment() {
    let instructions = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("define_array_el must be followed by the compiler-owned cursor increment");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::AppendOperandStackMismatch {
            opcode: FinalOpcode::Undefined,
            ..
        }
    ));

    let erased_cursor = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&erased_cursor, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the required increment cannot be erased before returning the destination");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::AppendOperandStackMismatch {
            opcode: FinalOpcode::Drop,
            ..
        }
    ));
}

#[test]
fn final_authority_rejects_append_provenance_at_a_terminal() {
    let instructions = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            encode(&instructions),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect_err("the staged gate rejects append provenance before it can reach a terminal");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::NonEmptyCompilerExitStack { remaining: 2 }
    );
}

#[test]
fn final_authority_allows_generator_return_to_abandon_append_provenance() {
    let instructions = [
        (FinalOpcode::InitialYield, Operands::None),
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ReturnAsync, Operands::None),
    ];
    let text = "function* f(){}";
    let span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let input = profiled_single_input(
        &instructions,
        UnverifiedFunctionHeader::generator_source_function_with_variable_references(false, 0, 0),
        CompilerExecutableKind::GeneratorFunction,
        &[atom("f")],
        Some(AtomPoolIndex::new(0)),
        &[],
        0,
        0,
        &[],
        source(
            text,
            span,
            Some(SourceByteSpan::new(10, 11)),
            &[(0, span), (1, span), (4, span), (5, span), (6, span)],
        ),
    );
    verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect("generator return discards the suspended array-append state");
}

#[test]
fn final_authority_rejects_linear_append_provenance_laundering() {
    let cases = [
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
    ];

    for instructions in cases {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("linear append ownership cannot be copied, collapsed, or dropped pending");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::AppendOperandStackMismatch { .. }
            ),
            "{error:?}"
        );
    }

    let collapsed_pair = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&collapsed_pair, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("nip cannot collapse a live append destination/cursor pair");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ForInIteratorStackMismatch {
            opcode: FinalOpcode::Nip,
            ..
        }
    ));

    let variable = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        var_policy(),
        false,
        None,
    );
    let stored_cursor = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&stored_cursor, &[atom("local")], &[variable]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an append cursor cannot be laundered through frame storage");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::AppendOperandStackMismatch {
                opcode: FinalOpcode::PutLoc0,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn final_authority_rejects_linear_append_provenance_join_laundering() {
    let instructions = [
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Append, Operands::None),
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse, Operands::Label(10)),
        (FinalOpcode::Inc, Operands::None),
        (FinalOpcode::Goto, Operands::Label(5)),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("joins cannot merge checked and pending-elision append ownership");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::AppendProvenanceJoinMismatch { .. }
    ));
}

#[test]
fn execution_requirement_capacity_matches_the_exhaustive_sorted_family_set() {
    let requirements = [
        ExecutionRequirement::CoreValues,
        ExecutionRequirement::Numbers,
        ExecutionRequirement::Strings,
        ExecutionRequirement::BigInts,
        ExecutionRequirement::Closures,
        ExecutionRequirement::Arrays,
        ExecutionRequirement::Iterators,
        ExecutionRequirement::OrdinaryObjects,
        ExecutionRequirement::DynamicPropertyKeys,
        ExecutionRequirement::Calls,
        ExecutionRequirement::AbruptCompletions,
        ExecutionRequirement::LexicalBindings,
        ExecutionRequirement::RealmGlobalBindings,
        ExecutionRequirement::ObjectOperators,
        ExecutionRequirement::DynamicOperators,
    ];

    assert_eq!(requirements.len(), EXECUTION_REQUIREMENT_COUNT);
    assert!(requirements.windows(2).all(|pair| pair[0] < pair[1]));
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
fn finally_return_address_certificate_accepts_shared_and_nested_subroutines() {
    let shared = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(10)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(15)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(8)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    let shared = verify_compiler_bytecode_graph(
        typed_stack_input(&shared, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("two callers may share one typed finalizer");
    assert!(
        shared
            .requirements()
            .contains(&ExecutionRequirement::AbruptCompletions)
    );

    let nested = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Ret, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&nested, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("an inner finalizer resumes an outer typed finalizer");
}

#[test]
fn finally_return_address_certificate_rejects_marker_misuse_and_ordinary_entry() {
    for (opcode, operands) in [
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Throw, Operands::None),
    ] {
        let instructions = [
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Gosub, Operands::Label(6)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
            (opcode, operands),
            (FinalOpcode::Ret, Operands::None),
        ];
        let variable = VariableDefinition::new(
            Some(AtomPoolIndex::new(0)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        );
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[atom("x")], &[variable]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("ordinary bytecode cannot consume or rearrange a finally return marker");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::FinallyReturnStackMismatch {
                    opcode: rejected,
                    ..
                } if *rejected == opcode
            ),
            "{opcode:?}: {error:?}"
        );
    }

    let ordinary_entry = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(9)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(10)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::Ret, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&ordinary_entry, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an ordinary branch cannot enter a certified finalizer target");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::FinallyReturnJoinMismatch { .. }
        ),
        "{error:?}"
    );

    let certified_cleanup = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&certified_cleanup, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a paired drop sequence may discard an overridden finally continuation");
}

#[test]
fn finally_return_address_certificate_rejects_missing_and_partial_pairs() {
    for (instructions, rejected) in [
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::Ret, Operands::None),
            ],
            FinalOpcode::Ret,
        ),
        (
            vec![
                (FinalOpcode::Gosub, Operands::Label(5)),
                (FinalOpcode::ReturnUndef, Operands::None),
                (FinalOpcode::Ret, Operands::None),
            ],
            FinalOpcode::Gosub,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::Gosub, Operands::Label(6)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Dup, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            FinalOpcode::Dup,
        ),
    ] {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("a finally return requires one adjacent typed pending/return pair");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::FinallyReturnStackMismatch {
                    opcode,
                    ..
                } if *opcode == rejected
            ),
            "{rejected:?}: {error:?}"
        );
    }

    let partial_exit = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&partial_exit, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("dropping only the return marker cannot leak its typed pending value");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { .. }
    ));
}

#[test]
fn finally_return_address_certificate_allows_only_an_inert_unreachable_ret() {
    verify_compiler_bytecode_graph(
        typed_stack_input(
            &[
                (FinalOpcode::ReturnUndef, Operands::None),
                (FinalOpcode::Ret, Operands::None),
            ],
            &[],
            &[],
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("an empty structurally unreachable trailing ret is inert compiler output");

    for instructions in [
        vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Ret, Operands::None),
        ],
        vec![
            (FinalOpcode::ReturnUndef, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Ret, Operands::None),
        ],
    ] {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("reachable or nonempty ret still requires an exact pending/return pair");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::FinallyReturnStackMismatch {
                    opcode: FinalOpcode::Ret,
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn compiler_return_cleanup_nips_only_the_adjacent_finally_pair() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("two exact nip steps remove the adjacent return marker then pending value");
}

#[test]
fn finally_abrupt_exits_discard_only_complete_typed_pairs() {
    for opcode in [
        FinalOpcode::Return,
        FinalOpcode::ReturnUndef,
        FinalOpcode::Throw,
    ] {
        let mut instructions = vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Gosub, Operands::Label(6)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ];
        if opcode != FinalOpcode::ReturnUndef {
            instructions.push((FinalOpcode::Push1, Operands::NoneInt));
        }
        instructions.push((opcode, Operands::None));
        verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("a finalizer abrupt completion discards its complete pending/return pair");
    }

    let through_catch = [
        (FinalOpcode::Catch, Operands::Label(13)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(9)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&through_catch, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("nip_catch may discard complete finally pairs above the nearest catch marker");

    let unrelated_nonempty_exit = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(9)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(9)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&unrelated_nonempty_exit, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a gosub elsewhere cannot authorize an unrelated nonempty return");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::FinallyReturnStackMismatch {
                opcode: FinalOpcode::Return,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn nip_catch_cannot_hide_a_following_typed_stack_underflow() {
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(13)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(9)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Add, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the certified variable-width nip_catch leaves only one value for add");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForInIteratorStackMismatch {
                opcode: FinalOpcode::Add,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn nip_catch_cannot_hide_an_empty_drop_on_an_effective_path() {
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(13)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(9)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the second effective drop has no typed value after variable-width cleanup");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForInIteratorStackMismatch {
                opcode: FinalOpcode::Drop,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn object_provenance_uses_the_certified_nip_catch_finally_transform() {
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::Catch, Operands::Label(15)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(13)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("fresh-object and converted-key provenance survive certified finally cleanup");
}

#[test]
fn effective_finally_edges_feed_binding_and_object_provenance_certificates() {
    let variable = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        let_policy(),
        true,
        None,
    );
    let initializes_in_finalizer = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(7)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::GetLoc0, Operands::NoneLoc),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Ret, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&initializes_in_finalizer, &[atom("x")], &[variable]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the continuation observes finalizer binding effects");

    let defines_method_after_finalizer = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(14)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
        ),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        define_method_input(
            &defines_method_after_finalizer,
            CompilerExecutableKind::OrdinaryMethod,
            0,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("object provenance crosses a physical finally marker and its certified return");
}

#[test]
fn finally_return_address_certificate_charges_exact_state_and_transfer_budgets() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Ret, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    let input = typed_stack_input(&instructions, &[], &[]);
    let usage =
        verify_compiler_bytecode_graph(input.clone(), BytecodeGraphVerificationLimits::default())
            .expect("baseline nested-finally certificate")
            .usage();
    assert!(usage.frame_state_entries() > 0);
    assert!(usage.policy_transfers() > 0);

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
fn final_authority_rejects_more_than_the_compatibility_gosub_site_cap() {
    let rejected_sites = usize::try_from(MAX_GOSUB_SITES_PER_FUNCTION).expect("gosub cap") + 1;
    let mut instructions = Vec::with_capacity(rejected_sites * 5);
    for _ in 0..rejected_sites {
        instructions.extend_from_slice(&[
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::Gosub, Operands::Label(6)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
            (FinalOpcode::Ret, Operands::None),
        ]);
    }

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the pinned per-function gosub-site compatibility cap is mandatory");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::GosubSiteCountOutOfRange {
            sites,
            maximum: MAX_GOSUB_SITES_PER_FUNCTION,
        } if *sites == u64::from(MAX_GOSUB_SITES_PER_FUNCTION) + 1
    ));
}

#[test]
fn final_authority_admits_direct_calls_and_records_the_requirement() {
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
fn final_authority_admits_ordinary_object_properties_and_method_calls() {
    let text = "function f(argument){var local;return undefined}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
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
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (
            FinalOpcode::DefineField,
            Operands::Atom(AtomPoolIndex::new(3)),
        ),
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::GetField, Operands::Atom(AtomPoolIndex::new(3))),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Insert2, Operands::None),
        (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(3))),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::GetField2,
            Operands::Atom(AtomPoolIndex::new(3)),
        ),
        (
            FinalOpcode::CallMethod,
            Operands::NPop { argument_count: 0 },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let pcs = [0, 1, 2, 7, 8, 13, 14, 15, 16, 21, 22, 23, 28, 31];

    let verified = verified_single(
        &instructions,
        &[atom("f"), atom("argument"), atom("local"), atom("value")],
        &variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &pcs.map(|pc| (pc, function_span)),
        ),
    )
    .expect("ordinary static property and method-call opcodes gain final authority");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Numbers,
            ExecutionRequirement::Strings,
            ExecutionRequirement::OrdinaryObjects,
            ExecutionRequirement::Calls,
        ]
    );
}

#[test]
fn final_authority_admits_computed_properties_and_records_dynamic_keys() {
    let text = "function f(argument){var local;return undefined}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
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
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(3)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Dup, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(3)),
        ),
        (FinalOpcode::GetArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Dup, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(3)),
        ),
        (FinalOpcode::GetArrayEl2, Operands::None),
        (
            FinalOpcode::CallMethod,
            Operands::NPop { argument_count: 0 },
        ),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Dup, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(3)),
        ),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Insert3, Operands::None),
        (FinalOpcode::PutArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let pcs = [
        0, 1, 6, 7, 8, 9, 10, 11, 16, 17, 18, 19, 24, 25, 28, 29, 30, 35, 36, 37, 38, 39,
    ];

    let verified = verified_single(
        &instructions,
        &[atom("f"), atom("argument"), atom("local"), atom("key")],
        &variables,
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &pcs.map(|pc| (pc, function_span)),
        ),
    )
    .expect("the bounded computed-property opcode family gains final authority");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Numbers,
            ExecutionRequirement::Strings,
            ExecutionRequirement::OrdinaryObjects,
            ExecutionRequirement::DynamicPropertyKeys,
            ExecutionRequirement::Calls,
        ]
    );
}

#[test]
fn final_authority_requires_object_data_keys_to_be_converted_before_the_value() {
    let text = "function f(argument){var local;return undefined}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
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
    let cases = [
        (
            vec![
                (FinalOpcode::Object, Operands::None),
                (
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(AtomPoolIndex::new(3)),
                ),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::DefineArrayEl, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            vec![0, 1, 6, 7, 8, 9],
        ),
        (
            vec![
                (FinalOpcode::Object, Operands::None),
                (FinalOpcode::Push1, Operands::NoneInt),
                (
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(AtomPoolIndex::new(3)),
                ),
                (FinalOpcode::ToPropKey, Operands::None),
                (FinalOpcode::Insert2, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::DefineArrayEl, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            vec![0, 1, 2, 7, 8, 9, 10, 11, 12],
        ),
        (
            vec![
                (FinalOpcode::Object, Operands::None),
                (
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(AtomPoolIndex::new(3)),
                ),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::Insert3, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ToPropKey, Operands::None),
                (FinalOpcode::Insert3, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Insert3, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::DefineArrayEl, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            vec![0, 1, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        ),
    ];

    for (instructions, pcs) in cases {
        let error = verified_single(
            &instructions,
            &[atom("f"), atom("argument"), atom("local"), atom("key")],
            &variables,
            source(
                text,
                function_span,
                Some(SourceByteSpan::new(9, 10)),
                &pcs.into_iter()
                    .map(|pc| (pc, function_span))
                    .collect::<Vec<_>>(),
            ),
        )
        .expect_err("define_array_el requires a key converted before its value");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::DefineArrayElementKeyMismatch { .. }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn final_authority_admits_named_evaluation_for_one_fresh_anonymous_closure() {
    let instructions = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::SetName, Operands::Atom(AtomPoolIndex::new(1))),
        (FinalOpcode::Return, Operands::None),
    ];
    let verified = verify_compiler_bytecode_graph(
        define_method_input(&instructions, CompilerExecutableKind::OrdinaryFunction, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("one adjacent anonymous ordinary closure gains named-evaluation authority");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Closures,
            ExecutionRequirement::OrdinaryObjects,
        ]
    );
}

#[test]
fn final_authority_admits_computed_named_evaluation_only_for_its_data_definition() {
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::SetNameComputed, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let verified = verify_compiler_bytecode_graph(
        define_method_input(&instructions, CompilerExecutableKind::OrdinaryFunction, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("one computed data property gains exact named-evaluation authority");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Closures,
            ExecutionRequirement::OrdinaryObjects,
            ExecutionRequirement::DynamicPropertyKeys,
        ]
    );
}

#[test]
fn final_authority_admits_only_a_fresh_class_computed_name_sequence() {
    let valid = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineClass,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 0,
            },
        ),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Perm3, Operands::None),
        (FinalOpcode::SetNameComputed, Operands::None),
        (FinalOpcode::Perm3, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let verified = verify_compiler_bytecode_graph(
        define_method_input(&valid, CompilerExecutableKind::ClassConstructor, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("one fresh class gains computed named-evaluation authority");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::DynamicPropertyKeys)
    );

    let nonadjacent = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineClass,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 0,
            },
        ),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Perm3, Operands::None),
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::SetNameComputed, Operands::None),
        (FinalOpcode::Perm3, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&nonadjacent, CompilerExecutableKind::ClassConstructor, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("computed class naming cannot target an older class value");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));
}

#[test]
fn final_authority_rejects_unpaired_or_method_set_name_operands() {
    let nonadjacent = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::SetName, Operands::Atom(AtomPoolIndex::new(1))),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&nonadjacent, CompilerExecutableKind::OrdinaryFunction, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("set_name cannot target an older stack function");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));

    let joined = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::SetName, Operands::Atom(AtomPoolIndex::new(1))),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&joined, CompilerExecutableKind::OrdinaryFunction, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a branch cannot enter an otherwise adjacent set_name pair");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));

    let method = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::SetName, Operands::Atom(AtomPoolIndex::new(1))),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&method, CompilerExecutableKind::OrdinaryMethod, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("set_name cannot stand in for define_method");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));
}

#[test]
fn final_authority_rejects_invalid_computed_set_name_shapes() {
    let nonadjacent_computed = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::SetNameComputed, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(
            &nonadjacent_computed,
            CompilerExecutableKind::OrdinaryFunction,
            0,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("set_name_computed cannot target an older stack function");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));

    let detached_computed = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::SetNameComputed, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(
            &detached_computed,
            CompilerExecutableKind::OrdinaryFunction,
            0,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("set_name_computed cannot detach from its data-property definition");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));

    let computed_method = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::ToPropKey, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::SetNameComputed, Operands::None),
        (FinalOpcode::DefineArrayEl, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&computed_method, CompilerExecutableKind::OrdinaryMethod, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("set_name_computed cannot rename a method template");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::SetNameTemplateMismatch { .. }
    ));
}

#[test]
fn final_authority_admits_enumerable_object_literal_method_kinds() {
    for (flags, arguments) in [(4, 0), (5, 0), (6, 1)] {
        let instructions = [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(1),
                    value: flags,
                },
            ),
            (FinalOpcode::Return, Operands::None),
        ];
        let verified = verify_compiler_bytecode_graph(
            define_method_input(
                &instructions,
                CompilerExecutableKind::OrdinaryMethod,
                arguments,
            ),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("the compiler's method, getter, and setter flags gain authority");

        assert_eq!(
            verified.requirements(),
            [
                ExecutionRequirement::CoreValues,
                ExecutionRequirement::Strings,
                ExecutionRequirement::Closures,
                ExecutionRequirement::OrdinaryObjects,
            ]
        );
        let child = verified
            .function(FunctionTemplateId::new(1))
            .expect("method child");
        assert_eq!(
            child.metadata().executable_kind(),
            CompilerExecutableKind::OrdinaryMethod
        );
        assert!(
            !child
                .function()
                .control_flow()
                .function_header()
                .flags()
                .has_prototype()
        );
    }
}

#[test]
fn final_authority_admits_only_typed_base_class_templates() {
    let base_class = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineClass,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 0,
            },
        ),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let verified = verify_compiler_bytecode_graph(
        define_method_input(&base_class, CompilerExecutableKind::ClassConstructor, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a base class consumes one typed strict constructor template");

    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Closures,
            ExecutionRequirement::OrdinaryObjects,
        ]
    );
    let child = verified
        .function(FunctionTemplateId::new(1))
        .expect("class constructor child");
    assert_eq!(
        child.metadata().executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
    assert!(
        !child
            .function()
            .control_flow()
            .function_header()
            .flags()
            .has_prototype(),
        "define_class, rather than the function header, owns the class prototype"
    );

    let arbitrary_parent = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineClass,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 0,
            },
        ),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(
            &arbitrary_parent,
            CompilerExecutableKind::ClassConstructor,
            0,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a base class cannot substitute an arbitrary superclass input");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineClassTemplateMismatch { .. }
        ),
        "{error:?}"
    );

    let escaping_template = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(
            &escaping_template,
            CompilerExecutableKind::ClassConstructor,
            0,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a class constructor template cannot escape without define_class");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineClassTemplateMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn final_authority_rejects_non_enumerable_object_literal_method_definitions() {
    for flags in 0..=2 {
        for (opcode, operands) in [
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(1),
                    value: flags,
                },
            ),
            (FinalOpcode::DefineMethodComputed, Operands::U8(flags)),
        ] {
            let mut instructions = vec![(FinalOpcode::Object, Operands::None)];
            if opcode == FinalOpcode::DefineMethodComputed {
                instructions.push((
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(AtomPoolIndex::new(1)),
                ));
            }
            instructions.extend([
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (opcode, operands),
                (FinalOpcode::Return, Operands::None),
            ]);
            let error = verify_compiler_bytecode_graph(
                define_method_input(
                    &instructions,
                    CompilerExecutableKind::OrdinaryMethod,
                    u32::from(flags == 2),
                ),
                BytecodeGraphVerificationLimits::default(),
            )
            .expect_err("non-enumerable flags require a certified class target");
            assert!(matches!(
                error.kind(),
                BytecodeVerificationErrorKind::DefineMethodTargetMismatch { .. }
            ));
        }
    }
}

#[test]
fn final_authority_admits_typed_computed_method_kinds() {
    for (flags, arguments) in [(4, 0), (5, 0), (6, 1)] {
        let computed = [
            (FinalOpcode::Object, Operands::None),
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::DefineMethodComputed, Operands::U8(flags)),
            (FinalOpcode::Return, Operands::None),
        ];
        let verified = verify_compiler_bytecode_graph(
            define_method_input(&computed, CompilerExecutableKind::OrdinaryMethod, arguments),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("computed method, getter, and setter definitions gain authority");
        assert_eq!(
            verified.requirements(),
            [
                ExecutionRequirement::CoreValues,
                ExecutionRequirement::Strings,
                ExecutionRequirement::Closures,
                ExecutionRequirement::OrdinaryObjects,
                ExecutionRequirement::DynamicPropertyKeys,
            ]
        );
    }
}

#[test]
fn final_authority_rejects_untyped_or_nonadjacent_computed_method_closures() {
    let wrong_kind = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::DefineMethodComputed, Operands::U8(4)),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&wrong_kind, CompilerExecutableKind::OrdinaryFunction, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a computed method requires an ordinary-method child");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineMethodTemplateMismatch { .. }
        ),
        "{error:?}"
    );

    let nonadjacent = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::DefineMethodComputed, Operands::U8(4)),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&nonadjacent, CompilerExecutableKind::OrdinaryMethod, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the method closure must immediately precede its computed definition");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn final_authority_rejects_nonfresh_or_multiply_owned_computed_method_targets() {
    let nonfresh = [
        (FinalOpcode::GetArg0, Operands::NoneArg),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::DefineMethodComputed, Operands::U8(4)),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input_with_root_arguments(
            &nonfresh,
            CompilerExecutableKind::OrdinaryMethod,
            0,
            1,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a computed method cannot mutate an arbitrary argument");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineMethodTargetMismatch { .. }
        ),
        "{error:?}"
    );

    let duplicate = [
        (FinalOpcode::Object, Operands::None),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::DefineMethodComputed, Operands::U8(4)),
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(1)),
        ),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::DefineMethodComputed, Operands::U8(4)),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&duplicate, CompilerExecutableKind::OrdinaryMethod, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("one method template cannot back two computed definition sites");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::OrdinaryMethodTemplateOwnershipMismatch {
                definitions: 2,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn final_authority_requires_a_typed_method_closure_and_accessor_arity() {
    let method = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&method, CompilerExecutableKind::OrdinaryFunction, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an ordinary constructable function cannot back define_method");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineMethodTemplateMismatch { .. }
        ),
        "{error:?}"
    );

    let getter_with_argument = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 5,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(
            &getter_with_argument,
            CompilerExecutableKind::OrdinaryMethod,
            1,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a getter must have zero formal parameters");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch { .. }
        ),
        "{error:?}"
    );

    let unconsumed = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input(&unconsumed, CompilerExecutableKind::OrdinaryMethod, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a method closure cannot escape its definition site");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch { .. }
    ));
}

#[test]
fn final_authority_requires_define_method_to_target_one_fresh_literal_object() {
    let source_ordered = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (
            FinalOpcode::DefineField,
            Operands::Atom(AtomPoolIndex::new(0)),
        ),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let source_ordered_input =
        define_method_input(&source_ordered, CompilerExecutableKind::OrdinaryMethod, 0);
    let verified = verify_compiler_bytecode_graph(
        source_ordered_input.clone(),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("source-ordered data and method definitions preserve one fresh literal target");
    let usage = verified.usage();
    assert_eq!(
        usage.frame_state_entries(),
        7,
        "fresh-object entry states are charged to the aggregate frame-state budget"
    );
    assert_eq!(
        usage.policy_transfers(),
        25,
        "fresh-object state visits are charged to the aggregate transfer-work budget"
    );
    assert_limit(
        &source_ordered_input,
        BytecodeGraphVerificationLimits::default()
            .with_max_frame_state_entries(usage.frame_state_entries()),
        BytecodeGraphVerificationLimits::default()
            .with_max_frame_state_entries(usage.frame_state_entries() - 1),
        BytecodeGraphResource::FrameStateEntries,
        usage.frame_state_entries() - 1,
        usage.frame_state_entries(),
    );
    assert_limit(
        &source_ordered_input,
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
fn final_authority_retains_one_fresh_literal_target_across_copy_data_properties() {
    let instructions = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::CopyDataProperties, Operands::U8(0b0000_0110)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        define_method_input(&instructions, CompilerExecutableKind::OrdinaryMethod, 0),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("object-spread copy preserves the fresh target for a later accessor definition");
}

#[test]
fn final_authority_rejects_copy_data_properties_into_an_argument() {
    let instructions = [
        (FinalOpcode::GetArg0, Operands::NoneArg),
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::CopyDataProperties, Operands::U8(0b0000_0110)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input_with_root_arguments(
            &instructions,
            CompilerExecutableKind::OrdinaryFunction,
            0,
            1,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("copy_data_properties cannot mutate an arbitrary argument");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineMethodTargetMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn final_authority_preserves_one_fresh_method_target_across_a_field_value_join() {
    let same_literal_across_join = [
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Goto8, Operands::Label8(2)),
        (FinalOpcode::Push2, Operands::NoneInt),
        (
            FinalOpcode::DefineField,
            Operands::Atom(AtomPoolIndex::new(0)),
        ),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        define_method_input(
            &same_literal_across_join,
            CompilerExecutableKind::OrdinaryMethod,
            0,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a join preserves the same fresh literal while selecting one field value");
}

#[test]
fn final_authority_rejects_argument_and_primitive_define_method_targets() {
    for hostile in [
        vec![
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(1),
                    value: 4,
                },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(1),
                    value: 4,
                },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
    ] {
        let error = verify_compiler_bytecode_graph(
            define_method_input_with_root_arguments(
                &hostile,
                CompilerExecutableKind::OrdinaryMethod,
                0,
                1,
            ),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("a method cannot target an argument or primitive value");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::DefineMethodTargetMismatch { .. }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn final_authority_rejects_mixed_define_method_target_provenance_at_a_join() {
    let mixed = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(2)),
        (FinalOpcode::GetArg0, Operands::NoneArg),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        define_method_input_with_root_arguments(
            &mixed,
            CompilerExecutableKind::OrdinaryMethod,
            0,
            1,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a join cannot mix a fresh literal with an arbitrary argument target");

    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::DefineMethodTargetMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn push_this_authority_covers_normalized_functions_and_script_roots() {
    let instructions = [
        (FinalOpcode::PushThis, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    let text = "function f(){\"use strict\";return this}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let ordinary_source = || {
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[(0, function_span), (1, function_span)],
        )
    };
    let strict = shaped_input_with_strict(
        &instructions,
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        ordinary_source(),
        true,
    );
    let verified =
        verify_compiler_bytecode_graph(strict, BytecodeGraphVerificationLimits::default())
            .expect("strict functions may load their raw receiver");
    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Calls,
        ]
    );

    let sloppy = shaped_input_with_strict(
        &instructions,
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        ordinary_source(),
        false,
    );
    let verified =
        verify_compiler_bytecode_graph(sloppy, BytecodeGraphVerificationLimits::default())
            .expect("sloppy functions receive their normalized receiver at call entry");
    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Calls,
        ]
    );

    let script_text = "return this";
    let script_span =
        SourceByteSpan::new(0, u32::try_from(script_text.len()).expect("source length"));
    let dynamic_script = profiled_single_input(
        &instructions,
        UnverifiedFunctionHeader::dynamic_function_script(0),
        CompilerExecutableKind::DynamicFunctionScript,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(
            script_text,
            script_span,
            None,
            &[(0, script_span), (1, script_span)],
        ),
    );
    let verified =
        verify_compiler_bytecode_graph(dynamic_script, BytecodeGraphVerificationLimits::default())
            .expect(
                "a Dynamic Function Script may forward its ordinary-call receiver to its child",
            );
    assert_eq!(
        verified.root().metadata().executable_kind(),
        CompilerExecutableKind::DynamicFunctionScript
    );
}

#[test]
fn final_authority_admits_constructor_calls_and_records_the_requirement() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Dup, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (
            FinalOpcode::CallConstructor,
            Operands::NPop { argument_count: 2 },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let text = "function f(argument){var local;return new argument(1,2)}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
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
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, function_span),
                (1, function_span),
                (2, function_span),
                (3, function_span),
                (4, function_span),
                (7, function_span),
            ],
        ),
    )
    .expect("ordinary constructor calls gain final authority");

    assert_eq!(
        verified
            .root()
            .function()
            .control_flow()
            .computed_stack_size(),
        4
    );
    assert_eq!(
        verified.requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Numbers,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Calls,
        ]
    );
}

#[test]
fn final_authority_keeps_tail_call_families_fail_closed() {
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

fn typed_stack_input(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
) -> UnverifiedCompilerBytecodeGraph {
    typed_stack_input_with_captures(instructions, atoms, variables, &[])
}

fn typed_stack_input_with_captures(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    captures: &[CompilerCapturedBinding],
) -> UnverifiedCompilerBytecodeGraph {
    let has_direct_eval = instructions
        .iter()
        .any(|(opcode, _)| matches!(opcode, FinalOpcode::Eval | FinalOpcode::ApplyEval));
    let locals = u32::try_from(variables.len()).expect("fixture local count");
    let flow = flow(
        instructions,
        u32::try_from(atoms.len()).expect("fixture atom count"),
        0,
        locals,
        captures,
        0,
        &[],
    );
    let text: Arc<str> = Arc::from("typed stack fixture");
    let span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("fixture source length"));
    let source = CompilerSource::new(
        Arc::from("fixture.js"),
        text,
        span,
        None,
        Arc::from(
            flow.instructions()
                .iter()
                .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), span))
                .collect::<Vec<_>>(),
        ),
    );
    let graph = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(
            FunctionTemplateId::new(0),
            Arc::from([
                UnverifiedCompilerFunction::new(flow, Arc::from([]), Arc::from([]))
                    .with_atom_pool(Arc::from(atoms))
                    .with_direct_eval(has_direct_eval),
            ]),
        ),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("typed stack fixture graph");
    UnverifiedCompilerBytecodeGraph::new(
        Arc::new(graph),
        Arc::from([UnverifiedFunctionMetadata::new(
            None,
            Arc::from(variables),
            Arc::from([]),
            source,
        )]),
    )
}

#[test]
fn compiler_eval_scope_operand_is_tied_to_verified_lexical_metadata() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        let_policy(),
        true,
        None,
    );
    let eval = |scope_index| {
        typed_stack_input(
            &[
                (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
                (FinalOpcode::Push7, Operands::NoneInt),
                (
                    FinalOpcode::Eval,
                    Operands::NPopU16 {
                        argument_count: 0,
                        scope_index,
                    },
                ),
                (FinalOpcode::Return, Operands::None),
            ],
            &[atom("lexical")],
            std::slice::from_ref(&definition),
        )
    };

    verify_compiler_bytecode_graph(eval(2), BytecodeGraphVerificationLimits::default())
        .expect("adjusted scope index two selects lexical local zero");

    let error = verify_compiler_bytecode_graph(eval(3), BytecodeGraphVerificationLimits::default())
        .expect_err("an eval scope head cannot exceed the local metadata domain");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::EvalScopeIndexOutOfBounds {
            scope_index: 3,
            locals: 1,
            ..
        }
    ));

    let function_scoped = typed_stack_input(
        &[
            (FinalOpcode::Push7, Operands::NoneInt),
            (
                FinalOpcode::Eval,
                Operands::NPopU16 {
                    argument_count: 0,
                    scope_index: 2,
                },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("local")],
        &[VariableDefinition::new(
            Some(AtomPoolIndex::new(0)),
            ScopeLink::End,
            var_policy(),
            false,
            None,
        )],
    );
    let error =
        verify_compiler_bytecode_graph(function_scoped, BytecodeGraphVerificationLimits::default())
            .expect_err("the lexical scope chain cannot start at a function-scoped local");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::EvalScopeHeadNotLexical { local: 0, .. }
    ));
}

#[test]
fn compiler_eval_scope_accepts_both_adjusted_sentinels_and_apply_eval() {
    for scope_index in [0, 1] {
        let eval = typed_stack_input(
            &[
                (FinalOpcode::Push7, Operands::NoneInt),
                (
                    FinalOpcode::Eval,
                    Operands::NPopU16 {
                        argument_count: 0,
                        scope_index,
                    },
                ),
                (FinalOpcode::Return, Operands::None),
            ],
            &[],
            &[],
        );
        verify_compiler_bytecode_graph(eval, BytecodeGraphVerificationLimits::default())
            .expect("adjusted eval-scope sentinel");

        let apply_eval = typed_stack_input(
            &[
                (FinalOpcode::Push7, Operands::NoneInt),
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ApplyEval, Operands::U16(scope_index)),
                (FinalOpcode::Return, Operands::None),
            ],
            &[],
            &[],
        );
        verify_compiler_bytecode_graph(apply_eval, BytecodeGraphVerificationLimits::default())
            .expect("apply_eval uses the same adjusted eval-scope sentinel");
    }
}

#[test]
fn catch_binding_requires_the_exact_handler_value_initialization() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        catch_policy(),
        true,
        None,
    );
    let valid = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&valid, &[atom("error")], std::slice::from_ref(&definition)),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the exceptional handler value initializes its exact catch local");

    let optional_binding_shape = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(
            &optional_binding_shape,
            &[atom("error")],
            std::slice::from_ref(&definition),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("catch metadata cannot omit its handler-value initialization");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Local(0),
            reason: BindingPolicyViolationReason::MissingLexicalScopeInitialization,
            ..
        }
    ));

    let unrelated_value = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(
            &unrelated_value,
            &[atom("error")],
            std::slice::from_ref(&definition),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an ordinary handler value cannot initialize catch metadata");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::BindingPolicyViolation {
                slot: BindingSlot::Local(0),
                reason: BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn catch_handler_value_cannot_initialize_a_non_catch_local() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        var_policy(),
        false,
        None,
    );
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("value")], &[definition]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("handler-value initialization authority belongs only to Catch metadata");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Local(0),
            reason: BindingPolicyViolationReason::InvalidLexicalInitialization,
            ..
        }
    ));
}

#[test]
fn catch_handler_value_must_be_consumed_before_block_scope_initialization() {
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(0)),
            ScopeLink::End,
            catch_policy(),
            true,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::Local(0),
            let_policy(),
            true,
            None,
        ),
    ];
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::SetLocUninitialized, Operands::Loc(1)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("error"), atom("lexical")], &variables),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the catch parameter must consume the handler value before block-scope entry");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::BindingPolicyViolation {
                slot: BindingSlot::Local(0),
                reason: BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn catch_marker_certificate_accepts_normal_throw_and_nested_for_in_cleanup() {
    let normal = [
        (FinalOpcode::Catch, Operands::Label(7)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let verified = verify_compiler_bytecode_graph(
        typed_stack_input(&normal, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("nip_catch preserves the normal completion and removes the exact catch marker");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::AbruptCompletions)
    );

    let explicit_throw = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Throw, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&explicit_throw, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("throw consumes the protected value and transfers through the catch marker");

    let catch_outside_for_in = [
        (FinalOpcode::Catch, Operands::Label(10)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&catch_outside_for_in, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("for-in and catch markers are removed inside-out by their distinct cleanup opcodes");

    let for_in_outside_catch = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Catch, Operands::Label(8)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&for_in_outside_catch, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a handler inside a for-in region retains only the enclosing iterator marker");
}

#[test]
fn catch_marker_certificate_rejects_ordinary_values_stranded_by_throw() {
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(7)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Throw, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("throw may retain active catch markers but no ordinary operand values");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::CatchMarkerStackMismatch {
                opcode: FinalOpcode::Throw,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn catch_marker_certificate_rejects_forged_copied_stored_or_crossed_markers() {
    let local = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        var_policy(),
        false,
        None,
    );
    let cases = [
        (
            vec![
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::Push2, Operands::NoneInt),
                (FinalOpcode::NipCatch, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::NipCatch,
        ),
        (
            vec![
                (FinalOpcode::Catch, Operands::Label(8)),
                (FinalOpcode::Dup, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Dup,
        ),
        (
            vec![
                (FinalOpcode::Catch, Operands::Label(6)),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
                (FinalOpcode::ReturnUndef, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            vec![atom("stored")],
            vec![local],
            FinalOpcode::PutLoc0,
        ),
        (
            vec![
                (FinalOpcode::Catch, Operands::Label(11)),
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::NipCatch, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::NipCatch,
        ),
    ];

    for (instructions, atoms, variables, opcode) in cases {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &atoms, &variables),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("catch markers cannot be forged, copied, stored, or crossed");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::CatchMarkerStackMismatch {
                    opcode: actual,
                    ..
                } if *actual == opcode
            ),
            "{opcode}: {error:?}"
        );
    }
}

#[test]
fn catch_handler_entry_rejects_an_ordinary_control_flow_join() {
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(8)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("ordinary values cannot enter an exceptional handler target");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::CatchMarkerJoinMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn unreachable_ordinary_component_cannot_enter_a_certified_catch_handler() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        catch_policy(),
        true,
        None,
    );
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Goto8, Operands::Label8(-4)),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("error")], &[definition]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an unreachable ordinary edge cannot bypass the typed handler join");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::CatchMarkerJoinMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn compiler_shaped_caught_throw_keeps_dead_for_in_rotation_outside_the_handler_join() {
    let variables = [
        VariableDefinition::new(
            Some(AtomPoolIndex::new(1)),
            ScopeLink::End,
            let_policy(),
            true,
            None,
        ),
        VariableDefinition::new(
            Some(AtomPoolIndex::new(2)),
            ScopeLink::End,
            catch_policy(),
            true,
            None,
        ),
    ];
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(35)),
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (
            FinalOpcode::DefineField,
            Operands::Atom(AtomPoolIndex::new(0)),
        ),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(11)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::GetLocCheck, Operands::Loc(0)),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Throw, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::Goto8, Operands::Label8(-15)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(4)),
        (FinalOpcode::PutLoc1, Operands::NoneLoc),
        (FinalOpcode::GetLoc1, Operands::NoneLoc),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input(
            &instructions,
            &[atom("a"), atom("key"), atom("error")],
            &variables,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect(
        "dead normal-completion rotation after throw may reenter the for-in head, not the Catch handler",
    );
}

#[test]
fn catch_marker_certificate_checks_unreachable_components_and_terminal_markers() {
    let forged = [
        (FinalOpcode::Goto8, Operands::Label8(5)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&forged, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an unreachable component cannot forge a catch marker");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::CatchMarkerStackMismatch {
            opcode: FinalOpcode::NipCatch,
            ..
        }
    ));

    let marker_exit = [
        (FinalOpcode::Goto8, Operands::Label8(9)),
        (FinalOpcode::Catch, Operands::Label(5)),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&marker_exit, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an unreachable terminal cannot retain a catch marker");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::CatchMarkerAtExit { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn disconnected_catch_component_cannot_hide_a_marker_in_an_earlier_component() {
    let instructions = [
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Catch, Operands::Label(7)),
        (FinalOpcode::Goto8, Operands::Label8(-7)),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a cross-component edge cannot hide a live catch marker");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::CatchMarkerJoinMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn disconnected_for_in_component_cannot_hide_a_marker_in_an_earlier_component() {
    let instructions = [
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-4)),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a cross-component edge cannot hide a live for-in marker");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForInIteratorJoinMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn catch_marker_certificate_charges_exact_state_and_transfer_budgets() {
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(7)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let input = typed_stack_input(&instructions, &[], &[]);
    let usage =
        verify_compiler_bytecode_graph(input.clone(), BytecodeGraphVerificationLimits::default())
            .expect("baseline catch marker certificate")
            .usage();
    assert!(usage.frame_state_entries() > 0);
    assert!(usage.policy_transfers() > 0);

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
fn for_in_marker_certificate_accepts_a_loop_and_exact_cleanup() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-8)),
    ];

    let verified = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("for-in keeps its typed iterator through the loop and drops it on exit");

    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::OrdinaryObjects)
    );
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::Strings)
    );
}

#[test]
fn for_in_marker_certificate_accepts_nested_nip_cleanup_and_ordinary_rotations() {
    let nested = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Nip, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&nested, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("nested markers are removed inside-out while preserving an ordinary completion");

    for ordinary in [
        vec![
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Swap, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        vec![
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Push3, Operands::NoneInt),
            (FinalOpcode::Rot3l, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
    ] {
        verify_compiler_bytecode_graph(
            typed_stack_input(&ordinary, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("ordinary assignment-target rotations remain in the compiler profile");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the adversarial cases cover each forbidden marker escape class in one table"
)]
fn for_in_marker_certificate_rejects_forged_or_exposed_iterators() {
    let local = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        var_policy(),
        false,
        None,
    );
    let cases = [
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInNext, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::ForInNext,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::Dup, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Dup,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::Swap, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Swap,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::Push2, Operands::NoneInt),
                (FinalOpcode::Rot3l, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Rot3l,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Return,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            vec![atom("stored")],
            vec![local.clone()],
            FinalOpcode::PutLoc0,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForInStart, Operands::None),
                (FinalOpcode::Call0, Operands::NPopX),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Call0,
        ),
    ];

    for (instructions, atoms, variables, opcode) in cases {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &atoms, &variables),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err(
            "a for-in iterator marker cannot be forged, copied, stored, called, or returned",
        );
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::ForInIteratorStackMismatch {
                    opcode: actual,
                    ..
                } if *actual == opcode
            ),
            "{opcode}: {error:?}"
        );
    }
}

#[test]
fn for_in_marker_certificate_requires_exact_join_identity() {
    let instructions = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(5)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(3)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("different for-in iterator sites cannot merge into one stack slot");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForInIteratorJoinMismatch { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn marker_free_dead_gosub_can_reuse_an_already_verified_finalizer() {
    let instructions = [
        (FinalOpcode::Catch, Operands::Label(13)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(18)),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(7)),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect(
        "a marker-free dead component may reuse a finalizer verified under the live catch prefix",
    );
}

#[test]
fn for_in_marker_certificate_rejects_crossed_and_marker_free_nip() {
    let cases = [
        vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::ForInStart, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::ForInStart, Operands::None),
            (FinalOpcode::Nip, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        vec![
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Nip, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
    ];

    for instructions in cases {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("nip is reserved for inside-out for-in marker cleanup");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::ForInIteratorStackMismatch {
                    opcode: FinalOpcode::Nip,
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn for_in_marker_certificate_checks_unreachable_components() {
    let instructions = [
        (FinalOpcode::Goto8, Operands::Label8(6)),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an unreachable component cannot smuggle a forged iterator opcode");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForInIteratorStackMismatch {
                opcode: FinalOpcode::ForInNext,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn for_in_marker_certificate_stops_dead_scaffolding_at_the_reachable_loop_boundary() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(10)),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-18)),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect(
        "dead normal-rotation scaffolding at PC 19 cannot contaminate reachable ForInNext PC 2",
    );
}

#[test]
fn for_in_head_key_certificate_allows_const_reinitialization_on_loop_backedges() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        const_policy(),
        true,
        None,
    );
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-8)),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("key")], &[definition]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the exact for-in head key may reinitialize one uncaptured const on every iteration");
}

#[test]
fn captured_for_in_const_requires_close_loc_before_certified_reinitialization() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        const_policy(),
        true,
        Some(0),
    );
    let captures = [CompilerCapturedBinding::ScopedLocal(0)];
    let closed = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::CloseLoc, Operands::Loc(0)),
        (FinalOpcode::Goto8, Operands::Label8(-11)),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input_with_captures(
            &closed,
            &[atom("key")],
            std::slice::from_ref(&definition),
            &captures,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("closing the captured cell permits a fresh const binding on the backedge");

    let missing_close = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-8)),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input_with_captures(&missing_close, &[atom("key")], &[definition], &captures),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an initialized captured const cannot reuse its active iteration cell");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Local(0),
            reason: BindingPolicyViolationReason::InvalidLexicalInitialization,
            ..
        }
    ));
}

#[test]
fn captured_for_in_let_declaration_requires_close_loc_before_reinitialization() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        let_policy(),
        true,
        Some(0),
    );
    let captures = [CompilerCapturedBinding::ScopedLocal(0)];
    let closed = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::CloseLoc, Operands::Loc(0)),
        (FinalOpcode::Goto8, Operands::Label8(-11)),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input_with_captures(
            &closed,
            &[atom("key")],
            std::slice::from_ref(&definition),
            &captures,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("closing a captured declarative let cell permits the next iteration binding");

    let missing_close = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-8)),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input_with_captures(&missing_close, &[atom("key")], &[definition], &captures),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a captured declarative let needs a fresh closed cell on the backedge");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Local(0),
            reason: BindingPolicyViolationReason::InvalidLexicalInitialization,
            ..
        }
    ));
}

#[test]
fn captured_outer_let_for_in_assignment_reuses_its_initialized_cell() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        let_policy(),
        true,
        Some(0),
    );
    let captures = [CompilerCapturedBinding::ScopedLocal(0)];
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLocCheck, Operands::Loc(0)),
        (FinalOpcode::Goto8, Operands::Label8(-10)),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input_with_captures(&instructions, &[atom("key")], &[definition], &captures),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("an outer captured let uses checked assignment and keeps the same active cell");
}

#[test]
fn for_in_const_reinitialization_rejects_transformed_or_wrong_edge_keys() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        const_policy(),
        true,
        None,
    );
    let intervening_nop = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-9)),
    ];
    let transformed = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Swap, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-11)),
    ];
    let done_fallthrough = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-5)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    for instructions in [
        intervening_nop.as_slice(),
        transformed.as_slice(),
        done_fallthrough.as_slice(),
    ] {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(
                instructions,
                &[atom("key")],
                std::slice::from_ref(&definition),
            ),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("only the immediate false-edge for-in key consumption can reinitialize const");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::BindingPolicyViolation {
                    slot: BindingSlot::Local(0),
                    reason: BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn for_in_head_key_cannot_reinitialize_a_const_owned_by_another_put_site() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        const_policy(),
        true,
        None,
    );
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-8)),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("key")], &[definition]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a for-in key cannot mutate a const initialized by another unchecked PutLoc site");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Local(0),
            reason: BindingPolicyViolationReason::InvalidLexicalInitialization,
            ..
        }
    ));
}

#[test]
fn for_in_declarative_local_rejects_multiple_iterator_cursor_sites() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        const_policy(),
        true,
        None,
    );
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(5)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(5)),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("key")], &[definition]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("one declarative local cannot take fresh-binding authority from two iterators");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Local(0),
            reason: BindingPolicyViolationReason::InvalidLexicalInitialization,
            ..
        }
    ));
}

#[test]
fn for_in_marker_certificate_rejects_a_marker_at_an_unreachable_terminal() {
    let instructions = [
        (FinalOpcode::Goto8, Operands::Label8(4)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("even an unreachable terminal must not retain an internal iterator marker");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForInIteratorMarkerAtExit { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn for_in_marker_certificate_charges_exact_state_and_transfer_budgets() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForInStart, Operands::None),
        (FinalOpcode::ForInNext, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let input = typed_stack_input(&instructions, &[], &[]);
    let usage =
        verify_compiler_bytecode_graph(input.clone(), BytecodeGraphVerificationLimits::default())
            .expect("baseline typed for-in certificate")
            .usage();
    assert!(usage.frame_state_entries() > 0);
    assert!(usage.policy_transfers() > 0);

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
fn for_of_marker_certificate_accepts_exact_loop_close_return_and_throw_grammars() {
    let loop_body = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ForOfNext, Operands::U8(0)),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-9)),
    ];
    let verified = verify_compiler_bytecode_graph(
        typed_stack_input(&loop_body, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a same-site synchronous for-of record survives stepping and closes on exit");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::Iterators)
    );

    let returning = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Rot3r, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&returning, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("return closes the iterator through the exact nip_catch/rot3r grammar");

    let nested_return = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Rot3r, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Rot3r, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&nested_return, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("nested synchronous iterators close from the innermost record outward");

    let throwing = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Throw, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&throwing, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a complete for-of catch record may remain for exceptional VM cleanup");
}

#[test]
fn for_of_head_value_certificate_allows_const_reinitialization_on_backedges() {
    let definition = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        const_policy(),
        true,
        None,
    );
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ForOfNext, Operands::U8(0)),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::Goto8, Operands::Label8(-9)),
    ];

    verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[atom("value")], &[definition]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the immediate false-edge for-of value has fresh lexical initialization authority");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the table covers forged, partial, copied, stored, and malformed close records"
)]
fn for_of_marker_certificate_rejects_forged_partial_or_exposed_records() {
    let local = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        var_policy(),
        false,
        None,
    );
    let cases = [
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForOfStart, Operands::None),
                (FinalOpcode::ForOfNext, Operands::U8(1)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::IteratorClose, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::ForOfNext,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::IteratorClose, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::IteratorClose,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForOfStart, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Drop,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForOfStart, Operands::None),
                (FinalOpcode::Dup, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            Vec::new(),
            Vec::new(),
            FinalOpcode::Dup,
        ),
        (
            vec![
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::ForOfStart, Operands::None),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            vec![atom("stored")],
            vec![local],
            FinalOpcode::PutLoc0,
        ),
    ];

    for (instructions, atoms, variables, opcode) in cases {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &atoms, &variables),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("a synchronous for-of record cannot be forged, split, copied, or stored");
        assert!(
            matches!(
                error.kind(),
                BytecodeVerificationErrorKind::ForOfIteratorStackMismatch {
                    opcode: actual,
                    ..
                } if *actual == opcode
            ),
            "{opcode}: {error:?}"
        );
    }
}

#[test]
fn for_of_marker_certificate_requires_exact_site_and_return_close_provenance() {
    let joined = [
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse8, Operands::Label8(5)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(3)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&joined, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("records created at different for-of sites cannot join");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ForOfIteratorJoinMismatch { .. }
    ));

    let malformed_returns = [
        vec![
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Push3, Operands::NoneInt),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::ForOfStart, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::ForOfStart, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::NipCatch, Operands::None),
            (FinalOpcode::Nop, Operands::None),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::IteratorClose, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::ForOfStart, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::NipCatch, Operands::None),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Nop, Operands::None),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::IteratorClose, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        vec![
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::ForOfStart, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::NipCatch, Operands::None),
            (FinalOpcode::Rot3r, Operands::None),
            (FinalOpcode::Null, Operands::None),
            (FinalOpcode::IteratorClose, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
    ];
    for instructions in malformed_returns {
        let error = verify_compiler_bytecode_graph(
            typed_stack_input(&instructions, &[], &[]),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("rot3r and iterator_close require exact return-cleanup provenance");
        assert!(matches!(
            error.kind(),
            BytecodeVerificationErrorKind::ForOfIteratorStackMismatch { .. }
        ));
    }
}

#[test]
fn for_of_marker_certificate_rejects_restepping_the_natural_done_record() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ForOfNext, Operands::U8(0)),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(-9)),
    ];

    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("done=true disables the iterator record and cannot re-enter for_of_next");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ForOfIteratorJoinMismatch { .. }
            | BytecodeVerificationErrorKind::ForOfIteratorStackMismatch {
                opcode: FinalOpcode::ForOfNext,
                ..
            }
    ));
}

#[test]
fn for_of_marker_certificate_merges_active_and_exhausted_records_only_at_shared_close() {
    let shared_close = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ForOfNext, Operands::U8(0)),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    verify_compiler_bytecode_graph(
        typed_stack_input(&shared_close, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("active break and natural exhaustion may share their exact iterator_close site");

    let non_close_join = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ForOfNext, Operands::U8(0)),
        (FinalOpcode::IfFalse8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(4)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Goto8, Operands::Label8(1)),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&non_close_join, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("active and exhausted records cannot widen at a non-close instruction");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ForOfIteratorJoinMismatch { .. }
    ));
}

#[test]
fn for_of_marker_certificate_rejects_a_marker_at_an_unreachable_terminal() {
    let instructions = [
        (FinalOpcode::Goto8, Operands::Label8(4)),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let error = verify_compiler_bytecode_graph(
        typed_stack_input(&instructions, &[], &[]),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("even unreachable terminals cannot retain a for-of record");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::ForOfIteratorMarkerAtExit { .. }
    ));
}

#[test]
fn for_of_marker_certificate_charges_exact_state_and_transfer_budgets() {
    let instructions = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let input = typed_stack_input(&instructions, &[], &[]);
    let usage =
        verify_compiler_bytecode_graph(input.clone(), BytecodeGraphVerificationLimits::default())
            .expect("baseline typed for-of certificate")
            .usage();
    assert!(usage.frame_state_entries() > 0);
    assert!(usage.policy_transfers() > 0);

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

fn shaped_input(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    source: CompilerSource,
) -> UnverifiedCompilerBytecodeGraph {
    shaped_input_with_strict(
        instructions,
        atoms,
        variables,
        arguments,
        locals,
        captures,
        source,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn shaped_input_with_strict(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    source: CompilerSource,
    strict: bool,
) -> UnverifiedCompilerBytecodeGraph {
    shaped_input_with_parameter_profile(
        instructions,
        atoms,
        variables,
        arguments,
        locals,
        captures,
        source,
        strict,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn shaped_input_with_parameter_profile(
    instructions: &[(FinalOpcode, Operands)],
    atoms: &[CompilerAtom],
    variables: &[VariableDefinition],
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    source: CompilerSource,
    strict: bool,
    simple_parameter_list: bool,
) -> UnverifiedCompilerBytecodeGraph {
    let header = UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(
        strict,
        arguments,
        u32::try_from(captures.len()).expect("capture count"),
    )
    .with_simple_parameter_list(simple_parameter_list);
    let flow = flow_with_header(
        instructions,
        u32::try_from(atoms.len()).expect("atom count"),
        arguments,
        locals,
        captures,
        0,
        &[],
        header,
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

fn shaped_mapped_arguments_input(
    instructions: &[(FinalOpcode, Operands)],
    arguments: u32,
    mapped_arguments: &[u32],
    source: CompilerSource,
    strict: bool,
) -> UnverifiedCompilerBytecodeGraph {
    let flow = mapped_arguments_flow(instructions, arguments, mapped_arguments, strict);
    let graph = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(
            FunctionTemplateId::new(0),
            Arc::from([
                UnverifiedCompilerFunction::new(flow, Arc::from([]), Arc::from([]))
                    .with_atom_pool(Arc::from([atom("f")])),
            ]),
        ),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("mapped arguments fixture graph");
    UnverifiedCompilerBytecodeGraph::new(
        Arc::new(graph),
        Arc::from([UnverifiedFunctionMetadata::new(
            Some(AtomPoolIndex::new(0)),
            Arc::from([]),
            Arc::from([]),
            source,
        )]),
    )
}

#[allow(clippy::too_many_arguments)]
fn profiled_single_input(
    instructions: &[(FinalOpcode, Operands)],
    header: UnverifiedFunctionHeader,
    executable_kind: CompilerExecutableKind,
    atoms: &[CompilerAtom],
    function_name: Option<AtomPoolIndex>,
    variables: &[VariableDefinition],
    arguments: u32,
    locals: u32,
    captures: &[CompilerCapturedBinding],
    source: CompilerSource,
) -> UnverifiedCompilerBytecodeGraph {
    let flow = flow_with_header(
        instructions,
        u32::try_from(atoms.len()).expect("atom count"),
        arguments,
        locals,
        captures,
        0,
        &[],
        header,
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
            function_name,
            Arc::from(variables),
            Arc::from([]),
            source,
        )
        .with_executable_kind(executable_kind)]),
    )
}

fn realm_global_definition(
    realm_global: bool,
    name: AtomPoolIndex,
    policy: CompilerBindingPolicy,
    source: CompilerClosureSource,
) -> ClosureVariableDefinition {
    if realm_global {
        ClosureVariableDefinition::realm_global(Some(name), policy, source)
    } else {
        ClosureVariableDefinition::new(Some(name), policy, source)
    }
}

fn dynamic_realm_global_input(
    child_instructions: &[(FinalOpcode, Operands)],
    child_atoms: &[CompilerAtom],
    root_realm_global: bool,
    child_realm_global: bool,
    policy: CompilerBindingPolicy,
) -> UnverifiedCompilerBytecodeGraph {
    let root_flow = flow_with_header(
        &[
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        1,
        0,
        0,
        &[],
        1,
        &[CompilerConstantKind::Function],
        UnverifiedFunctionHeader::dynamic_function_script(0),
    );
    let child_flow = flow(
        child_instructions,
        u32::try_from(child_atoms.len()).expect("child atom count"),
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
                        Arc::clone(&root_flow),
                        Arc::from([quickjs_bytecode::CompilerConstant::Function(
                            FunctionTemplateId::new(1),
                        )]),
                        Arc::from([CompilerClosureSource::ConstructorRealmGlobal(
                            AtomPoolIndex::new(0),
                        )]),
                    )
                    .with_atom_pool(Arc::from([atom("realmValue")])),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        Arc::from([CompilerClosureSource::ParentClosure(0)]),
                    )
                    .with_atom_pool(Arc::from(child_atoms)),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("dynamic realm-global staged graph"),
    );
    let text: Arc<str> = Arc::from("function anonymous(){return realmValue}");
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mapped_source = |flow: &VerifiedControlFlow, name_span| {
        CompilerSource::new(
            Arc::from("fixture.js"),
            Arc::clone(&text),
            full_span,
            name_span,
            Arc::from(
                flow.instructions()
                    .iter()
                    .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), full_span))
                    .collect::<Vec<_>>(),
            ),
        )
    };
    UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                None,
                Arc::from([]),
                Arc::from([realm_global_definition(
                    root_realm_global,
                    AtomPoolIndex::new(0),
                    policy,
                    CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(0)),
                )]),
                mapped_source(&root_flow, None),
            )
            .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript),
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([]),
                Arc::from([realm_global_definition(
                    child_realm_global,
                    AtomPoolIndex::new(1),
                    policy,
                    CompilerClosureSource::ParentClosure(0),
                )]),
                mapped_source(&child_flow, Some(SourceByteSpan::new(9, 18))),
            ),
        ]),
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

#[allow(
    clippy::too_many_lines,
    reason = "the two-function authority fixture keeps graph, metadata, source, and header variants together"
)]
fn define_method_input(
    root_instructions: &[(FinalOpcode, Operands)],
    child_kind: CompilerExecutableKind,
    child_arguments: u32,
) -> UnverifiedCompilerBytecodeGraph {
    define_method_input_with_root_arguments(root_instructions, child_kind, child_arguments, 0)
}

#[allow(
    clippy::too_many_lines,
    reason = "the two-function authority fixture keeps graph, metadata, source, and header variants together"
)]
fn define_method_input_with_root_arguments(
    root_instructions: &[(FinalOpcode, Operands)],
    child_kind: CompilerExecutableKind,
    child_arguments: u32,
    root_arguments: u32,
) -> UnverifiedCompilerBytecodeGraph {
    let root_flow = flow_with_header(
        root_instructions,
        3,
        root_arguments,
        0,
        &[],
        0,
        &[CompilerConstantKind::Function],
        UnverifiedFunctionHeader::ordinary_source_function(false, root_arguments),
    );
    let child_flow = flow_with_header(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        child_arguments,
        child_arguments,
        0,
        &[],
        0,
        &[],
        match child_kind {
            CompilerExecutableKind::OrdinaryFunction => {
                UnverifiedFunctionHeader::ordinary_source_function(false, child_arguments)
            }
            CompilerExecutableKind::OrdinaryMethod => {
                UnverifiedFunctionHeader::ordinary_method_with_variable_references(
                    false,
                    child_arguments,
                    0,
                )
            }
            CompilerExecutableKind::ClassConstructor => {
                UnverifiedFunctionHeader::class_constructor_with_variable_references(
                    true,
                    child_arguments,
                    0,
                )
            }
            CompilerExecutableKind::OrdinaryArrow => {
                panic!("a define_method child cannot be an arrow")
            }
            CompilerExecutableKind::GlobalScript
            | CompilerExecutableKind::IndirectEvalScript
            | CompilerExecutableKind::DirectEvalScript
            | CompilerExecutableKind::DynamicFunctionScript => {
                panic!("a define_method child cannot be a Script")
            }
            CompilerExecutableKind::GeneratorFunction | CompilerExecutableKind::GeneratorMethod => {
                panic!("this ordinary-method fixture cannot create a generator child")
            }
            CompilerExecutableKind::AsyncFunction | CompilerExecutableKind::AsyncMethod => {
                panic!("this ordinary-method fixture cannot create an async child")
            }
            CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod => {
                panic!("this ordinary-method fixture cannot create an async-generator child")
            }
        },
    );
    let child_atoms = (0..child_arguments)
        .map(|index| atom(&format!("argument{index}")))
        .collect::<Vec<_>>();
    let child_variables = (0..child_arguments)
        .map(|index| {
            VariableDefinition::new(
                Some(AtomPoolIndex::new(index)),
                ScopeLink::End,
                parameter_policy(),
                false,
                None,
            )
        })
        .collect::<Vec<_>>();
    let root_variables = (0..root_arguments)
        .map(|_| {
            VariableDefinition::new(
                Some(AtomPoolIndex::new(2)),
                ScopeLink::End,
                parameter_policy(),
                false,
                None,
            )
        })
        .collect::<Vec<_>>();
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
                    .with_atom_pool(Arc::from([
                        atom("outer"),
                        atom("value"),
                        atom("target"),
                    ])),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        Arc::from([]),
                    )
                    .with_atom_pool(child_atoms.into()),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("define_method fixture graph"),
    );
    let text: Arc<str> = Arc::from("function outer(){return {value(){}}}");
    let full_span =
        SourceByteSpan::new(0, u32::try_from(text.len()).expect("fixture source length"));
    let mapped_source = |flow: &VerifiedControlFlow, name_span| {
        CompilerSource::new(
            Arc::from("fixture.js"),
            Arc::clone(&text),
            full_span,
            name_span,
            Arc::from(
                flow.instructions()
                    .iter()
                    .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), full_span))
                    .collect::<Vec<_>>(),
            ),
        )
    };
    UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                root_variables.into(),
                Arc::from([]),
                mapped_source(&root_flow, Some(SourceByteSpan::new(9, 14))),
            ),
            UnverifiedFunctionMetadata::new(
                None,
                child_variables.into(),
                Arc::from([]),
                mapped_source(&child_flow, None),
            )
            .with_executable_kind(child_kind),
        ]),
    )
}

fn realm_global_function_initializer_input(
    instructions: &[(FinalOpcode, Operands)],
    definition_name: &str,
    definition: ClosureVariableDefinition,
) -> UnverifiedCompilerBytecodeGraph {
    let root_flow = flow_with_header(
        instructions,
        1,
        0,
        0,
        &[],
        1,
        &[CompilerConstantKind::Function],
        UnverifiedFunctionHeader::dynamic_function_script(0),
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
    let source = CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(0));
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
                    .with_atom_pool(Arc::from([atom(definition_name)])),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("declared")])),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("realm-global function fixture graph"),
    );
    let text: Arc<str> = Arc::from("function declared(){}");
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mapped_source = |flow: &VerifiedControlFlow, name_span| {
        CompilerSource::new(
            Arc::from("fixture.js"),
            Arc::clone(&text),
            full_span,
            name_span,
            Arc::from(
                flow.instructions()
                    .iter()
                    .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), full_span))
                    .collect::<Vec<_>>(),
            ),
        )
    };
    UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                None,
                Arc::from([]),
                Arc::from([definition]),
                mapped_source(&root_flow, None),
            )
            .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript),
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([]),
                Arc::from([]),
                mapped_source(&child_flow, Some(SourceByteSpan::new(9, 17))),
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
        function.metadata().executable_kind(),
        CompilerExecutableKind::OrdinaryFunction
    );
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
fn dynamic_function_script_profile_grants_only_exact_root_script_authority() {
    let text = "return undefined;";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let input_for = |header| {
        profiled_single_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            header,
            CompilerExecutableKind::DynamicFunctionScript,
            &[],
            None,
            &[],
            0,
            0,
            &[],
            source(text, function_span, None, &[(0, function_span)]),
        )
    };

    let verified = verify_compiler_bytecode_graph(
        input_for(UnverifiedFunctionHeader::dynamic_function_script(0)),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("an exact non-eval root Script gains execution authority");
    let root = verified.root();
    assert_eq!(
        root.metadata().executable_kind(),
        CompilerExecutableKind::DynamicFunctionScript
    );
    assert_eq!(
        root.function()
            .control_flow()
            .function_header()
            .flags()
            .bits(),
        0x0400
    );
    assert_eq!(
        root.function()
            .control_flow()
            .function_header()
            .mode()
            .bits(),
        0
    );
    assert_eq!(
        root.function()
            .control_flow()
            .function_header()
            .defined_argument_count(),
        0
    );

    for rejected in [
        UnverifiedFunctionHeader::new(0x0401, 0, 0, 0),
        UnverifiedFunctionHeader::new(0x0400, 1, 0, 0),
        UnverifiedFunctionHeader::new(0x0c00, 0, 0, 0),
        UnverifiedFunctionHeader::ordinary_source_function(false, 0),
    ] {
        let error = verify_compiler_bytecode_graph(
            input_for(rejected),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("Script authority requires exact debug-only normal-mode header bits");
        assert_eq!(
            error.kind(),
            &BytecodeVerificationErrorKind::UnsupportedFunctionHeader
        );
    }

    let ordinary_with_script_header = profiled_single_input(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        UnverifiedFunctionHeader::dynamic_function_script(0),
        CompilerExecutableKind::OrdinaryFunction,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(text, function_span, None, &[(0, function_span)]),
    );
    let error = verify_compiler_bytecode_graph(
        ordinary_with_script_header,
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("ordinary authority retains the exact 0x0643 header contract");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::UnsupportedFunctionHeader
    );
}

#[test]
fn ordinary_arrow_profile_is_lexical_and_nonconstructable() {
    let text = "()=>this";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mappings = [(0, function_span), (1, function_span)];
    let input = profiled_single_input(
        &[
            (FinalOpcode::PushThis, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        UnverifiedFunctionHeader::ordinary_arrow_with_variable_references(false, 0, 0),
        CompilerExecutableKind::OrdinaryArrow,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(text, function_span, None, &mappings),
    );
    let verified =
        verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
            .expect("sloppy arrow PushThis is lexical authority");
    let header = verified.root().function().control_flow().function_header();
    assert_eq!(header.flags().bits(), 0x0442);
    assert!(!header.flags().has_prototype());
    assert!(!header.flags().arguments_allowed());
    assert!(header.flags().new_target_allowed());

    let arguments = profiled_single_input(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        UnverifiedFunctionHeader::ordinary_arrow_with_variable_references(true, 0, 0),
        CompilerExecutableKind::OrdinaryArrow,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(
            text,
            function_span,
            None,
            &[(0, function_span), (2, function_span)],
        ),
    );
    let error =
        verify_compiler_bytecode_graph(arguments, BytecodeGraphVerificationLimits::default())
            .expect_err("an arrow cannot create an own arguments object");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));
}

#[test]
fn ordinary_arrow_home_object_requires_a_verified_method_ancestor() {
    let text = "()=>super.value";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let input = profiled_single_input(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(5)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        UnverifiedFunctionHeader::ordinary_arrow_with_variable_references(true, 0, 0),
        CompilerExecutableKind::OrdinaryArrow,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(
            text,
            function_span,
            None,
            &[(0, function_span), (2, function_span), (3, function_span)],
        ),
    );
    let error = verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect_err("an arrow without a method ancestor has no lexical home object");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));
}

#[test]
fn ordinary_arrow_super_call_requires_a_verified_derived_constructor_ancestor() {
    let text = "()=>super()";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let instructions = [
        (FinalOpcode::SpecialObject, Operands::U8(4)),
        (FinalOpcode::GetSuper, Operands::None),
        (FinalOpcode::SpecialObject, Operands::U8(3)),
        (
            FinalOpcode::CallConstructor,
            Operands::NPop { argument_count: 0 },
        ),
        (FinalOpcode::CheckCtorReturn, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let mappings = [0, 2, 3, 5, 8, 9, 10, 11].map(|pc| (pc, function_span));
    let input = profiled_single_input(
        &instructions,
        UnverifiedFunctionHeader::ordinary_arrow_with_variable_references(true, 0, 0),
        CompilerExecutableKind::OrdinaryArrow,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(text, function_span, None, &mappings),
    );
    let error = verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect_err("an arrow without a derived constructor ancestor has no super-call binding");
    assert!(
        matches!(
            error.kind(),
            BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                pc,
                opcode: FinalOpcode::SpecialObject,
            } if *pc == BytecodePc::ZERO
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn dynamic_function_script_profile_rejects_names_and_every_argument_domain() {
    let named_text = "script";
    let named_span = SourceByteSpan::new(0, 6);
    let named = profiled_single_input(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        UnverifiedFunctionHeader::dynamic_function_script(0),
        CompilerExecutableKind::DynamicFunctionScript,
        &[atom("script")],
        Some(AtomPoolIndex::new(0)),
        &[],
        0,
        0,
        &[],
        source(named_text, named_span, Some(named_span), &[(0, named_span)]),
    );
    let error = verify_compiler_bytecode_graph(named, BytecodeGraphVerificationLimits::default())
        .expect_err("a Script record has no function-name metadata");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::DynamicFunctionScriptHasFunctionName
    );

    let argument_text = "argument";
    let argument_span = SourceByteSpan::new(0, 8);
    let argument_definition = [VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        parameter_policy(),
        false,
        None,
    )];
    let argument_input = |header| {
        profiled_single_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            header,
            CompilerExecutableKind::DynamicFunctionScript,
            &[atom("argument")],
            None,
            &argument_definition,
            1,
            0,
            &[],
            source(argument_text, argument_span, None, &[(0, argument_span)]),
        )
    };
    for (header, defined) in [
        (UnverifiedFunctionHeader::dynamic_function_script(0), 0),
        (UnverifiedFunctionHeader::new(0x0400, 0, 1, 0), 1),
    ] {
        let error = verify_compiler_bytecode_graph(
            argument_input(header),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("a Script root cannot expose source-defined or frame arguments");
        assert_eq!(
            error.kind(),
            &BytecodeVerificationErrorKind::DynamicFunctionScriptHasArguments {
                defined,
                arguments: 1,
            }
        );
    }
}

#[test]
fn dynamic_function_script_rejects_internal_function_name_binding_authority() {
    let text = "return undefined";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let internal_root_binding = [VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        function_name_policy(),
        false,
        None,
    )];
    let input = profiled_single_input(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        UnverifiedFunctionHeader::dynamic_function_script(0),
        CompilerExecutableKind::DynamicFunctionScript,
        &[atom("internal-script-root")],
        None,
        &internal_root_binding,
        0,
        1,
        &[],
        source(text, function_span, None, &[(0, function_span)]),
    );

    let error = verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect_err("Script metadata cannot expose its internal root as a named-function binding");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::DynamicFunctionScriptHasFunctionName
    );
}

#[test]
fn dynamic_function_script_profile_is_forbidden_on_child_templates() {
    let root_flow = flow(
        &[
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        1,
        0,
        0,
        &[],
        0,
        &[CompilerConstantKind::Function],
    );
    let child_flow = flow_with_header(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        0,
        &[],
        0,
        &[],
        UnverifiedFunctionHeader::dynamic_function_script(0),
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
                    .with_atom_pool(Arc::from([atom("outer")])),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        Arc::from([]),
                    ),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("staged child graph"),
    );
    let text: Arc<str> = Arc::from("function outer(){}");
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let source_for = |flow: &VerifiedControlFlow, name_span| {
        CompilerSource::new(
            Arc::from("fixture.js"),
            Arc::clone(&text),
            full_span,
            name_span,
            Arc::from(
                flow.instructions()
                    .iter()
                    .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), full_span))
                    .collect::<Vec<_>>(),
            ),
        )
    };
    let input = UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([]),
                Arc::from([]),
                source_for(&root_flow, Some(SourceByteSpan::new(9, 14))),
            ),
            UnverifiedFunctionMetadata::new(
                None,
                Arc::from([]),
                Arc::from([]),
                source_for(&child_flow, None),
            )
            .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript),
        ]),
    );

    let error = verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect_err("a Script executable cannot be owned by a function constant");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::DynamicFunctionScriptNotRoot
    );
    assert_eq!(error.function_id(), Some(FunctionTemplateId::new(1)));
}

#[test]
fn arguments_object_authority_is_single_site_mode_and_kind_exact() {
    let text = "function f(){\"use strict\";return arguments}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let source_for = |mappings: &[(u32, SourceByteSpan)]| {
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            mappings,
        )
    };
    let single = shaped_input_with_strict(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        source_for(&[(0, function_span), (2, function_span)]),
        true,
    );
    let verified =
        verify_compiler_bytecode_graph(single, BytecodeGraphVerificationLimits::default())
            .expect("one strict unmapped arguments object is admitted");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::OrdinaryObjects)
    );

    let mapped = shaped_mapped_arguments_input(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(1)),
            (FinalOpcode::Return, Operands::None),
        ],
        0,
        &[],
        source_for(&[(0, function_span), (2, function_span)]),
        false,
    );
    verify_compiler_bytecode_graph(mapped, BytecodeGraphVerificationLimits::default())
        .expect("one sloppy mapped arguments object is admitted");

    let mode_mismatch = shaped_mapped_arguments_input(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(1)),
            (FinalOpcode::Return, Operands::None),
        ],
        0,
        &[],
        source_for(&[(0, function_span), (2, function_span)]),
        true,
    );
    let error =
        verify_compiler_bytecode_graph(mode_mismatch, BytecodeGraphVerificationLimits::default())
            .expect_err("mapped arguments authority is forbidden in strict code");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));

    let duplicate = shaped_input_with_strict(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::SpecialObject, Operands::U8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        source_for(&[
            (0, function_span),
            (2, function_span),
            (3, function_span),
            (5, function_span),
        ]),
        true,
    );
    let error =
        verify_compiler_bytecode_graph(duplicate, BytecodeGraphVerificationLimits::default())
            .expect_err("a second arguments object site is not executable authority");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::new(3)
    ));
}

#[test]
fn new_target_special_object_requires_function_header_authority() {
    let text = "function f(){return new.target}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mappings = [(0, function_span), (2, function_span)];
    let instructions = [
        (FinalOpcode::SpecialObject, Operands::U8(3)),
        (FinalOpcode::Return, Operands::None),
    ];
    let valid = shaped_input_with_strict(
        &instructions,
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &mappings,
        ),
        true,
    );
    let verified =
        verify_compiler_bytecode_graph(valid, BytecodeGraphVerificationLimits::default())
            .expect("ordinary function new.target is admitted");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::Calls)
    );

    let script = profiled_single_input(
        &instructions,
        UnverifiedFunctionHeader::dynamic_function_script(0),
        CompilerExecutableKind::DynamicFunctionScript,
        &[],
        None,
        &[],
        0,
        0,
        &[],
        source(text, function_span, None, &mappings),
    );
    let error = verify_compiler_bytecode_graph(script, BytecodeGraphVerificationLimits::default())
        .expect_err("a Script frame cannot expose new.target");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one certificate test covers the complete rest-site authority matrix"
)]
fn rest_parameter_authority_is_single_site_non_simple_and_after_arguments() {
    let text = "function f(fixed,...rest){return rest}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let source_for = |mappings: &[(u32, SourceByteSpan)]| {
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            mappings,
        )
    };
    let fixed = VariableDefinition::new(
        Some(AtomPoolIndex::new(0)),
        ScopeLink::End,
        parameter_policy(),
        false,
        None,
    );

    let valid = shaped_input_with_parameter_profile(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Rest, Operands::U16(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("fixed")],
        std::slice::from_ref(&fixed),
        1,
        0,
        &[],
        source_for(&[
            (0, function_span),
            (2, function_span),
            (3, function_span),
            (6, function_span),
            (7, function_span),
        ]),
        false,
        false,
    );
    let verified =
        verify_compiler_bytecode_graph(valid, BytecodeGraphVerificationLimits::default())
            .expect("one exact rest allocation after the arguments object is admitted");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::Arrays)
    );

    let wrong_first = shaped_input_with_parameter_profile(
        &[
            (FinalOpcode::Rest, Operands::U16(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("fixed")],
        std::slice::from_ref(&fixed),
        1,
        0,
        &[],
        source_for(&[(0, function_span), (3, function_span), (4, function_span)]),
        false,
        false,
    );
    let error =
        verify_compiler_bytecode_graph(wrong_first, BytecodeGraphVerificationLimits::default())
            .expect_err("rest must begin exactly after the fixed argument domain");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::Rest,
        } if *pc == BytecodePc::ZERO
    ));

    let simple = shaped_input_with_parameter_profile(
        &[
            (FinalOpcode::Rest, Operands::U16(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("fixed")],
        std::slice::from_ref(&fixed),
        1,
        0,
        &[],
        source_for(&[(0, function_span), (3, function_span), (4, function_span)]),
        false,
        true,
    );
    let error = verify_compiler_bytecode_graph(simple, BytecodeGraphVerificationLimits::default())
        .expect_err("a simple-parameter header cannot authorize rest allocation");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::Rest,
        } if *pc == BytecodePc::ZERO
    ));

    let duplicate = shaped_input_with_parameter_profile(
        &[
            (FinalOpcode::Rest, Operands::U16(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Rest, Operands::U16(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("fixed")],
        std::slice::from_ref(&fixed),
        1,
        0,
        &[],
        source_for(&[
            (0, function_span),
            (3, function_span),
            (4, function_span),
            (7, function_span),
            (8, function_span),
        ]),
        false,
        false,
    );
    let error =
        verify_compiler_bytecode_graph(duplicate, BytecodeGraphVerificationLimits::default())
            .expect_err("a second rest allocation site is not executable authority");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::Rest,
        } if *pc == BytecodePc::new(4)
    ));

    let late_arguments = shaped_input_with_parameter_profile(
        &[
            (FinalOpcode::Rest, Operands::U16(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::SpecialObject, Operands::U8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("fixed")],
        &[fixed],
        1,
        0,
        &[],
        source_for(&[
            (0, function_span),
            (3, function_span),
            (4, function_span),
            (6, function_span),
        ]),
        false,
        false,
    );
    let error =
        verify_compiler_bytecode_graph(late_arguments, BytecodeGraphVerificationLimits::default())
            .expect_err("arguments-object creation must precede rest snapshot consumption");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::new(4)
    ));
}

#[test]
fn sloppy_arguments_kind_matches_the_simple_parameter_header_bit() {
    let text = "function f(){return arguments}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let source_for = || {
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[(0, function_span), (2, function_span)],
        )
    };
    let instructions = [
        (FinalOpcode::SpecialObject, Operands::U8(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let non_simple = shaped_input_with_parameter_profile(
        &instructions,
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        source_for(),
        false,
        false,
    );
    verify_compiler_bytecode_graph(non_simple, BytecodeGraphVerificationLimits::default())
        .expect("one sloppy non-simple unmapped arguments object is admitted");

    let simple_unmapped = shaped_input_with_strict(
        &instructions,
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        source_for(),
        false,
    );
    let error =
        verify_compiler_bytecode_graph(simple_unmapped, BytecodeGraphVerificationLimits::default())
            .expect_err("sloppy simple parameters cannot claim an unmapped arguments object");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));
}

#[test]
fn mapped_arguments_opcode_and_mapping_certificate_are_bijective() {
    let text = "function f(){return arguments}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let source_for = |mappings: &[(u32, SourceByteSpan)]| {
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            mappings,
        )
    };
    let missing_mapping = shaped_input_with_strict(
        &[
            (FinalOpcode::SpecialObject, Operands::U8(1)),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("f")],
        &[],
        0,
        0,
        &[],
        source_for(&[(0, function_span), (2, function_span)]),
        false,
    );
    let error =
        verify_compiler_bytecode_graph(missing_mapping, BytecodeGraphVerificationLimits::default())
            .expect_err("mapped arguments require exact compiler mapping authority");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));

    let unused_mapping = shaped_mapped_arguments_input(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        &[],
        source_for(&[(0, function_span)]),
        false,
    );
    let error =
        verify_compiler_bytecode_graph(unused_mapping, BytecodeGraphVerificationLimits::default())
            .expect_err("mapping authority cannot survive without its allocation site");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
            pc,
            opcode: FinalOpcode::SpecialObject,
        } if *pc == BytecodePc::ZERO
    ));
}

#[test]
fn dynamic_function_authority_carries_verified_constructor_realm_global_references() {
    let input = dynamic_realm_global_input(
        &[
            (FinalOpcode::GetVarUndef, Operands::VarRef(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutVar, Operands::VarRef(0)),
            (
                FinalOpcode::DeleteVar,
                Operands::Atom(AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("anonymous"), atom("realmValue")],
        true,
        true,
        global_reference_policy(),
    );
    let verified =
        verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
            .expect("global lookup, assignment, and delete gain typed dynamic-Function authority");

    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::RealmGlobalBindings)
    );
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(1))
            .expect("dynamic function")
            .metadata()
            .closures()[0]
            .binding(),
        CompilerClosureBinding::RealmGlobal(global_reference_policy())
    );
}

#[test]
fn constructor_realm_global_opcodes_cannot_cross_captured_slot_boundaries() {
    let realm_slot_with_capture_opcode = dynamic_realm_global_input(
        &[
            (FinalOpcode::GetVarRef, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("anonymous"), atom("realmValue")],
        true,
        true,
        global_reference_policy(),
    );
    let error = verify_compiler_bytecode_graph(
        realm_slot_with_capture_opcode,
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a realm-global slot cannot execute captured-cell opcodes");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::ClosureBindingOpcodeMismatch {
            closure: 0,
            pc: BytecodePc::new(0),
            opcode: FinalOpcode::GetVarRef,
        }
    );

    let captured_slot_with_global_opcode = dynamic_realm_global_input(
        &[
            (FinalOpcode::GetVar, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("anonymous"), atom("realmValue")],
        true,
        false,
        global_reference_policy(),
    );
    let error = verify_compiler_bytecode_graph(
        captured_slot_with_global_opcode,
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a captured slot cannot execute constructor-realm global opcodes");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Closure(0),
            pc: None,
            reason: BindingPolicyViolationReason::InvalidDeclarationPolicy,
        }
    );

    let captured_root_source = dynamic_realm_global_input(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        &[atom("anonymous"), atom("realmValue")],
        false,
        true,
        global_reference_policy(),
    );
    let error = verify_compiler_bytecode_graph(
        captured_root_source,
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("root realm-global provenance cannot be relabeled as a captured binding");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Closure(0),
            pc: None,
            reason: BindingPolicyViolationReason::InvalidDeclarationPolicy,
        }
    );
}

#[test]
fn ordinary_root_authority_cannot_originate_constructor_realm_globals() {
    let flow = flow_with_header(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        1,
        0,
        0,
        &[],
        1,
        &[],
        UnverifiedFunctionHeader::ordinary_source_function(false, 0),
    );
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([UnverifiedCompilerFunction::new(
                    Arc::clone(&flow),
                    Arc::from([]),
                    Arc::from([CompilerClosureSource::ConstructorRealmGlobal(
                        AtomPoolIndex::new(0),
                    )]),
                )
                .with_atom_pool(Arc::from([atom("realmValue")]))]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("staged graphs defer the executable-role check to final verification"),
    );
    let text = "function(){return undefined}";
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let input = UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([UnverifiedFunctionMetadata::new(
            None,
            Arc::from([]),
            Arc::from([ClosureVariableDefinition::realm_global(
                Some(AtomPoolIndex::new(0)),
                global_reference_policy(),
                CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(0)),
            )]),
            source(text, full_span, None, &[(0, full_span)]),
        )]),
    );
    let error = verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect_err("only a dynamic-Function Script authority owns constructor-realm globals");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::ConstructorRealmGlobalSourceRequiresDynamicFunctionScript {
            closure: 0,
        }
    );
}

fn direct_eval_binding_input(
    executable_kind: CompilerExecutableKind,
    instructions: &[(FinalOpcode, Operands)],
    policy: CompilerBindingPolicy,
) -> UnverifiedCompilerBytecodeGraph {
    direct_eval_source_input(
        executable_kind,
        instructions,
        policy,
        CompilerClosureSource::DirectEvalBinding {
            index: 1,
            environment_size: 2,
        },
        Some(AtomPoolIndex::new(0)),
    )
}

fn direct_eval_source_input(
    executable_kind: CompilerExecutableKind,
    instructions: &[(FinalOpcode, Operands)],
    policy: CompilerBindingPolicy,
    closure_source: CompilerClosureSource,
    name: Option<AtomPoolIndex>,
) -> UnverifiedCompilerBytecodeGraph {
    let flow = flow_with_header(
        instructions,
        1,
        0,
        0,
        &[],
        1,
        &[],
        UnverifiedFunctionHeader::global_script(false, 0),
    );
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([UnverifiedCompilerFunction::new(
                    Arc::clone(&flow),
                    Arc::from([]),
                    Arc::from([closure_source]),
                )
                .with_atom_pool(Arc::from([atom("callerValue")]))]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("staged direct-eval caller-binding graph"),
    );
    let text = "callerValue";
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let mappings = flow
        .instructions()
        .iter()
        .map(|instruction| (instruction.decoded().pc().get(), full_span))
        .collect::<Vec<_>>();
    UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([UnverifiedFunctionMetadata::new(
            None,
            Arc::from([]),
            Arc::from([ClosureVariableDefinition::new(name, policy, closure_source)]),
            source(text, full_span, None, &mappings),
        )
        .with_executable_kind(executable_kind)]),
    )
}

#[test]
fn direct_eval_new_variables_require_named_mutable_var_or_function_bindings() {
    let source = CompilerClosureSource::DirectEvalVariable {
        index: 1,
        environment_size: 2,
    };
    let verified = verify_compiler_bytecode_graph(
        direct_eval_source_input(
            CompilerExecutableKind::DirectEvalScript,
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            var_policy(),
            source,
            Some(AtomPoolIndex::new(0)),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a named mutable var may be created in the caller variable environment");
    assert_eq!(verified.root().function().closure_sources(), [source]);

    let error = verify_compiler_bytecode_graph(
        direct_eval_source_input(
            CompilerExecutableKind::DirectEvalScript,
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            const_policy(),
            source,
            Some(AtomPoolIndex::new(0)),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("new eval variables must retain mutable var/function policy");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Closure(0),
            pc: None,
            reason: BindingPolicyViolationReason::InvalidDeclarationPolicy,
        }
    );

    let error = verify_compiler_bytecode_graph(
        direct_eval_source_input(
            CompilerExecutableKind::DirectEvalScript,
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            var_policy(),
            source,
            None,
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a new eval variable must retain its name");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::MissingMetadataAtom {
            field: MetadataAtomField::ClosureName(0),
        }
    );
}

#[test]
fn direct_eval_authority_binds_only_direct_eval_caller_sources() {
    let verified = verify_compiler_bytecode_graph(
        direct_eval_binding_input(
            CompilerExecutableKind::DirectEvalScript,
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            var_policy(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a direct-eval Script root can import a typed caller binding");
    assert_eq!(
        verified.root().metadata().executable_kind(),
        CompilerExecutableKind::DirectEvalScript
    );
    assert_eq!(
        verified.root().metadata().closures()[0].binding(),
        CompilerClosureBinding::Captured(var_policy())
    );

    let error = verify_compiler_bytecode_graph(
        direct_eval_binding_input(
            CompilerExecutableKind::IndirectEvalScript,
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            var_policy(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("an indirect-eval authority cannot import a caller binding");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::DirectEvalBindingSourceRequiresDirectEvalScript {
            closure: 0,
        }
    );
}

#[test]
fn direct_eval_immutable_caller_writes_remain_runtime_checked() {
    let verified = verify_compiler_bytecode_graph(
        direct_eval_binding_input(
            CompilerExecutableKind::DirectEvalScript,
            &[
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::PutVarRefCheck, Operands::VarRef(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            const_policy(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a verified captured write retains immutable caller policy for the VM");

    assert_eq!(
        verified.root().metadata().closures()[0].policy(),
        const_policy()
    );
}

#[test]
fn unresolved_realm_globals_forbid_initialization_and_undeclared_delete_atoms() {
    let put_init = dynamic_realm_global_input(
        &[
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutVarInit, Operands::VarRef(0)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("anonymous"), atom("realmValue")],
        true,
        true,
        global_reference_policy(),
    );
    let error =
        verify_compiler_bytecode_graph(put_init, BytecodeGraphVerificationLimits::default())
            .expect_err("an unresolved global reference is never declaration-initialized");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::ClosureBindingOpcodeMismatch {
            closure: 0,
            pc: BytecodePc::new(1),
            opcode: FinalOpcode::PutVarInit,
        }
    );

    let undeclared_delete = dynamic_realm_global_input(
        &[
            (
                FinalOpcode::DeleteVar,
                Operands::Atom(AtomPoolIndex::new(2)),
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("anonymous"), atom("realmValue"), atom("other")],
        true,
        true,
        global_reference_policy(),
    );
    let error = verify_compiler_bytecode_graph(
        undeclared_delete,
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("delete_var needs a same-name unresolved realm-global descriptor");
    assert_eq!(
        error.kind(),
        &BytecodeVerificationErrorKind::RealmGlobalDeleteBindingMissing {
            pc: BytecodePc::new(0),
            atom: AtomPoolIndex::new(2),
        }
    );
}

#[test]
fn realm_global_authority_supports_indirect_eval_var_but_rejects_lexical_declarations() {
    let verified = verify_compiler_bytecode_graph(
        dynamic_realm_global_input(
            &[
                (FinalOpcode::GetVar, Operands::VarRef(0)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::PutVar, Operands::VarRef(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ],
            &[atom("anonymous"), atom("realmValue")],
            true,
            true,
            var_policy(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("indirect-eval var remains a mutable constructor-realm binding");
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::RealmGlobalBindings)
    );

    let delete_declared_var = verify_compiler_bytecode_graph(
        dynamic_realm_global_input(
            &[
                (
                    FinalOpcode::DeleteVar,
                    Operands::Atom(AtomPoolIndex::new(1)),
                ),
                (FinalOpcode::Return, Operands::None),
            ],
            &[atom("anonymous"), atom("realmValue")],
            true,
            true,
            var_policy(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("an indirect-eval var is backed by a configurable global object property");
    assert!(
        delete_declared_var
            .requirements()
            .contains(&ExecutionRequirement::RealmGlobalBindings)
    );

    for policy in [let_policy(), const_policy()] {
        let error = verify_compiler_bytecode_graph(
            dynamic_realm_global_input(
                &[(FinalOpcode::ReturnUndef, Operands::None)],
                &[atom("anonymous"), atom("realmValue")],
                true,
                true,
                policy,
            ),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("indirect-eval lexical declarations are evaluation-local, not realm globals");
        assert_eq!(
            error.kind(),
            &BytecodeVerificationErrorKind::BindingPolicyViolation {
                slot: BindingSlot::Closure(0),
                pc: None,
                reason: BindingPolicyViolationReason::InvalidDeclarationPolicy,
            }
        );
    }

    let missing_function_initializer = verify_compiler_bytecode_graph(
        dynamic_realm_global_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            &[atom("anonymous"), atom("realmValue")],
            true,
            true,
            function_policy(CompilerInitializationPolicy::FunctionAtInstantiation),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("realm-global Function declarations require an exact child initializer");
    assert_eq!(
        missing_function_initializer.kind(),
        &BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
            closure: 0,
            constant: None,
        }
    );
}

#[test]
fn realm_global_function_initializer_requires_an_exact_root_entry_pair_and_named_child() {
    let source = CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(0));
    let definition = ClosureVariableDefinition::realm_global(
        Some(AtomPoolIndex::new(0)),
        function_policy(CompilerInitializationPolicy::FunctionAtInstantiation),
        source,
    )
    .with_function_initializer(0);
    let valid = [
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutVar, Operands::VarRef(0)),
        (FinalOpcode::GetVar, Operands::VarRef(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let verified = verify_compiler_bytecode_graph(
        realm_global_function_initializer_input(&valid, "declared", definition.clone()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("a root global function is tied to its exact initializer child");
    assert_eq!(
        verified.root().metadata().closures()[0].function_initializer(),
        Some(0)
    );

    let missing_pair = verify_compiler_bytecode_graph(
        realm_global_function_initializer_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            "declared",
            definition.clone(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("metadata cannot initialize a global function without executable bytecode");
    assert_eq!(
        missing_pair.kind(),
        &BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
            closure: 0,
            constant: 0,
            matches: 0,
        }
    );

    let misplaced = [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
        (FinalOpcode::PutVar, Operands::VarRef(0)),
        (FinalOpcode::ReturnUndef, Operands::None),
    ];
    let misplaced = verify_compiler_bytecode_graph(
        realm_global_function_initializer_input(&misplaced, "declared", definition.clone()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("global declaration initialization precedes user bytecode");
    assert!(matches!(
        misplaced.kind(),
        BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerPlacementMismatch {
            closure: 0,
            ..
        }
    ));

    let wrong_name = verify_compiler_bytecode_graph(
        realm_global_function_initializer_input(&valid, "other", definition),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("the global binding and initializing child must have the same name");
    assert_eq!(
        wrong_name.kind(),
        &BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
            closure: 0,
            constant: Some(0),
        }
    );

    let scope_entry = ClosureVariableDefinition::realm_global(
        Some(AtomPoolIndex::new(0)),
        function_policy(CompilerInitializationPolicy::FunctionAtScopeEntry),
        source,
    )
    .with_function_initializer(0);
    let error = verify_compiler_bytecode_graph(
        realm_global_function_initializer_input(&valid, "declared", scope_entry),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a Program global function is instantiated once, never at block scope entry");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            slot: BindingSlot::Closure(0),
            reason: BindingPolicyViolationReason::InvalidDeclarationPolicy,
            ..
        }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn dynamic_script_lexical_is_evaluation_local_and_capturable_by_its_child() {
    let root_flow = flow_with_header(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        2,
        0,
        1,
        &[CompilerCapturedBinding::ScopedLocal(0)],
        0,
        &[CompilerConstantKind::Function],
        UnverifiedFunctionHeader::dynamic_function_script(1),
    );
    let child_flow = flow(
        &[
            (FinalOpcode::GetVarRefCheck, Operands::VarRef(0)),
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
                        Arc::clone(&root_flow),
                        Arc::from([quickjs_bytecode::CompilerConstant::Function(
                            FunctionTemplateId::new(1),
                        )]),
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("lexical"), atom("inner")])),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        Arc::from([CompilerClosureSource::ParentVariableReference(0)]),
                    )
                    .with_atom_pool(Arc::from([atom("inner"), atom("lexical")])),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("dynamic Script evaluation-local lexical graph"),
    );
    let text: Arc<str> = Arc::from("let lexical=1; function inner(){return lexical} inner");
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let input = UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                None,
                Arc::from([VariableDefinition::new(
                    Some(AtomPoolIndex::new(0)),
                    ScopeLink::End,
                    let_policy(),
                    true,
                    Some(0),
                )]),
                Arc::from([]),
                CompilerSource::new(
                    Arc::from("fixture.js"),
                    Arc::clone(&text),
                    full_span,
                    None,
                    Arc::from(
                        root_flow
                            .instructions()
                            .iter()
                            .map(|instruction| {
                                PcSourceSpan::new(instruction.decoded().pc(), full_span)
                            })
                            .collect::<Vec<_>>(),
                    ),
                ),
            )
            .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript),
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([]),
                Arc::from([ClosureVariableDefinition::new(
                    Some(AtomPoolIndex::new(1)),
                    let_policy(),
                    CompilerClosureSource::ParentVariableReference(0),
                )]),
                source_for_flow(
                    &text,
                    &child_flow,
                    SourceByteSpan::new(15, 47),
                    SourceByteSpan::new(24, 29),
                ),
            ),
        ]),
    );

    let verified =
        verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
            .expect("an escaped child can capture an evaluation-local Script lexical");
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(1))
            .expect("escaped child")
            .metadata()
            .closures()[0]
            .binding(),
        CompilerClosureBinding::Captured(let_policy())
    );
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::LexicalBindings)
    );
    assert!(
        !verified
            .requirements()
            .contains(&ExecutionRequirement::RealmGlobalBindings)
    );
}

#[test]
fn sloppy_this_is_authorized_only_inside_a_script_authority() {
    let verified = verify_compiler_bytecode_graph(
        dynamic_realm_global_input(
            &[
                (FinalOpcode::PushThis, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            &[atom("anonymous"), atom("realmValue")],
            true,
            true,
            global_reference_policy(),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("ordinary dynamic Function receives constructor-realm sloppy-this authority");
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(1))
            .expect("dynamic function")
            .function()
            .control_flow()
            .function_header()
            .mode()
            .bits(),
        0
    );
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
fn parameter_expression_authority_requires_non_simple_reduced_length_and_local_tdz_storage() {
    let text = "function f(value=1){return value}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
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
            parameter_tdz_policy(),
            false,
            None,
        ),
    ];
    let instructions = [
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        (FinalOpcode::GetArg0, Operands::NoneArg),
        (FinalOpcode::PutLoc0, Operands::NoneLoc),
        (FinalOpcode::GetLocCheck, Operands::Loc(0)),
        (FinalOpcode::Return, Operands::None),
    ];
    let header = UnverifiedFunctionHeader::ordinary_source_function(false, 0)
        .with_simple_parameter_list(false);
    let input = profiled_single_input(
        &instructions,
        header,
        CompilerExecutableKind::OrdinaryFunction,
        &[atom("f"), atom("_arg_0_"), atom("value")],
        Some(AtomPoolIndex::new(0)),
        &variables,
        1,
        1,
        &[],
        source(
            text,
            function_span,
            Some(SourceByteSpan::new(9, 10)),
            &[
                (0, SourceByteSpan::new(11, 18)),
                (3, SourceByteSpan::new(11, 18)),
                (4, SourceByteSpan::new(11, 18)),
                (5, SourceByteSpan::new(27, 32)),
                (8, SourceByteSpan::new(20, 33)),
            ],
        ),
    );
    verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .expect("a reduced non-simple header and one activated parameter TDZ local are valid");

    let simple_header = UnverifiedFunctionHeader::ordinary_source_function(false, 0);
    let error = verify_compiler_bytecode_graph(
        profiled_single_input(
            &instructions,
            simple_header,
            CompilerExecutableKind::OrdinaryFunction,
            &[atom("f"), atom("_arg_0_"), atom("value")],
            Some(AtomPoolIndex::new(0)),
            &variables,
            1,
            1,
            &[],
            source(
                text,
                function_span,
                Some(SourceByteSpan::new(9, 10)),
                &[
                    (0, SourceByteSpan::new(11, 18)),
                    (3, SourceByteSpan::new(11, 18)),
                    (4, SourceByteSpan::new(11, 18)),
                    (5, SourceByteSpan::new(27, 32)),
                    (8, SourceByteSpan::new(20, 33)),
                ],
            ),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("a simple header cannot reduce observable function length");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::DefinedArgumentCountMismatch { .. }
    ));

    let error = verify_compiler_bytecode_graph(
        profiled_single_input(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            UnverifiedFunctionHeader::ordinary_source_function(false, 1)
                .with_simple_parameter_list(false),
            CompilerExecutableKind::OrdinaryFunction,
            &[atom("f"), atom("value")],
            Some(AtomPoolIndex::new(0)),
            &[VariableDefinition::new(
                Some(AtomPoolIndex::new(1)),
                ScopeLink::End,
                parameter_tdz_policy(),
                false,
                None,
            )],
            1,
            0,
            &[],
            source(
                text,
                function_span,
                Some(SourceByteSpan::new(9, 10)),
                &[(0, function_span)],
            ),
        ),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect_err("raw argument slots are already initialized and cannot carry TDZ policy");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            reason: BindingPolicyViolationReason::InvalidArgumentDefinition,
            ..
        }
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
    .expect_err("a bytecode write cannot mutate the named-function self binding");
    assert!(matches!(
        error.kind(),
        BytecodeVerificationErrorKind::BindingPolicyViolation {
            reason: BindingPolicyViolationReason::ImmutableWrite,
            ..
        }
    ));
}

#[test]
fn function_name_binding_is_initialized_at_entry_with_only_the_exact_policy() {
    let text = "function self(){}";
    let function_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let definition = |policy| {
        VariableDefinition::new(
            Some(AtomPoolIndex::new(0)),
            ScopeLink::End,
            policy,
            false,
            None,
        )
    };
    let input = |strict, policy| {
        profiled_single_input(
            &[
                (FinalOpcode::GetLoc0, Operands::NoneLoc),
                (FinalOpcode::Return, Operands::None),
            ],
            UnverifiedFunctionHeader::ordinary_source_function(strict, 0),
            CompilerExecutableKind::OrdinaryFunction,
            &[atom("self")],
            Some(AtomPoolIndex::new(0)),
            &[definition(policy)],
            0,
            1,
            &[],
            source(
                text,
                function_span,
                Some(SourceByteSpan::new(9, 13)),
                &[(0, function_span), (1, function_span)],
            ),
        )
    };

    let sloppy = verify_compiler_bytecode_graph(
        input(false, function_name_policy()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the sloppy named self binding uses ImmutableInStrictCode");
    assert_eq!(
        sloppy.root().metadata().variables()[0].policy(),
        function_name_policy()
    );
    assert_eq!(
        sloppy.usage().frame_state_entries(),
        2,
        "entry initialization remains part of bounded abstract-state analysis"
    );

    let strict = verify_compiler_bytecode_graph(
        input(true, strict_function_name_policy()),
        BytecodeGraphVerificationLimits::default(),
    )
    .expect("the strict named self binding uses Immutable");
    assert_eq!(
        strict.root().metadata().variables()[0].policy(),
        strict_function_name_policy()
    );

    for (strict, rejected) in [
        (false, strict_function_name_policy()),
        (true, function_name_policy()),
        (
            false,
            CompilerBindingPolicy::new(
                CompilerBindingKind::FunctionName,
                CompilerInitializationPolicy::AtDeclaration,
                CompilerWritePolicy::ImmutableInStrictCode,
                false,
            ),
        ),
        (
            false,
            CompilerBindingPolicy::new(
                CompilerBindingKind::FunctionName,
                CompilerInitializationPolicy::FunctionName,
                CompilerWritePolicy::ImmutableInStrictCode,
                true,
            ),
        ),
    ] {
        let error = verify_compiler_bytecode_graph(
            input(strict, rejected),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect_err("named-self write policy must match the owning function strictness");
        assert!(matches!(
            error.kind(),
            BytecodeVerificationErrorKind::BindingPolicyViolation {
                reason: BindingPolicyViolationReason::InvalidDeclarationPolicy,
                ..
            }
        ));
    }
}

#[test]
fn captured_function_name_binding_starts_initialized_with_an_active_cell() {
    let root_flow = flow_with_header(
        &[
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        2,
        0,
        1,
        &[CompilerCapturedBinding::FunctionLocal(0)],
        0,
        &[CompilerConstantKind::Function],
        UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(true, 0, 1),
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
                        Arc::clone(&root_flow),
                        Arc::from([quickjs_bytecode::CompilerConstant::Function(
                            FunctionTemplateId::new(1),
                        )]),
                        Arc::from([]),
                    )
                    .with_atom_pool(Arc::from([atom("self"), atom("child")])),
                    UnverifiedCompilerFunction::new(
                        Arc::clone(&child_flow),
                        Arc::from([]),
                        Arc::from([CompilerClosureSource::ParentVariableReference(0)]),
                    )
                    .with_atom_pool(Arc::from([atom("child"), atom("self")])),
                ]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("captured named-self graph"),
    );
    let text: Arc<str> = Arc::from("function self(){return function child(){return self}}");
    let full_span = SourceByteSpan::new(0, u32::try_from(text.len()).expect("source length"));
    let input = UnverifiedCompilerBytecodeGraph::new(
        graph,
        Arc::from([
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([VariableDefinition::new(
                    Some(AtomPoolIndex::new(0)),
                    ScopeLink::End,
                    strict_function_name_policy(),
                    false,
                    Some(0),
                )]),
                Arc::from([]),
                source_for_flow(&text, &root_flow, full_span, SourceByteSpan::new(9, 13)),
            ),
            UnverifiedFunctionMetadata::new(
                Some(AtomPoolIndex::new(0)),
                Arc::from([]),
                Arc::from([ClosureVariableDefinition::new(
                    Some(AtomPoolIndex::new(1)),
                    strict_function_name_policy(),
                    CompilerClosureSource::ParentVariableReference(0),
                )]),
                source_for_flow(
                    &text,
                    &child_flow,
                    SourceByteSpan::new(23, full_span.end()),
                    SourceByteSpan::new(32, 37),
                ),
            ),
        ]),
    );

    let verified =
        verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
            .expect("a child can capture and read the initialized named-self binding");
    assert_eq!(verified.usage().frame_state_entries(), 2);
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(1))
            .expect("child")
            .metadata()
            .closures()[0]
            .policy(),
        strict_function_name_policy()
    );
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
fn checked_tdz_access_suppresses_definite_throw_and_narrows_mixed_normal_paths() {
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
            const_policy(),
            true,
            None,
        ),
    ];
    let text = "function f(a){const x=1}";
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

    verified_single(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::GetLocCheck, Operands::Loc(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("x")],
        &variables,
        make_source(&[0, 3, 6, 7, 8, 9]),
    )
    .expect("a definite TDZ throw has no normal path to poison a later const initializer");

    verified_single(
        &[
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
            (FinalOpcode::PushTrue, Operands::None),
            (FinalOpcode::IfFalse8, Operands::Label8(5)),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::Goto8, Operands::Label8(1)),
            (FinalOpcode::GetLocCheck, Operands::Loc(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::GetLoc0, Operands::NoneLoc),
            (FinalOpcode::Return, Operands::None),
        ],
        &[atom("f"), atom("a"), atom("x")],
        &variables,
        make_source(&[0, 3, 4, 6, 7, 8, 10, 13, 14, 15]),
    )
    .expect("the normal edge of a mixed checked access contains only initialized states");
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
