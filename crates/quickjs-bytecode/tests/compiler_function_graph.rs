use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, CompilerAtom, CompilerCaptureLayout, CompilerCapturedBinding,
    CompilerClosureSource, CompilerConstant, CompilerConstantKind, CompilerConstantLayout,
    CompilerString, FinalOpcode, FunctionGraphResource, FunctionGraphVerificationErrorKind,
    FunctionGraphVerificationLimits, FunctionIndexDomains, FunctionTemplateId, Operands,
    UnverifiedCompilerFunction, UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph,
    UnverifiedFunctionHeader, VerificationLimits, VerifiedCompilerFunctionGraph,
    VerifiedControlFlow, verify_compiler_control_flow, verify_compiler_function_graph,
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
        builder
            .push(opcode, operands)
            .expect("test instruction must encode");
    }
    builder.into_bytes()
}

fn compiler_flow(
    instructions: &[(FinalOpcode, Operands)],
    argument_count: u32,
    local_count: u32,
    owned_captures: &[CompilerCapturedBinding],
    imported_closure_count: u32,
    constant_kinds: &[CompilerConstantKind],
) -> Arc<VerifiedControlFlow> {
    compiler_flow_with_atoms(
        instructions,
        0,
        argument_count,
        local_count,
        owned_captures,
        imported_closure_count,
        constant_kinds,
    )
}

#[allow(clippy::too_many_arguments)]
fn compiler_flow_with_atoms(
    instructions: &[(FinalOpcode, Operands)],
    atom_count: u32,
    argument_count: u32,
    local_count: u32,
    owned_captures: &[CompilerCapturedBinding],
    imported_closure_count: u32,
    constant_kinds: &[CompilerConstantKind],
) -> Arc<VerifiedControlFlow> {
    let domains = FunctionIndexDomains::new(
        atom_count,
        u32::try_from(constant_kinds.len()).expect("fixture constant count fits u32"),
        argument_count,
        local_count,
        imported_closure_count,
    );
    let header =
        UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
            false,
            argument_count,
            u32::try_from(owned_captures.len()).expect("fixture capture count fits u32"),
        );
    Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(encode(instructions), domains, header)
                .with_capture_layout(CompilerCaptureLayout::new(Arc::from(owned_captures)))
                .with_constant_layout(CompilerConstantLayout::new(Arc::from(constant_kinds))),
            VerificationLimits::default(),
        )
        .expect("fixture control flow must verify"),
    )
}

fn leaf_flow() -> Arc<VerifiedControlFlow> {
    compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        0,
        &[],
    )
}

fn parameter_eval_flow(scope_index: u16) -> Arc<VerifiedControlFlow> {
    let instructions = [
        (FinalOpcode::Push7, Operands::NoneInt),
        (
            FinalOpcode::Eval,
            Operands::NPopU16 {
                argument_count: 0,
                scope_index,
            },
        ),
        (FinalOpcode::Return, Operands::None),
    ];
    let header =
        UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
            false, 0, 0,
        )
        .with_simple_parameter_list(false);
    Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(&instructions),
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                header,
            )
            .with_capture_layout(CompilerCaptureLayout::default())
            .with_constant_layout(CompilerConstantLayout::default()),
            VerificationLimits::default(),
        )
        .expect("parameter eval fixture control flow"),
    )
}

fn function(
    control_flow: Arc<VerifiedControlFlow>,
    constants: &[u32],
    closure_sources: &[CompilerClosureSource],
) -> UnverifiedCompilerFunction {
    UnverifiedCompilerFunction::new(
        control_flow,
        Arc::from(
            constants
                .iter()
                .copied()
                .map(|index| CompilerConstant::Function(FunctionTemplateId::new(index)))
                .collect::<Vec<_>>(),
        ),
        Arc::from(closure_sources),
    )
}

fn graph(functions: Vec<UnverifiedCompilerFunction>) -> UnverifiedCompilerFunctionGraph {
    UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), functions.into())
}

