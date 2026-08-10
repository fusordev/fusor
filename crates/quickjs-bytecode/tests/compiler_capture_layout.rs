use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, CompilerCaptureLayout, CompilerCapturedBinding, FinalOpcode,
    FunctionIndexDomains, OperandIndexDomain, Operands, UnsupportedVerifierFeature,
    UnverifiedCompilerFunctionBody, UnverifiedFunctionBody, UnverifiedFunctionHeader,
    VerificationError, VerificationErrorKind, VerificationLimits, verify_compiler_control_flow,
    verify_control_flow,
};

fn encode(instructions: &[(FinalOpcode, Operands)]) -> Vec<u8> {
    let mut builder = BytecodeBuilder::new();
    for &(opcode, operands) in instructions {
        builder
            .push(opcode, operands)
            .expect("test instruction must encode");
    }
    builder.into_bytes()
}

fn layout(bindings: &[CompilerCapturedBinding]) -> CompilerCaptureLayout {
    CompilerCaptureLayout::new(Arc::from(bindings))
}

fn compiler_body(
    bytecode: Vec<u8>,
    domains: FunctionIndexDomains,
    bindings: &[CompilerCapturedBinding],
) -> UnverifiedCompilerFunctionBody {
    UnverifiedCompilerFunctionBody::new(
        bytecode,
        domains,
        UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
            false,
            0,
            u32::try_from(bindings.len()).expect("small fixture"),
        ),
    )
    .with_capture_layout(layout(bindings))
}

fn reject_compiler(
    bytecode: Vec<u8>,
    domains: FunctionIndexDomains,
    bindings: &[CompilerCapturedBinding],
) -> VerificationError {
    verify_compiler_control_flow(
        compiler_body(bytecode, domains, bindings),
        VerificationLimits::default(),
    )
    .expect_err("compiler bytecode must be rejected")
}

#[test]
fn compiler_capture_layout_is_dense_ordered_and_retained() {
    let bindings = [
        CompilerCapturedBinding::Argument(0),
        CompilerCapturedBinding::FunctionLocal(1),
        CompilerCapturedBinding::ScopedLocal(2),
    ];
    let verified = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (FinalOpcode::CloseLoc, Operands::Loc(2)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::new(0, 0, 1, 3, 0),
            &bindings,
        ),
        VerificationLimits::default(),
    )
    .expect("typed scoped capture authorizes close_loc");
    let retained = verified
        .compiler_capture_layout()
        .expect("compiler certificate retains capture metadata");

    assert_eq!(retained.bindings(), bindings);
    assert_eq!(
        retained.binding_for_variable_reference(0),
        Some(CompilerCapturedBinding::Argument(0))
    );
    assert_eq!(
        retained.binding_for_variable_reference(1),
        Some(CompilerCapturedBinding::FunctionLocal(1))
    );
    assert_eq!(
        retained.binding_for_variable_reference(2),
        Some(CompilerCapturedBinding::ScopedLocal(2))
    );
    assert_eq!(retained.binding_for_variable_reference(3), None);
}

#[test]
fn compiler_mapped_argument_positions_are_retained() {
    let verified = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            FunctionIndexDomains::new(0, 0, 4, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                false, 4, 0,
            ),
        )
        .with_capture_layout(
            CompilerCaptureLayout::default().with_mapped_arguments(Arc::from([1, 3])),
        ),
        VerificationLimits::default(),
    )
    .expect("ascending in-bounds mapped positions are compiler authority");

    assert_eq!(
        verified
            .compiler_capture_layout()
            .expect("layout retained")
            .mapped_arguments(),
        Some([1, 3].as_slice())
    );
}

#[test]
fn compiler_mapped_argument_positions_must_be_in_bounds_and_strictly_ascending() {
    let body = |mapped_arguments: Arc<[u32]>| {
        UnverifiedCompilerFunctionBody::new(
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            FunctionIndexDomains::new(0, 0, 2, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                false, 2, 0,
            ),
        )
        .with_capture_layout(
            CompilerCaptureLayout::default().with_mapped_arguments(mapped_arguments),
        )
    };

    let out_of_bounds =
        verify_compiler_control_flow(body(Arc::from([2])), VerificationLimits::default())
            .expect_err("the argument domain bounds mapped positions");
    assert_eq!(
        out_of_bounds.kind(),
        &VerificationErrorKind::CompilerMappedArgumentIndexOutOfBounds { index: 2, len: 2 }
    );

    let non_ascending: [Arc<[u32]>; 2] = [Arc::from([1, 1]), Arc::from([1, 0])];
    for mapped_arguments in non_ascending {
        let rejected_index = mapped_arguments[1];
        let error =
            verify_compiler_control_flow(body(mapped_arguments), VerificationLimits::default())
                .expect_err("mapped positions must be unique and ascending");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::CompilerMappedArgumentsNotAscending {
                previous: 1,
                index: rejected_index,
            }
        );
    }
}

