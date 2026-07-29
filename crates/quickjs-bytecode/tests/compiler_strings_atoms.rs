use std::sync::Arc;

use quickjs_bytecode::{
    BytecodeBuilder, CompilerAtom, CompilerCaptureLayout, CompilerConstant, CompilerConstantLayout,
    CompilerConstantValue, CompilerString, FinalOpcode, FunctionGraphResource,
    FunctionGraphVerificationErrorKind, FunctionGraphVerificationLimits, FunctionIndexDomains,
    FunctionTemplateId, Operands, UnverifiedCompilerFunction, UnverifiedCompilerFunctionBody,
    UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader, VerificationLimits,
    verify_compiler_control_flow, verify_compiler_function_graph,
};

fn string(units: &[u16]) -> CompilerString {
    CompilerString::try_from_code_units(Arc::from(units))
        .expect("fixture string fits the compatible length")
}

fn atom(units: &[u16]) -> CompilerAtom {
    CompilerAtom::new(string(units))
}

fn graph_function(
    atom_count: u32,
    instructions: &[(FinalOpcode, Operands)],
    atoms: Arc<[CompilerAtom]>,
    constants: Arc<[CompilerConstant]>,
) -> UnverifiedCompilerFunction {
    let mut builder = BytecodeBuilder::new();
    for &(opcode, operands) in instructions {
        builder
            .push(opcode, operands)
            .expect("fixture instruction must encode");
    }
    let constant_count = u32::try_from(constants.len()).expect("fixture constant count fits u32");
    let constant_kinds = constants
        .iter()
        .map(CompilerConstant::kind)
        .collect::<Vec<_>>();
    let flow = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            builder.into_bytes(),
            FunctionIndexDomains::new(atom_count, constant_count, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
        )
        .with_capture_layout(CompilerCaptureLayout::default())
        .with_constant_layout(CompilerConstantLayout::new(constant_kinds.into())),
        VerificationLimits::default(),
    )
    .expect("fixture body must verify against its declared domains");

    UnverifiedCompilerFunction::new(Arc::new(flow), constants, Arc::from([])).with_atom_pool(atoms)
}

fn graph(
    function: UnverifiedCompilerFunction,
    limits: FunctionGraphVerificationLimits,
) -> Result<
    quickjs_bytecode::VerifiedCompilerFunctionGraph,
    quickjs_bytecode::FunctionGraphVerificationError,
> {
    verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), Arc::from([function])),
        limits,
    )
}

fn function_graph(
    functions: Vec<UnverifiedCompilerFunction>,
    limits: FunctionGraphVerificationLimits,
) -> Result<
    quickjs_bytecode::VerifiedCompilerFunctionGraph,
    quickjs_bytecode::FunctionGraphVerificationError,
> {
    verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), functions.into()),
        limits,
    )
}

#[test]
fn explicit_graph_limits_include_atom_and_string_budgets() {
    let limits = FunctionGraphVerificationLimits::new(1, 2, 3, 4, 5, 6, 7, 8, 9);

    assert_eq!(limits.max_functions(), 1);
    assert_eq!(limits.max_nesting_depth(), 2);
    assert_eq!(limits.max_bytecode_bytes(), 3);
    assert_eq!(limits.max_instructions(), 4);
    assert_eq!(limits.max_constants(), 5);
    assert_eq!(limits.max_atoms(), 6);
    assert_eq!(limits.max_string_payload_bytes(), 7);
    assert_eq!(limits.max_closure_variables(), 8);
    assert_eq!(limits.max_closure_edge_evaluations(), 9);
    assert_eq!(limits.max_transfer_evaluations(), 9);
}

#[test]
fn compiler_strings_are_canonical_exact_utf16_and_arc_backed() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompilerString>();
    assert_send_sync::<CompilerAtom>();
    assert_send_sync::<CompilerConstantValue>();
    assert_send_sync::<CompilerConstant>();

    let latin1 = string(&[0x00, 0xff]);
    assert_eq!(latin1.len(), 2);
    assert_eq!(latin1.latin1_units(), Some(&[0x00, 0xff][..]));
    assert_eq!(latin1.utf16_units(), None);
    assert_eq!(latin1.code_units().collect::<Vec<_>>(), [0x00, 0xff]);

    let wide = string(&[0x0100, 0xd800, 0xdc00, 0xd83d, 0xde00]);
    assert_eq!(wide.latin1_units(), None);
    assert_eq!(
        wide.utf16_units(),
        Some(&[0x0100, 0xd800, 0xdc00, 0xd83d, 0xde00][..])
    );
    assert_eq!(
        wide.code_units().collect::<Vec<_>>(),
        [0x0100, 0xd800, 0xdc00, 0xd83d, 0xde00]
    );
    assert_eq!(wide.clone(), wide);
}