#[test]
fn rejects_empty_graphs_and_out_of_bounds_roots_structurally() {
    let error = verify_compiler_function_graph(
        graph(Vec::new()),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("an empty graph has no root function");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::EmptyGraph
    );

    let error = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(
            FunctionTemplateId::new(1),
            Arc::from([function(leaf_flow(), &[], &[])]),
        ),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("the root identity must name a supplied record");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::RootOutOfBounds {
            root: FunctionTemplateId::new(1),
            functions: 1,
        }
    );
}

#[test]
fn direct_eval_marker_must_match_the_verified_callsite_family() {
    let eval_flow = compiler_flow(
        &[
            (FinalOpcode::Push7, Operands::NoneInt),
            (
                FinalOpcode::Eval,
                Operands::NPopU16 {
                    argument_count: 0,
                    scope_index: 1,
                },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        0,
        0,
        &[],
        0,
        &[],
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(eval_flow, &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("an encoded eval callsite requires the planner marker");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalMarkerMismatch {
            declared: false,
            encoded: true,
        }
    );

    let error = verify_compiler_function_graph(
        graph(vec![function(leaf_flow(), &[], &[]).with_direct_eval(true)]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a planner marker requires an encoded eval callsite");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalMarkerMismatch {
            declared: true,
            encoded: false,
        }
    );
}

#[test]
fn direct_eval_parameter_boundary_certifies_scope_zero_only_in_parameter_code() {
    let verified = verify_compiler_function_graph(
        graph(vec![
            function(parameter_eval_flow(0), &[], &[])
                .with_parameter_initialization_end(Some(2))
                .with_direct_eval(true),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("scope-zero eval precedes the parameter/body boundary");
    assert_eq!(verified.root().parameter_initialization_end(), Some(2));

    let error = verify_compiler_function_graph(
        graph(vec![
            function(parameter_eval_flow(0), &[], &[]).with_direct_eval(true),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("scope-zero eval requires an explicit parameter boundary");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalParameterPhaseMismatch {
            instruction: 1,
            scope_index: 0,
            boundary: None,
        }
    );

    let error = verify_compiler_function_graph(
        graph(vec![
            function(parameter_eval_flow(1), &[], &[])
                .with_parameter_initialization_end(Some(2))
                .with_direct_eval(true),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("body-scope eval cannot precede the boundary");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalParameterPhaseMismatch {
            instruction: 1,
            scope_index: 1,
            boundary: Some(2),
        }
    );
}

#[test]
fn function_initializer_prefix_start_is_bounded_by_the_verified_body() {
    let error = verify_compiler_function_graph(
        graph(vec![
            function(leaf_flow(), &[], &[]).with_function_initializer_prefix_start(2),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("initializer prefix start must name an instruction boundary");

    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::FunctionInitializerPrefixStartOutOfBounds {
            start: 2,
            instructions: 1,
        }
    );
}

#[test]
fn eval_reference_call_metadata_is_checked_and_retained() {
    let flow = compiler_flow(
        &[
            (FinalOpcode::Push7, Operands::NoneInt),
            (FinalOpcode::Push7, Operands::NoneInt),
            (
                FinalOpcode::Eval,
                Operands::NPopU16 {
                    argument_count: 0,
                    scope_index: 1,
                },
            ),
            (FinalOpcode::Swap, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ],
        0,
        0,
        &[],
        0,
        &[],
    );
    let verified = verify_compiler_function_graph(
        graph(vec![
            function(flow, &[], &[])
                .with_eval_reference_call_instructions(Arc::from([2]))
                .with_direct_eval(true),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("receiver-carrying eval metadata");

    assert_eq!(verified.root().eval_reference_call_instructions(), [2]);
}

#[test]
fn eval_reference_call_metadata_rejects_missing_receiver_and_wrong_opcode() {
    let error = verify_compiler_function_graph(
        graph(vec![
            function(parameter_eval_flow(1), &[], &[])
                .with_eval_reference_call_instructions(Arc::from([1]))
                .with_direct_eval(true),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a receiver-carrying eval needs one extra stack value");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::EvalReferenceCallReceiverMissing {
            instruction: 1,
            required: 2,
            actual: Some(1),
        }
    );

    let error = verify_compiler_function_graph(
        graph(vec![
            function(leaf_flow(), &[], &[]).with_eval_reference_call_instructions(Arc::from([0])),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("metadata cannot relabel an ordinary opcode as eval");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::EvalReferenceCallOpcodeMismatch {
            instruction: 0,
            opcode: FinalOpcode::ReturnUndef,
        }
    );
}

#[test]
fn accepts_a_nonzero_root_identity_without_reordering_records() {
    let parent = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[0],
        &[],
    );
    let verified = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(
            FunctionTemplateId::new(1),
            Arc::from([function(leaf_flow(), &[], &[]), parent]),
        ),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("dense identities do not require the root to occupy slot zero");

    assert_eq!(verified.root_id(), FunctionTemplateId::new(1));
    assert_eq!(
        verified.root().constants(),
        [CompilerConstant::Function(FunctionTemplateId::new(0))]
    );
    assert_eq!(verified.max_nesting_depth(), 2);
}

#[test]
fn verifies_nested_function_constants_and_both_capture_source_domains() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VerifiedCompilerFunctionGraph>();

    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            1,
            0,
            &[CompilerCapturedBinding::Argument(0)],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let middle = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            1,
            &[CompilerConstantKind::Function],
        ),
        &[2],
        &[CompilerClosureSource::ParentVariableReference(0)],
    );
    let inner = function(
        compiler_flow(
            &[
                (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentClosure(0)],
    );

    let verified = verify_compiler_function_graph(
        graph(vec![root, middle, inner]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("complete nested graph must verify");

    assert_eq!(verified.root_id(), FunctionTemplateId::new(0));
    assert_eq!(
        verified.root().constants(),
        [CompilerConstant::Function(FunctionTemplateId::new(1))]
    );
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(1))
            .expect("middle template")
            .closure_sources(),
        [CompilerClosureSource::ParentVariableReference(0)]
    );
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(2))
            .expect("inner template")
            .closure_sources(),
        [CompilerClosureSource::ParentClosure(0)]
    );
    assert_eq!(verified.max_nesting_depth(), 3);
    assert_eq!(verified.usage().functions(), 3);
    assert_eq!(verified.usage().bytecode_bytes(), 8);
    assert_eq!(verified.usage().instructions(), 6);
    assert_eq!(verified.usage().constants(), 2);
    assert_eq!(verified.usage().closure_variables(), 2);
    assert_eq!(verified.usage().closure_edge_evaluations(), 2);
    assert_eq!(verified.usage().transfer_evaluations(), 6);
}

#[test]
fn keeps_parent_owned_and_imported_cell_domains_distinct_on_one_edge() {
    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            2,
            0,
            &[CompilerCapturedBinding::Argument(1)],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let middle = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            1,
            0,
            &[CompilerCapturedBinding::Argument(0)],
            1,
            &[CompilerConstantKind::Function],
        ),
        &[2],
        &[CompilerClosureSource::ParentVariableReference(0)],
    );
    let child = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            2,
            &[],
        ),
        &[],
        &[
            CompilerClosureSource::ParentVariableReference(0),
            CompilerClosureSource::ParentClosure(0),
        ],
    );

    let verified = verify_compiler_function_graph(
        graph(vec![root, middle, child]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("same numeric index is valid in the two distinct parent cell domains");
    assert_eq!(
        verified
            .function(FunctionTemplateId::new(2))
            .expect("mixed-capture child")
            .closure_sources(),
        [
            CompilerClosureSource::ParentVariableReference(0),
            CompilerClosureSource::ParentClosure(0),
        ]
    );
}

#[test]
fn graph_requires_exact_compiler_owned_metadata() {
    let missing_layouts = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::default(),
            ),
            VerificationLimits::default(),
        )
        .expect("pool-free body may omit staged layouts"),
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(missing_layouts, &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("whole compiler graph requires explicit metadata");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::MissingCompilerCaptureLayout
    );

    let declared_constant = compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        0,
        &[CompilerConstantKind::Function],
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(declared_constant, &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("actual constants must match the certified domain");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ConstantCountMismatch {
            declared: 1,
            entries: 0,
        }
    );

    let declared_closure = compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        1,
        &[],
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(declared_closure, &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("actual closure sources must match the certified domain");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ClosureVariableCountMismatch {
            declared: 1,
            entries: 0,
        }
    );
}

#[test]
fn owned_constant_kinds_must_match_and_declared_atom_domains_require_tables() {
    let value_constant = compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        0,
        &[CompilerConstantKind::Value],
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(value_constant, &[0], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a function payload cannot satisfy a value-kind slot");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ConstantKindMismatch {
            index: 0,
            declared: CompilerConstantKind::Value,
            actual: CompilerConstantKind::Function,
        }
    );

    let atom_domain = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
                FunctionIndexDomains::new(1, 0, 0, 0, 0),
                UnverifiedFunctionHeader::default(),
            )
            .with_capture_layout(CompilerCaptureLayout::default())
            .with_constant_layout(CompilerConstantLayout::default()),
            VerificationLimits::default(),
        )
        .expect("unused atom domain reaches graph verification"),
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(atom_domain, &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("graph has no actual atom payload");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::MissingAtomPool { declared: 1 }
    );
}

#[test]
fn aggregate_preflight_limits_precede_per_entry_metadata_scans() {
    let missing_layouts = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::default(),
            ),
            VerificationLimits::default(),
        )
        .expect("pool-free body may omit staged layouts"),
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(missing_layouts, &[], &[])]),
        FunctionGraphVerificationLimits::default().with_max_nesting_depth(0),
    )
    .expect_err("minimum graph depth is charged before body metadata scans");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::LimitExceeded {
            resource: FunctionGraphResource::NestingDepth,
            limit: 0,
            observed: 1,
        }
    );

    let value_constant = compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        0,
        &[CompilerConstantKind::Value],
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(value_constant, &[9], &[])]),
        FunctionGraphVerificationLimits::default().with_max_constants(0),
    )
    .expect_err("aggregate limits are charged before kind and target validation");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::LimitExceeded {
            resource: FunctionGraphResource::Constants,
            limit: 0,
            observed: 1,
        }
    );

    let duplicate_closures = compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        2,
        &[],
    );
    let error = verify_compiler_function_graph(
        graph(vec![function(
            duplicate_closures,
            &[],
            &[
                CompilerClosureSource::ParentClosure(0),
                CompilerClosureSource::ParentClosure(0),
            ],
        )]),
        FunctionGraphVerificationLimits::default().with_max_closure_variables(0),
    )
    .expect_err("closure budget precedes uniqueness allocation and scanning");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::LimitExceeded {
            resource: FunctionGraphResource::ClosureVariables,
            limit: 0,
            observed: 2,
        }
    );
}