#[test]
fn absent_and_explicitly_empty_compiler_capture_layouts_remain_distinct() {
    let bytecode = encode(&[(FinalOpcode::ReturnUndef, Operands::None)]);
    let absent = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode.clone(),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
        ),
        VerificationLimits::default(),
    )
    .expect("ordinary compiler bytecode does not require capture metadata");
    let explicit_empty = verify_compiler_control_flow(
        compiler_body(bytecode, FunctionIndexDomains::default(), &[]),
        VerificationLimits::default(),
    )
    .expect("an explicit empty layout is valid");

    assert_eq!(absent.compiler_capture_layout(), None);
    assert_eq!(
        explicit_empty
            .compiler_capture_layout()
            .expect("explicit layout must be retained")
            .bindings(),
        []
    );
}

#[test]
fn nonzero_compiler_variable_references_require_an_explicit_layout() {
    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            FunctionIndexDomains::new(0, 0, 1, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                false, 0, 1,
            ),
        ),
        VerificationLimits::default(),
    )
    .expect_err("compiler-owned variable-reference cells require typed metadata");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::MissingCompilerCaptureLayout {
            variable_references: 1,
        }
    );
}

#[test]
fn close_loc_without_compiler_capture_metadata_uses_the_fail_closed_capability_error() {
    let bytecode = encode(&[
        (FinalOpcode::CloseLoc, Operands::Loc(0)),
        (FinalOpcode::ReturnUndef, Operands::None),
    ]);
    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode.clone(),
            FunctionIndexDomains::new(0, 0, 0, 1, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
        ),
        VerificationLimits::default(),
    )
    .expect_err("absent compiler capture metadata must fail closed");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::UnsupportedOpcodeSemantics {
            feature: UnsupportedVerifierFeature::CapturedBindingMetadata,
        }
    );

    let explicit_empty_error =
        reject_compiler(bytecode, FunctionIndexDomains::new(0, 0, 0, 1, 0), &[]);
    assert_eq!(
        explicit_empty_error.kind(),
        &VerificationErrorKind::CloseLocRequiresScopedCapture { local: 0 }
    );
}

#[test]
fn compiler_capture_layout_count_must_match_header_variable_references() {
    let body = UnverifiedCompilerFunctionBody::new(
        encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
        FunctionIndexDomains::new(0, 0, 2, 0, 0),
        UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
            false, 0, 2,
        ),
    )
    .with_capture_layout(layout(&[CompilerCapturedBinding::Argument(0)]));
    let error = verify_compiler_control_flow(body, VerificationLimits::default())
        .expect_err("capture count mismatch must fail");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::CompilerCaptureCountMismatch {
            variable_references: 2,
            captures: 1,
        }
    );
}

#[test]
fn compiler_capture_bindings_must_be_in_frame_bounds() {
    let argument_error = reject_compiler(
        encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
        FunctionIndexDomains::new(0, 0, 1, 0, 0),
        &[CompilerCapturedBinding::Argument(1)],
    );
    assert_eq!(
        argument_error.kind(),
        &VerificationErrorKind::CompilerCaptureIndexOutOfBounds {
            binding: CompilerCapturedBinding::Argument(1),
            len: 1,
        }
    );

    for binding in [
        CompilerCapturedBinding::FunctionLocal(1),
        CompilerCapturedBinding::ScopedLocal(1),
    ] {
        let error = reject_compiler(
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            FunctionIndexDomains::new(0, 0, 0, 1, 0),
            &[binding],
        );
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::CompilerCaptureIndexOutOfBounds { binding, len: 1 }
        );
    }
}

#[test]
fn compiler_capture_bindings_are_unique_even_across_local_lifetimes() {
    for bindings in [
        [
            CompilerCapturedBinding::Argument(0),
            CompilerCapturedBinding::Argument(0),
        ],
        [
            CompilerCapturedBinding::FunctionLocal(0),
            CompilerCapturedBinding::ScopedLocal(0),
        ],
    ] {
        let domains = match bindings[0] {
            CompilerCapturedBinding::Argument(_) => FunctionIndexDomains::new(0, 0, 2, 0, 0),
            CompilerCapturedBinding::FunctionLocal(_) | CompilerCapturedBinding::ScopedLocal(_) => {
                FunctionIndexDomains::new(0, 0, 0, 2, 0)
            }
        };
        let error = reject_compiler(
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            domains,
            &bindings,
        );
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::DuplicateCompilerCapture {
                binding: bindings[1],
            }
        );
    }
}