#[test]
fn graph_owns_and_verifies_function_local_atom_payloads() {
    let hello = atom(&['h' as u16, 'i' as u16]);
    let function = graph_function(
        1,
        &[
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(quickjs_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        Arc::from([hello.clone()]),
        Arc::from([]),
    );

    let verified = graph(function, FunctionGraphVerificationLimits::default())
        .expect("owned atom exactly satisfies the body domain");
    assert_eq!(verified.root().atoms(), [hello]);
    assert_eq!(verified.usage().atoms(), 1);
    assert_eq!(verified.usage().string_payload_bytes(), 2);
}

#[test]
fn graph_rejects_atom_count_mismatch_and_duplicate_payloads() {
    let mismatch = graph_function(
        1,
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        Arc::from([]),
        Arc::from([]),
    );
    let error = graph(mismatch, FunctionGraphVerificationLimits::default())
        .expect_err("declared atom domains need exact owned payloads");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::AtomCountMismatch {
            declared: 1,
            entries: 0,
        }
    );

    let repeated = atom(&['x' as u16]);
    let duplicate = graph_function(
        2,
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        Arc::from([repeated.clone(), repeated]),
        Arc::from([]),
    );
    let error = graph(duplicate, FunctionGraphVerificationLimits::default())
        .expect_err("one function atom pool is content-interned");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DuplicateAtom {
            first: 0,
            duplicate: 1,
        }
    );
}

#[test]
fn graph_budgets_atom_entries_and_all_owned_string_payload_bytes() {
    let function = graph_function(
        1,
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        Arc::from([atom(&[0x0100, 0xd800])]),
        Arc::from([CompilerConstant::Value(CompilerConstantValue::String(
            string(&['0' as u16]),
        ))]),
    );
    let error = graph(
        function.clone(),
        FunctionGraphVerificationLimits::default().with_max_atoms(0),
    )
    .expect_err("aggregate atom entries are bounded before payload scans");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::LimitExceeded {
            resource: FunctionGraphResource::Atoms,
            limit: 0,
            observed: 1,
        }
    );

    let error = graph(
        function,
        FunctionGraphVerificationLimits::default().with_max_string_payload_bytes(4),
    )
    .expect_err("atoms and string constants share one aggregate payload budget");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::LimitExceeded {
            resource: FunctionGraphResource::StringPayloadBytes,
            limit: 4,
            observed: 5,
        }
    );

    let verified = graph(
        graph_function(
            1,
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            Arc::from([atom(&[0x0100, 0xd800])]),
            Arc::from([CompilerConstant::Value(CompilerConstantValue::String(
                string(&['0' as u16]),
            ))]),
        ),
        FunctionGraphVerificationLimits::default()
            .with_max_atoms(1)
            .with_max_string_payload_bytes(5),
    )
    .expect("aggregate atom and payload limits are inclusive");
    assert_eq!(verified.usage().atoms(), 1);
    assert_eq!(verified.usage().string_payload_bytes(), 5);
}

#[test]
fn graph_aggregates_payloads_but_atom_uniqueness_is_function_local() {
    let shared = atom(&['x' as u16]);
    let root = graph_function(
        1,
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        Arc::from([shared.clone()]),
        Arc::from([CompilerConstant::Function(FunctionTemplateId::new(1))]),
    );
    let child = graph_function(
        1,
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        Arc::from([shared]),
        Arc::from([CompilerConstant::Value(CompilerConstantValue::String(
            string(&['0' as u16]),
        ))]),
    );

    let verified = function_graph(
        vec![root, child],
        FunctionGraphVerificationLimits::default().with_max_string_payload_bytes(3),
    )
    .expect("equal contents in distinct function-local atom domains are valid");
    assert_eq!(verified.usage().atoms(), 2);
    assert_eq!(verified.usage().string_payload_bytes(), 3);
}

#[test]
fn graph_checks_all_pool_counts_before_atom_content() {
    let mut builder = BytecodeBuilder::new();
    builder
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("fixture instruction must encode");
    let flow = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            builder.into_bytes(),
            FunctionIndexDomains::new(2, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
        )
        .with_capture_layout(CompilerCaptureLayout::default())
        .with_constant_layout(CompilerConstantLayout::default()),
        VerificationLimits::default(),
    )
    .expect("fixture body must verify");
    let repeated = atom(&['x' as u16]);
    let malformed = UnverifiedCompilerFunction::new(
        Arc::new(flow),
        Arc::from([CompilerConstant::Value(CompilerConstantValue::String(
            string(&['0' as u16]),
        ))]),
        Arc::from([]),
    )
    .with_atom_pool(Arc::from([repeated.clone(), repeated]));

    let error = graph(malformed, FunctionGraphVerificationLimits::default())
        .expect_err("cardinality errors precede atom hashing and duplicate scans");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ConstantCountMismatch {
            declared: 0,
            entries: 1,
        }
    );
}