#[test]
fn validates_constant_targets_before_graph_topology() {
    let parent = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[2],
        &[],
    );
    let error = verify_compiler_function_graph(
        graph(vec![parent, function(leaf_flow(), &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("out-of-range target must precede unreachable-node reporting");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::FunctionConstantOutOfBounds {
            index: 0,
            target: FunctionTemplateId::new(2),
            functions: 2,
        }
    );
    assert_eq!(error.function(), Some(FunctionTemplateId::new(0)));
}

#[test]
fn validates_each_child_capture_recipe_against_its_parent_edge() {
    let parent_own_cell = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            2,
            0,
            &[CompilerCapturedBinding::Argument(1)],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let child_from_own = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentVariableReference(1)],
    );
    let error = verify_compiler_function_graph(
        graph(vec![parent_own_cell, child_from_own]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("child own-cell source must fit the parent table");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ClosureSourceOutOfBounds {
            child: FunctionTemplateId::new(1),
            closure: 0,
            source: CompilerClosureSource::ParentVariableReference(1),
            len: 1,
        }
    );

    let root_with_cell = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            1,
            0,
            &[CompilerCapturedBinding::Argument(0)],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let parent_import = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            1,
            &[CompilerConstantKind::Function],
        ),
        &[2],
        &[CompilerClosureSource::ParentVariableReference(0)],
    );
    let child_from_import = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentClosure(1)],
    );
    let error = verify_compiler_function_graph(
        graph(vec![root_with_cell, parent_import, child_from_import]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("child imported source must fit the parent environment");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ClosureSourceOutOfBounds {
            child: FunctionTemplateId::new(2),
            closure: 0,
            source: CompilerClosureSource::ParentClosure(1),
            len: 1,
        }
    );
    assert_eq!(error.function(), Some(FunctionTemplateId::new(1)));
}