#[test]
fn argument_and_local_zero_are_distinct_capture_identities() {
    let bindings = [
        CompilerCapturedBinding::Argument(0),
        CompilerCapturedBinding::ScopedLocal(0),
    ];
    let verified = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (FinalOpcode::CloseLoc, Operands::Loc(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::new(0, 0, 1, 1, 0),
            &bindings,
        ),
        VerificationLimits::default(),
    )
    .expect("argument and local namespaces must not alias");

    assert_eq!(
        verified
            .compiler_capture_layout()
            .expect("layout retained")
            .bindings(),
        bindings
    );
}

#[test]
fn close_loc_requires_an_explicit_scoped_local_capture() {
    for (bindings, local) in [
        (vec![CompilerCapturedBinding::FunctionLocal(0)], 0_u16),
        (vec![CompilerCapturedBinding::ScopedLocal(0)], 1_u16),
    ] {
        let error = reject_compiler(
            encode(&[
                (FinalOpcode::CloseLoc, Operands::Loc(local)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::new(0, 0, 0, 2, 0),
            &bindings,
        );
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::CloseLocRequiresScopedCapture {
                local: u32::from(local),
            }
        );
    }
}

#[test]
fn captured_argument_zero_cannot_authorize_close_loc_zero() {
    let error = reject_compiler(
        encode(&[
            (FinalOpcode::CloseLoc, Operands::Loc(0)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        FunctionIndexDomains::new(0, 0, 1, 1, 0),
        &[CompilerCapturedBinding::Argument(0)],
    );

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::CloseLocRequiresScopedCapture { local: 0 }
    );
}

#[test]
fn unreachable_close_loc_is_still_authorized_against_capture_metadata() {
    let error = reject_compiler(
        encode(&[
            (FinalOpcode::Goto8, Operands::Label8(4)),
            (FinalOpcode::CloseLoc, Operands::Loc(0)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        FunctionIndexDomains::new(0, 0, 0, 1, 0),
        &[CompilerCapturedBinding::FunctionLocal(0)],
    );

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::CloseLocRequiresScopedCapture { local: 0 }
    );
}

#[test]
fn close_loc_local_bounds_are_checked_before_capture_authorization() {
    let error = reject_compiler(
        encode(&[
            (FinalOpcode::CloseLoc, Operands::Loc(1)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        FunctionIndexDomains::new(0, 0, 0, 1, 0),
        &[CompilerCapturedBinding::ScopedLocal(0)],
    );

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::IndexOutOfBounds {
            domain: OperandIndexDomain::Local,
            index: 1,
            len: 1,
        }
    );
}

#[test]
fn serialized_close_loc_remains_fail_closed() {
    let error = verify_control_flow(
        UnverifiedFunctionBody::new(
            encode(&[
                (FinalOpcode::CloseLoc, Operands::Loc(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            0,
            FunctionIndexDomains::new(0, 0, 0, 1, 0),
            UnverifiedFunctionHeader::new(0, 0, 0, 1),
        ),
        VerificationLimits::default(),
    )
    .expect_err("serialized capture metadata is not trusted");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::UnsupportedOpcodeSemantics {
            feature: UnsupportedVerifierFeature::CapturedBindingMetadata,
        }
    );
}

#[test]
fn compiler_make_reference_opcodes_remain_fail_closed() {
    let cases = [
        (
            FinalOpcode::MakeLocRef,
            Operands::AtomU16 {
                atom: AtomPoolIndex::new(0),
                value: 0,
            },
            FunctionIndexDomains::new(1, 0, 0, 1, 0),
        ),
        (
            FinalOpcode::MakeArgRef,
            Operands::AtomU16 {
                atom: AtomPoolIndex::new(0),
                value: 0,
            },
            FunctionIndexDomains::new(1, 0, 1, 0, 0),
        ),
        (
            FinalOpcode::MakeVarRef,
            Operands::Atom(AtomPoolIndex::new(0)),
            FunctionIndexDomains::new(1, 0, 0, 0, 0),
        ),
    ];

    for (opcode, operands, domains) in cases {
        let error = reject_compiler(
            encode(&[
                (opcode, operands),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            domains,
            &[],
        );
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::UnsupportedOpcodeSemantics {
                feature: UnsupportedVerifierFeature::CapturedBindingMetadata,
            },
            "{opcode}"
        );
    }
}

#[test]
fn compiler_captured_reference_transaction_is_structurally_admitted() {
    let verified = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (
                    FinalOpcode::MakeVarRefRef,
                    Operands::AtomU16 {
                        atom: AtomPoolIndex::new(0),
                        value: 0,
                    },
                ),
                (FinalOpcode::GetRefValue, Operands::None),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::Mul, Operands::None),
                (FinalOpcode::Insert3, Operands::None),
                (FinalOpcode::PutRefValue, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ]),
            FunctionIndexDomains::new(1, 0, 0, 1, 1),
            &[CompilerCapturedBinding::FunctionLocal(0)],
        ),
        VerificationLimits::default(),
    )
    .expect("compiler-owned captured reference transaction is structurally valid");

    assert_eq!(verified.computed_stack_size(), 4);
}