#[test]
fn rejects_roots_requiring_an_unverified_external_environment() {
    let root = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentClosure(0)],
    );
    let error = verify_compiler_function_graph(
        graph(vec![root]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a standalone root cannot import an omitted parent cell");
    assert_eq!(error.function(), Some(FunctionTemplateId::new(0)));
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::RootRequiresEnvironment {
            closure_variables: 1,
        }
    );
}

#[test]
fn constructor_realm_global_sources_are_atom_bound_and_root_owned() {
    let root_flow = compiler_flow_with_atoms(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        1,
        0,
        0,
        &[],
        1,
        &[],
    );
    let root = function(
        Arc::clone(&root_flow),
        &[],
        &[CompilerClosureSource::ConstructorRealmGlobal(
            AtomPoolIndex::new(0),
        )],
    )
    .with_atom_pool(Arc::from([atom("realmValue")]));
    let verified = verify_compiler_function_graph(
        graph(vec![root]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("a root can carry an atom-bound constructor-realm global source");
    assert_eq!(
        verified.root().closure_sources(),
        [CompilerClosureSource::ConstructorRealmGlobal(
            AtomPoolIndex::new(0)
        )]
    );

    let out_of_bounds = function(
        root_flow,
        &[],
        &[CompilerClosureSource::ConstructorRealmGlobal(
            AtomPoolIndex::new(1),
        )],
    )
    .with_atom_pool(Arc::from([atom("realmValue")]));
    let error = verify_compiler_function_graph(
        graph(vec![out_of_bounds]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("the realm-global source atom must belong to the root atom pool");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ClosureSourceAtomOutOfBounds {
            closure: 0,
            atom: AtomPoolIndex::new(1),
            atoms: 1,
        }
    );

    let parent = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let child = function(
        compiler_flow_with_atoms(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            1,
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ConstructorRealmGlobal(
            AtomPoolIndex::new(0),
        )],
    )
    .with_atom_pool(Arc::from([atom("realmValue")]));
    let error = verify_compiler_function_graph(
        graph(vec![parent, child]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("only the graph root can originate a constructor-realm global source");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ConstructorRealmGlobalSourceNotRoot { closure: 0 }
    );
    assert_eq!(error.function(), Some(FunctionTemplateId::new(1)));
}

#[test]
fn direct_eval_sources_are_shape_bound_and_root_owned() {
    let caller = |index, environment_size| CompilerClosureSource::DirectEvalBinding {
        index,
        environment_size,
    };
    let variable = |index, environment_size| CompilerClosureSource::DirectEvalVariable {
        index,
        environment_size,
    };
    let root_flow = compiler_flow(
        &[(FinalOpcode::ReturnUndef, Operands::None)],
        0,
        0,
        &[],
        2,
        &[],
    );
    let root = function(Arc::clone(&root_flow), &[], &[caller(0, 3), variable(2, 3)]);
    let verified = verify_compiler_function_graph(
        graph(vec![root]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("a root can bind a sparse subset of one exact caller environment");
    assert_eq!(
        verified.root().closure_sources(),
        [caller(0, 3), variable(2, 3)]
    );

    let out_of_bounds = function(Arc::clone(&root_flow), &[], &[variable(3, 3), caller(0, 3)]);
    let error = verify_compiler_function_graph(
        graph(vec![out_of_bounds]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a caller-binding index must fit the bound environment shape");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalBindingOutOfBounds {
            closure: 0,
            index: 3,
            environment_size: 3,
        }
    );

    let inconsistent = function(root_flow, &[], &[caller(0, 2), variable(1, 3)]);
    let error = verify_compiler_function_graph(
        graph(vec![inconsistent]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("every caller-binding source must bind the same environment shape");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalEnvironmentSizeMismatch {
            expected: 2,
            closure: 1,
            actual: 3,
        }
    );

    let parent = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let child = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[variable(0, 1)],
    );
    let error = verify_compiler_function_graph(
        graph(vec![parent, child]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a descendant must forward the root caller binding");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DirectEvalBindingSourceNotRoot { closure: 0 }
    );
    assert_eq!(error.function(), Some(FunctionTemplateId::new(1)));
}

#[test]
fn rejects_duplicate_compiler_capture_sources() {
    let parent = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            1,
            0,
            &[CompilerCapturedBinding::Argument(0)],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let child = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            2,
            &[],
        ),
        &[],
        &[
            CompilerClosureSource::ParentVariableReference(0),
            CompilerClosureSource::ParentVariableReference(0),
        ],
    );
    let error = verify_compiler_function_graph(
        graph(vec![parent, child]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("compiler output must not alias one source through duplicate slots");
    assert_eq!(error.function(), Some(FunctionTemplateId::new(1)));
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::DuplicateClosureSource {
            first: 0,
            duplicate: 1,
            source: CompilerClosureSource::ParentVariableReference(0),
        }
    );
}

#[test]
fn rejects_cycles_and_unreachable_function_records_iteratively() {
    let one_child_flow = || {
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[CompilerConstantKind::Function],
        )
    };
    let error = verify_compiler_function_graph(
        graph(vec![
            function(one_child_flow(), &[1], &[]),
            function(one_child_flow(), &[0], &[]),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("function constant cycle must fail closed");
    assert!(matches!(
        error.kind(),
        FunctionGraphVerificationErrorKind::Cycle { .. }
    ));

    let error = verify_compiler_function_graph(
        graph(vec![
            function(one_child_flow(), &[1], &[]),
            function(one_child_flow(), &[2], &[]),
            function(one_child_flow(), &[1], &[]),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a reachable cycle after an acyclic root prefix must fail");
    assert!(matches!(
        error.kind(),
        FunctionGraphVerificationErrorKind::Cycle { .. }
    ));

    let error = verify_compiler_function_graph(
        graph(vec![
            function(leaf_flow(), &[], &[]),
            function(leaf_flow(), &[], &[]),
        ]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("all supplied functions must belong to the root graph");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::UnreachableFunction {
            function: FunctionTemplateId::new(1),
        }
    );
}

#[test]
fn permits_shared_children_and_charges_every_parent_capture_edge() {
    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::FClosure8, Operands::Const8(1)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[
                CompilerConstantKind::Function,
                CompilerConstantKind::Function,
            ],
        ),
        &[1, 2],
        &[],
    );
    let branch = || {
        function(
            compiler_flow(
                &[
                    (FinalOpcode::FClosure8, Operands::Const8(0)),
                    (FinalOpcode::Return, Operands::None),
                ],
                1,
                0,
                &[CompilerCapturedBinding::Argument(0)],
                0,
                &[CompilerConstantKind::Function],
            ),
            &[3],
            &[],
        )
    };
    let shared = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentVariableReference(0)],
    );
    let verified = verify_compiler_function_graph(
        graph(vec![root, branch(), branch(), shared]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("every incoming edge independently validates the shared capture plan");
    assert_eq!(verified.max_nesting_depth(), 3);
    assert_eq!(verified.functions().len(), 4);
    assert_eq!(verified.usage().closure_edge_evaluations(), 2);
}

#[test]
fn validates_a_shared_child_against_every_distinct_parent_domain() {
    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::FClosure8, Operands::Const8(1)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[
                CompilerConstantKind::Function,
                CompilerConstantKind::Function,
            ],
        ),
        &[1, 2],
        &[],
    );
    let parent = |owned_capture: bool| {
        function(
            compiler_flow(
                &[
                    (FinalOpcode::FClosure8, Operands::Const8(0)),
                    (FinalOpcode::Return, Operands::None),
                ],
                u32::from(owned_capture),
                0,
                if owned_capture {
                    &[CompilerCapturedBinding::Argument(0)]
                } else {
                    &[]
                },
                0,
                &[CompilerConstantKind::Function],
            ),
            &[3],
            &[],
        )
    };
    let shared = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentVariableReference(0)],
    );

    let error = verify_compiler_function_graph(
        graph(vec![root, parent(true), parent(false), shared]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("the second incoming edge lacks the shared child's required own cell");
    assert_eq!(error.function(), Some(FunctionTemplateId::new(2)));
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ClosureSourceOutOfBounds {
            child: FunctionTemplateId::new(3),
            closure: 0,
            source: CompilerClosureSource::ParentVariableReference(0),
            len: 0,
        }
    );
}

#[test]
fn shared_child_longest_depth_uses_every_incoming_path() {
    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::FClosure8, Operands::Const8(1)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[
                CompilerConstantKind::Function,
                CompilerConstantKind::Function,
            ],
        ),
        &[2, 1],
        &[],
    );
    let branch = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            0,
            0,
            &[],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[2],
        &[],
    );
    let verified = verify_compiler_function_graph(
        graph(vec![root, branch, function(leaf_flow(), &[], &[])]),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("shared child is reachable through both a direct and longer path");
    assert_eq!(verified.max_nesting_depth(), 3);
}

#[test]
fn every_aggregate_graph_budget_is_enforced_without_recursion() {
    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::FClosure8, Operands::Const8(0)),
                (FinalOpcode::Return, Operands::None),
            ],
            1,
            0,
            &[CompilerCapturedBinding::Argument(0)],
            0,
            &[CompilerConstantKind::Function],
        ),
        &[1],
        &[],
    );
    let child = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            0,
            0,
            &[],
            1,
            &[],
        ),
        &[],
        &[CompilerClosureSource::ParentVariableReference(0)],
    );
    let fixture = || graph(vec![root.clone(), child.clone()]);
    let defaults = FunctionGraphVerificationLimits::default();

    for (limits, resource) in [
        (
            defaults.with_max_functions(1),
            FunctionGraphResource::Functions,
        ),
        (
            defaults.with_max_nesting_depth(1),
            FunctionGraphResource::NestingDepth,
        ),
        (
            defaults.with_max_bytecode_bytes(3),
            FunctionGraphResource::BytecodeBytes,
        ),
        (
            defaults.with_max_instructions(2),
            FunctionGraphResource::Instructions,
        ),
        (
            defaults.with_max_constants(0),
            FunctionGraphResource::Constants,
        ),
        (
            defaults.with_max_closure_variables(0),
            FunctionGraphResource::ClosureVariables,
        ),
        (
            defaults.with_max_closure_edge_evaluations(0),
            FunctionGraphResource::ClosureEdgeEvaluations,
        ),
        (
            defaults.with_max_transfer_evaluations(2),
            FunctionGraphResource::TransferEvaluations,
        ),
    ] {
        let error = verify_compiler_function_graph(fixture(), limits)
            .expect_err("lowered aggregate budget must reject the fixture");
        assert!(
            matches!(
                error.kind(),
                FunctionGraphVerificationErrorKind::LimitExceeded {
                    resource: actual,
                    ..
                } if *actual == resource
            ),
            "{resource}"
        );
    }
}

#[test]
fn nesting_limit_uses_iterative_graph_work() {
    let branch_flow = compiler_flow(
        &[
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ],
        0,
        0,
        &[],
        0,
        &[CompilerConstantKind::Function],
    );
    let chain = |functions: u32| {
        let mut records = Vec::new();
        for index in 0..functions {
            if index + 1 == functions {
                records.push(function(leaf_flow(), &[], &[]));
            } else {
                records.push(function(Arc::clone(&branch_flow), &[index + 1], &[]));
            }
        }
        graph(records)
    };

    let verified =
        verify_compiler_function_graph(chain(256), FunctionGraphVerificationLimits::default())
            .expect("depth 256 is within the explicit graph limit");
    assert_eq!(verified.max_nesting_depth(), 256);

    let verified = verify_compiler_function_graph(
        chain(4_096),
        FunctionGraphVerificationLimits::default().with_max_nesting_depth(4_096),
    )
    .expect("a deep graph uses heap-backed iterative work queues");
    assert_eq!(verified.max_nesting_depth(), 4_096);

    let error =
        verify_compiler_function_graph(chain(257), FunctionGraphVerificationLimits::default())
            .expect_err("depth 257 exceeds the explicit graph limit");
    assert!(matches!(
        error.kind(),
        FunctionGraphVerificationErrorKind::LimitExceeded {
            resource: FunctionGraphResource::NestingDepth,
            limit: 256,
            observed: 257,
        }
    ));
}
