mod support;

use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, BytecodePc, CompilerCaptureLayout, CompilerCapturedBinding,
    CompilerConstantKind, CompilerConstantLayout, ControlFlowEdge, DecodeError, FinalOpcode,
    FunctionCountDomain, FunctionIndexDomains, FunctionKind, FunctionKindRequirement,
    InvalidControlFlowTargetReason, OperandIndexDomain, Operands, SecondaryOperandField,
    UnsupportedVerifierFeature, UnverifiedCompilerFunctionBody, UnverifiedFunctionBody,
    UnverifiedFunctionHeader, VerificationError, VerificationErrorKind, VerificationLimits,
    VerificationResource, VerifiedSuccessorKind, verify_compiler_control_flow, verify_control_flow,
};

use support::snapshot_verified_control_flow;

fn encode(instructions: &[(FinalOpcode, Operands)]) -> Vec<u8> {
    let mut builder = BytecodeBuilder::new();
    for &(opcode, operands) in instructions {
        builder
            .push(opcode, operands)
            .expect("test instruction must encode");
    }
    builder.into_bytes()
}

fn unverified(
    bytecode: Vec<u8>,
    expected_stack_size: u32,
    domains: FunctionIndexDomains,
) -> UnverifiedFunctionBody {
    UnverifiedFunctionBody::new(
        bytecode,
        expected_stack_size,
        domains,
        UnverifiedFunctionHeader::default(),
    )
}

fn verify(
    bytecode: Vec<u8>,
    expected_stack_size: u32,
    domains: FunctionIndexDomains,
) -> quickjs_bytecode::VerifiedControlFlow {
    verify_control_flow(
        unverified(bytecode, expected_stack_size, domains),
        VerificationLimits::default(),
    )
    .expect("test bytecode must verify")
}

fn reject(
    bytecode: Vec<u8>,
    expected_stack_size: u32,
    domains: FunctionIndexDomains,
) -> VerificationError {
    verify_control_flow(
        unverified(bytecode, expected_stack_size, domains),
        VerificationLimits::default(),
    )
    .expect_err("test bytecode must be rejected")
}

#[test]
fn compiler_control_flow_certificate_has_a_complete_stable_snapshot() {
    let bytecode = encode(&[
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse, Operands::Label(10)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Goto, Operands::Label(5)),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Nop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ]);
    let body = UnverifiedCompilerFunctionBody::new(
        bytecode,
        FunctionIndexDomains::new(1, 2, 2, 2, 2),
        UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
            true, 1, 2,
        ),
    )
    .with_capture_layout(
        CompilerCaptureLayout::new(Arc::from([
            CompilerCapturedBinding::Argument(0),
            CompilerCapturedBinding::ScopedLocal(1),
        ]))
        .with_mapped_arguments(Arc::from([0])),
    )
    .with_constant_layout(CompilerConstantLayout::new(Arc::from([
        CompilerConstantKind::Value,
        CompilerConstantKind::Function,
    ])));

    let verified = verify_compiler_control_flow(body, VerificationLimits::default())
        .expect("the characterization body must verify");

    let expected =
        include_str!("support/snapshots/compiler-control-flow.txt").replace("\r\n", "\n");
    assert_eq!(snapshot_verified_control_flow(&verified), expected);
}

#[test]
fn straight_line_certificate_tracks_boundaries_stack_and_successors() {
    let bytecode = encode(&[
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::Add, Operands::None),
        (FinalOpcode::Return, Operands::None),
    ]);
    let verified = verify(bytecode.clone(), 2, FunctionIndexDomains::default());

    assert_eq!(verified.bytecode(), bytecode);
    assert_eq!(verified.computed_stack_size(), 2);
    assert_eq!(verified.domains(), FunctionIndexDomains::default());
    assert_eq!(verified.instructions().len(), 4);
    assert_eq!(
        verified
            .instructions()
            .iter()
            .map(|instruction| instruction.entry_stack_depth())
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2), Some(1)]
    );
    assert_eq!(
        verified
            .instructions()
            .iter()
            .map(|instruction| instruction.successors().kind())
            .collect::<Vec<_>>(),
        [
            VerifiedSuccessorKind::Fallthrough,
            VerifiedSuccessorKind::Fallthrough,
            VerifiedSuccessorKind::Fallthrough,
            VerifiedSuccessorKind::Terminate,
        ]
    );

    let add_index = verified
        .instruction_index_at(BytecodePc::new(2))
        .expect("add starts at PC 2");
    assert_eq!(add_index.get(), 2);
    assert_eq!(
        verified
            .instruction(add_index)
            .expect("validated index belongs to certificate")
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Add
    );
    assert!(verified.is_instruction_start(BytecodePc::ZERO));
    assert!(!verified.is_instruction_start(BytecodePc::new(4)));
    assert_eq!(verified.instruction_index_at(BytecodePc::new(4)), None);
}

#[test]
fn explicit_throw_is_a_terminal_with_one_consumed_value() {
    let bytecode = encode(&[
        (FinalOpcode::Push7, Operands::NoneInt),
        (FinalOpcode::Throw, Operands::None),
    ]);
    let verified = verify(bytecode.clone(), 1, FunctionIndexDomains::default());

    assert_eq!(verified.computed_stack_size(), 1);
    assert_eq!(
        verified
            .instructions()
            .iter()
            .map(|instruction| instruction.entry_stack_depth())
            .collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
    assert_eq!(
        verified.instructions()[1].successors().kind(),
        VerifiedSuccessorKind::Terminate
    );

    verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect("a consumed thrown value leaves an empty compiler exit stack");
}

#[test]
fn explicit_throw_underflow_is_rejected_at_the_terminal() {
    let error = reject(
        encode(&[(FinalOpcode::Throw, Operands::None)]),
        0,
        FunctionIndexDomains::default(),
    );

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::StackUnderflow {
            required: 1,
            available: 0,
        }
    );
    assert_eq!(error.pc(), Some(BytecodePc::ZERO));
    assert_eq!(error.opcode(), Some(FinalOpcode::Throw));
}

#[test]
fn compiler_throw_rejects_values_stranded_below_the_thrown_value() {
    let bytecode = encode(&[
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Throw, Operands::None),
    ]);
    verify(bytecode.clone(), 2, FunctionIndexDomains::default());

    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect_err("compiler terminals must not strand an ordinary value below the throw");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::NonEmptyCompilerExitStack { remaining: 1 }
    );
    assert_eq!(error.pc(), Some(BytecodePc::new(2)));
    assert_eq!(error.opcode(), Some(FinalOpcode::Throw));
}

#[test]
fn compiler_generator_return_async_may_abandon_suspended_expression_values() {
    let bytecode = encode(&[
        (FinalOpcode::Object, Operands::None),
        (FinalOpcode::Push0, Operands::NoneInt),
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ReturnAsync, Operands::None),
    ]);
    for kind in [FunctionKind::Generator, FunctionKind::AsyncGenerator] {
        let generator_header = UnverifiedFunctionHeader::new((kind as u16) << 4, 0, 0, 0);
        let verified = verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                bytecode.clone(),
                FunctionIndexDomains::default(),
                generator_header,
            ),
            VerificationLimits::default(),
        )
        .expect("a generator return abandons its suspended enclosing expression state");
        assert_eq!(verified.instructions()[3].entry_stack_depth(), Some(3));
    }

    let async_header = UnverifiedFunctionHeader::new((FunctionKind::Async as u16) << 4, 0, 0, 0);
    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            async_header,
        ),
        VerificationLimits::default(),
    )
    .expect_err("an async function has no suspended expression stack to abandon");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::NonEmptyCompilerExitStack { remaining: 2 }
    );
}

#[test]
fn conditional_edges_use_exact_instruction_indices_and_equal_join_depths() {
    let bytecode = encode(&[
        (FinalOpcode::PushTrue, Operands::None),
        (FinalOpcode::IfFalse, Operands::Label(6)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ]);
    let verified = verify(bytecode, 1, FunctionIndexDomains::default());
    let branch = verified.instructions()[1];

    assert_eq!(branch.successors().kind(), VerifiedSuccessorKind::Branch);
    assert_eq!(
        branch
            .successors()
            .branch_target()
            .expect("taken target")
            .get(),
        4
    );
    assert_eq!(
        branch
            .successors()
            .fallthrough()
            .expect("not-taken target")
            .get(),
        2
    );
    assert_eq!(verified.instructions()[4].entry_stack_depth(), Some(0));
}

#[test]
fn backward_jump_widths_share_the_quickjs_pc_plus_one_base() {
    let cases = [
        (FinalOpcode::Goto8, Operands::Label8(-1)),
        (FinalOpcode::Goto16, Operands::Label16(-1)),
        (FinalOpcode::Goto, Operands::Label(-1)),
    ];

    for (opcode, operands) in cases {
        let verified = verify(
            encode(&[(opcode, operands)]),
            0,
            FunctionIndexDomains::default(),
        );
        assert_eq!(verified.instructions().len(), 1, "{opcode}");
        assert_eq!(
            verified.instructions()[0]
                .successors()
                .jump_target()
                .expect("self-loop target")
                .get(),
            0,
            "{opcode}"
        );
    }
}

#[test]
fn targets_must_be_strict_in_range_instruction_starts() {
    let into_operand = reject(
        encode(&[
            (FinalOpcode::Goto, Operands::Label(1)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        into_operand.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Jump,
            target: 2,
            bytecode_len: 6,
            reason: InvalidControlFlowTargetReason::NotInstructionBoundary,
        }
    );

    let at_end = reject(
        encode(&[(FinalOpcode::Goto, Operands::Label(4))]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        at_end.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Jump,
            target: 5,
            bytecode_len: 5,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );

    let before_start = reject(
        encode(&[(FinalOpcode::Goto8, Operands::Label8(-2))]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        before_start.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Jump,
            target: -1,
            bytecode_len: 2,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );
}

#[test]
fn fallthrough_must_not_leave_the_bytecode() {
    let error = reject(
        encode(&[(FinalOpcode::Nop, Operands::None)]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Fallthrough,
            target: 1,
            bytecode_len: 1,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );
}

#[test]
fn unsupported_fallthrough_shapes_are_validated_before_capability_rejection() {
    let ordinary = reject(
        encode(&[(FinalOpcode::PushConst, Operands::Const(0))]),
        0,
        FunctionIndexDomains::new(0, 1, 0, 0, 0),
    );
    assert_eq!(
        ordinary.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Fallthrough,
            target: 5,
            bytecode_len: 5,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );

    let catch = reject(
        encode(&[
            (FinalOpcode::ReturnUndef, Operands::None),
            (FinalOpcode::Catch, Operands::Label(-1)),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        catch.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Fallthrough,
            target: 6,
            bytecode_len: 6,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );

    let with_binding = reject(
        encode(&[
            (FinalOpcode::ReturnUndef, Operands::None),
            (
                FinalOpcode::WithGetVar,
                Operands::AtomLabelU8 {
                    atom: AtomPoolIndex::new(0),
                    label: -5,
                    value: 0,
                },
            ),
        ]),
        0,
        FunctionIndexDomains::new(1, 0, 0, 0, 0),
    );
    assert_eq!(
        with_binding.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Fallthrough,
            target: 11,
            bytecode_len: 11,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );

    let suspension = reject(
        encode(&[(FinalOpcode::Await, Operands::None)]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        suspension.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::Fallthrough,
            target: 1,
            bytecode_len: 1,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );
}

#[test]
fn return_async_requires_a_non_normal_function() {
    let error = reject(
        encode(&[(FinalOpcode::ReturnAsync, Operands::None)]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::OpcodeNotAllowedForFunctionKind {
            kind: FunctionKind::Normal,
            requirement: FunctionKindRequirement::NonNormal,
        }
    );
}

#[test]
fn catch_zero_and_gosub_continuation_are_checked_before_capability_rejection() {
    let catch_error = reject(
        encode(&[
            (FinalOpcode::Catch, Operands::Label(-1)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        catch_error.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::CatchHandler,
            target: 0,
            bytecode_len: 6,
            reason: InvalidControlFlowTargetReason::CatchTargetZero,
        }
    );

    let gosub_error = reject(
        encode(&[(FinalOpcode::Gosub, Operands::Label(-1))]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        gosub_error.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::FinallyContinuation,
            target: 5,
            bytecode_len: 5,
            reason: InvalidControlFlowTargetReason::OutsideBytecode,
        }
    );
}

#[test]
fn with_targets_use_the_pc_plus_five_base_before_failing_closed() {
    let domains = FunctionIndexDomains::new(1, 0, 0, 0, 0);
    let valid_target = reject(
        encode(&[
            (
                FinalOpcode::WithGetVar,
                Operands::AtomLabelU8 {
                    atom: AtomPoolIndex::new(0),
                    label: 5,
                    value: 0,
                },
            ),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        domains,
    );
    assert_eq!(
        valid_target.kind(),
        &VerificationErrorKind::UnsupportedOpcodeSemantics {
            feature: UnsupportedVerifierFeature::WithEnvironmentBranches,
        }
    );

    let invalid_target = reject(
        encode(&[
            (
                FinalOpcode::WithGetVar,
                Operands::AtomLabelU8 {
                    atom: AtomPoolIndex::new(0),
                    label: -4,
                    value: 0,
                },
            ),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        domains,
    );
    assert_eq!(
        invalid_target.kind(),
        &VerificationErrorKind::InvalidControlFlowTarget {
            edge: ControlFlowEdge::WithBinding,
            target: 1,
            bytecode_len: 11,
            reason: InvalidControlFlowTargetReason::NotInstructionBoundary,
        }
    );
}

#[test]
fn complete_predecode_reports_a_later_truncation_before_unsupported_semantics() {
    let mut bytecode = encode(&[(FinalOpcode::PushConst, Operands::Const(0))]);
    bytecode.push(FinalOpcode::PushI32.encoded_byte());

    let error = reject(bytecode, 0, FunctionIndexDomains::new(0, 1, 0, 0, 0));
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::Decode(DecodeError::TruncatedOperands {
            pc: BytecodePc::new(5),
            opcode: FinalOpcode::PushI32,
            expected_bytes: 4,
            remaining_bytes: 0,
        })
    );
}

#[test]
fn complete_static_validation_reports_later_bounds_errors_before_capability_rejection() {
    let error = reject(
        encode(&[
            (FinalOpcode::PushConst, Operands::Const(0)),
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
        ]),
        0,
        FunctionIndexDomains::new(0, 1, 0, 0, 0),
    );
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::IndexOutOfBounds {
            domain: OperandIndexDomain::AtomPool,
            index: 0,
            len: 0,
        }
    );
}

#[test]
fn every_function_local_operand_namespace_is_bounds_checked() {
    let cases = [
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(0)),
            FunctionIndexDomains::default(),
            OperandIndexDomain::AtomPool,
        ),
        (
            FinalOpcode::PushConst,
            Operands::Const(0),
            FunctionIndexDomains::default(),
            OperandIndexDomain::ConstantPool,
        ),
        (
            FinalOpcode::GetLoc,
            Operands::Loc(0),
            FunctionIndexDomains::default(),
            OperandIndexDomain::Local,
        ),
        (
            FinalOpcode::GetArg,
            Operands::Arg(0),
            FunctionIndexDomains::default(),
            OperandIndexDomain::Argument,
        ),
        (
            FinalOpcode::GetVarRef,
            Operands::VarRef(0),
            FunctionIndexDomains::default(),
            OperandIndexDomain::ClosureVariable,
        ),
    ];

    for (opcode, operands, domains, domain) in cases {
        let error = reject(encode(&[(opcode, operands)]), 0, domains);
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::IndexOutOfBounds {
                domain,
                index: 0,
                len: 0,
            },
            "{opcode}"
        );
    }
}

#[test]
fn explicit_short_and_implied_index_forms_accept_the_last_valid_entry() {
    let cases = [
        (
            FinalOpcode::GetLoc8,
            Operands::Loc8(3),
            FunctionIndexDomains::new(0, 0, 0, 4, 0),
        ),
        (
            FinalOpcode::GetLoc3,
            Operands::NoneLoc,
            FunctionIndexDomains::new(0, 0, 0, 4, 0),
        ),
        (
            FinalOpcode::GetArg,
            Operands::Arg(3),
            FunctionIndexDomains::new(0, 0, 4, 0, 0),
        ),
        (
            FinalOpcode::GetArg3,
            Operands::NoneArg,
            FunctionIndexDomains::new(0, 0, 4, 0, 0),
        ),
        (
            FinalOpcode::GetVarRef,
            Operands::VarRef(3),
            FunctionIndexDomains::new(0, 0, 0, 0, 4),
        ),
        (
            FinalOpcode::GetVarRef3,
            Operands::NoneVarRef,
            FunctionIndexDomains::new(0, 0, 0, 0, 4),
        ),
    ];

    for (opcode, operands, domains) in cases {
        let verified = verify(
            encode(&[(opcode, operands), (FinalOpcode::Return, Operands::None)]),
            1,
            domains,
        );
        assert_eq!(verified.computed_stack_size(), 1, "{opcode}");
    }
}

#[test]
fn in_bounds_atom_indices_and_multibyte_boundaries_are_certified() {
    let verified = verify(
        encode(&[
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]),
        1,
        FunctionIndexDomains::new(1, 0, 0, 0, 0),
    );

    assert!(verified.is_instruction_start(BytecodePc::ZERO));
    assert!(!verified.is_instruction_start(BytecodePc::new(1)));
    assert!(!verified.is_instruction_start(BytecodePc::new(4)));
    assert!(verified.is_instruction_start(BytecodePc::new(5)));
}

#[test]
fn make_reference_trailing_indices_are_checked_before_failing_closed() {
    let error = reject(
        encode(&[(
            FinalOpcode::MakeLocRef,
            Operands::AtomU16 {
                atom: AtomPoolIndex::new(0),
                value: 0,
            },
        )]),
        0,
        FunctionIndexDomains::new(1, 0, 0, 0, 0),
    );
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::IndexOutOfBounds {
            domain: OperandIndexDomain::Local,
            index: 0,
            len: 0,
        }
    );
}

#[test]
fn secondary_operand_domains_are_rejected_before_stack_or_capability_checks() {
    let cases = [
        (
            FinalOpcode::SpecialObject,
            Operands::U8(7),
            FunctionIndexDomains::default(),
            SecondaryOperandField::SpecialObjectKind,
            7,
        ),
        (
            FinalOpcode::Rest,
            Operands::U16(2),
            FunctionIndexDomains::new(0, 0, 1, 0, 0),
            SecondaryOperandField::RestFirstArgument,
            2,
        ),
        (
            FinalOpcode::Apply,
            Operands::U16(3),
            FunctionIndexDomains::default(),
            SecondaryOperandField::ApplyMagic,
            3,
        ),
        (
            FinalOpcode::ThrowError,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 5,
            },
            FunctionIndexDomains::new(1, 0, 0, 0, 0),
            SecondaryOperandField::ThrowErrorKind,
            5,
        ),
        (
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 3,
            },
            FunctionIndexDomains::new(1, 0, 0, 0, 0),
            SecondaryOperandField::DefineMethodFlags,
            3,
        ),
        (
            FinalOpcode::DefinePrivateField,
            Operands::U8(4),
            FunctionIndexDomains::default(),
            SecondaryOperandField::DefinePrivateFieldKind,
            4,
        ),
        (
            FinalOpcode::DefineClass,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 2,
            },
            FunctionIndexDomains::new(1, 0, 0, 0, 0),
            SecondaryOperandField::DefineClassFlags,
            2,
        ),
        (
            FinalOpcode::IteratorCall,
            Operands::U8(3),
            FunctionIndexDomains::default(),
            SecondaryOperandField::IteratorCallFlags,
            3,
        ),
    ];

    for (opcode, operands, domains, field, value) in cases {
        let error = reject(encode(&[(opcode, operands)]), 0, domains);
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::InvalidSecondaryOperand { field, value },
            "{opcode}"
        );
    }
}

#[test]
fn supported_secondary_operand_boundaries_reach_stack_analysis() {
    let special = verify(
        encode(&[
            (FinalOpcode::SpecialObject, Operands::U8(6)),
            (FinalOpcode::Return, Operands::None),
        ]),
        1,
        FunctionIndexDomains::default(),
    );
    assert_eq!(special.computed_stack_size(), 1);

    let rest = verify(
        encode(&[
            (FinalOpcode::Rest, Operands::U16(1)),
            (FinalOpcode::Return, Operands::None),
        ]),
        1,
        FunctionIndexDomains::new(0, 0, 1, 0, 0),
    );
    assert_eq!(rest.computed_stack_size(), 1);

    let apply = verify(
        encode(&[
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Apply, Operands::U16(2)),
            (FinalOpcode::Return, Operands::None),
        ]),
        3,
        FunctionIndexDomains::default(),
    );
    assert_eq!(apply.computed_stack_size(), 3);
}

#[test]
fn each_missing_semantic_capability_fails_closed_with_a_typed_reason() {
    let ordinary_cases = [
        (
            encode(&[
                (FinalOpcode::PushConst, Operands::Const(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::new(0, 1, 0, 0, 0),
            UnsupportedVerifierFeature::ConstantPoolTyping,
        ),
        (
            encode(&[
                (
                    FinalOpcode::DefineClassComputed,
                    Operands::AtomU8 {
                        atom: AtomPoolIndex::new(0),
                        value: 1,
                    },
                ),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::new(1, 0, 0, 0, 0),
            UnsupportedVerifierFeature::RawFunctionStack,
        ),
        (
            encode(&[
                (
                    FinalOpcode::Eval,
                    Operands::NPopU16 {
                        argument_count: 0,
                        scope_index: 0,
                    },
                ),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::default(),
            UnsupportedVerifierFeature::EvalScopeMetadata,
        ),
        (
            encode(&[
                (FinalOpcode::CloseLoc, Operands::Loc(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::new(0, 0, 0, 1, 0),
            UnsupportedVerifierFeature::CapturedBindingMetadata,
        ),
        (
            encode(&[
                (FinalOpcode::ForAwaitOfStart, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::default(),
            UnsupportedVerifierFeature::IteratorMarkers,
        ),
        (
            encode(&[
                (FinalOpcode::CopyDataProperties, Operands::U8(0)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::default(),
            UnsupportedVerifierFeature::PackedStackOffsets,
        ),
    ];

    for (bytecode, domains, feature) in ordinary_cases {
        let error = reject(bytecode, 0, domains);
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::UnsupportedOpcodeSemantics { feature }
        );
    }
}

#[test]
fn synchronous_for_of_markers_are_compiler_only_structural_inputs() {
    let bytecode = encode(&[
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::ForOfStart, Operands::None),
        (FinalOpcode::ForOfNext, Operands::U8(0)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::IteratorClose, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ]);

    verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode.clone(),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect("the compiler structural pass defers exact synchronous marker proof");

    let error = reject(bytecode, 5, FunctionIndexDomains::default());
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::UnsupportedOpcodeSemantics {
            feature: UnsupportedVerifierFeature::IteratorMarkers,
        }
    );
    assert_eq!(error.opcode(), Some(FinalOpcode::ForOfStart));

    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            encode(&[
                (FinalOpcode::ForAwaitOfStart, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect_err("async iterator construction still requires its compiler-owned input");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::StackUnderflow {
            required: 1,
            available: 0,
        }
    );

    for (opcode, operands) in [
        (FinalOpcode::ForAwaitOfNext, Operands::None),
        (FinalOpcode::IteratorGetValueDone, Operands::None),
    ] {
        let error = verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(&[
                    (opcode, operands),
                    (FinalOpcode::ReturnUndef, Operands::None),
                ]),
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::default(),
            ),
            VerificationLimits::default(),
        )
        .expect_err("async iterator marker families stay fail-closed");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::UnsupportedOpcodeSemantics {
                feature: UnsupportedVerifierFeature::IteratorMarkers,
            },
            "{opcode}"
        );
    }

    for (opcode, operands) in [
        (FinalOpcode::IteratorNext, Operands::None),
        (FinalOpcode::IteratorCall, Operands::U8(0)),
    ] {
        let error = verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(&[
                    (opcode, operands),
                    (FinalOpcode::ReturnUndef, Operands::None),
                ]),
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::default(),
            ),
            VerificationLimits::default(),
        )
        .expect_err("delegated iterator calls require their compiler-owned stack record");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::StackUnderflow {
                required: 4,
                available: 0,
            },
            "{opcode}"
        );
    }
}

#[test]
fn catch_and_nip_catch_have_typed_structural_successors_and_stack_depths() {
    let bytecode = encode(&[
        (FinalOpcode::Catch, Operands::Label(7)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::NipCatch, Operands::None),
        (FinalOpcode::Return, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ]);
    let verified = verify(bytecode.clone(), 2, FunctionIndexDomains::default());

    assert_eq!(
        verified
            .instructions()
            .iter()
            .map(|instruction| instruction.entry_stack_depth())
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2), Some(1), Some(1), Some(0)]
    );
    assert_eq!(
        verified.instructions()[0].successors().kind(),
        VerifiedSuccessorKind::Branch
    );
    assert_eq!(
        verified.instructions()[0]
            .successors()
            .fallthrough()
            .expect("normal catch edge")
            .get(),
        1
    );
    assert_eq!(
        verified.instructions()[0]
            .successors()
            .branch_target()
            .expect("exceptional catch edge")
            .get(),
        4
    );

    verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect("normal and exceptional catch completions both empty the compiler stack");
}

#[test]
fn compiler_throw_may_leave_only_a_structural_catch_marker_for_final_verification() {
    let bytecode = encode(&[
        (FinalOpcode::Catch, Operands::Label(6)),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Throw, Operands::None),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
    ]);

    verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect("the whole-bytecode typed stack pass owns catch-marker exit validation");
}

#[test]
fn gosub_and_ret_have_structural_successors_and_stack_depths() {
    let bytecode = encode(&[
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ]);
    let verified = verify(bytecode.clone(), 2, FunctionIndexDomains::default());

    assert_eq!(verified.computed_stack_size(), 2);
    assert_eq!(
        verified
            .instructions()
            .iter()
            .map(|instruction| instruction.entry_stack_depth())
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(1), Some(0), Some(2)]
    );
    assert_eq!(
        verified.instructions()[1].successors().kind(),
        VerifiedSuccessorKind::Branch
    );
    assert_eq!(
        verified.instructions()[1]
            .successors()
            .fallthrough()
            .expect("finally continuation")
            .get(),
        2
    );
    assert_eq!(
        verified.instructions()[1]
            .successors()
            .branch_target()
            .expect("finally subroutine")
            .get(),
        4
    );
    assert_eq!(
        verified.instructions()[4].successors().kind(),
        VerifiedSuccessorKind::Terminate
    );

    verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect("ret returns the pending completion to the synthetic continuation");
}

#[test]
fn gosub_return_address_counts_toward_the_structural_stack_limit() {
    let bytecode = encode(&[
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ]);
    let defaults = VerificationLimits::default();
    let limits = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        defaults.max_instructions_per_function(),
        defaults.max_constants_per_function(),
        defaults.max_atom_pool_entries(),
        defaults.max_transfer_evaluations(),
        1,
    );
    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        limits,
    )
    .expect_err("gosub must reserve one structural return-address slot");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::StackLimitExceeded { depth: 2, limit: 1 }
    );
}

#[test]
fn compiler_finally_abrupt_exits_defer_nonempty_stack_proof_to_final_authority() {
    let bytecode = encode(&[
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Return, Operands::None),
    ]);

    verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        VerificationLimits::default(),
    )
    .expect("typed final authority will prove whether the return is inside the finalizer");
}

#[test]
fn reachable_underflow_and_dynamic_pop_underflow_are_rejected() {
    let fixed = reject(
        encode(&[
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        fixed.kind(),
        &VerificationErrorKind::StackUnderflow {
            required: 1,
            available: 0,
        }
    );

    let dynamic = reject(
        encode(&[
            (FinalOpcode::Call, Operands::NPop { argument_count: 2 }),
            (FinalOpcode::Return, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        dynamic.kind(),
        &VerificationErrorKind::StackUnderflow {
            required: 3,
            available: 0,
        }
    );

    let array = reject(
        encode(&[
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 2 }),
            (FinalOpcode::Return, Operands::None),
        ]),
        1,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        array.kind(),
        &VerificationErrorKind::StackUnderflow {
            required: 2,
            available: 1,
        }
    );
}

#[test]
fn dynamic_short_call_effect_contributes_to_the_exact_maximum() {
    let verified = verify(
        encode(&[
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Call2, Operands::NPopX),
            (FinalOpcode::Return, Operands::None),
        ]),
        3,
        FunctionIndexDomains::default(),
    );
    assert_eq!(verified.computed_stack_size(), 3);
    assert_eq!(verified.instructions()[3].entry_stack_depth(), Some(3));
    assert_eq!(verified.instructions()[4].entry_stack_depth(), Some(1));
}

#[test]
fn array_from_dynamic_effect_has_an_exact_inclusive_stack_budget() {
    let bytecode = encode(&[
        (FinalOpcode::Push1, Operands::NoneInt),
        (FinalOpcode::Push2, Operands::NoneInt),
        (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 2 }),
        (FinalOpcode::Return, Operands::None),
    ]);
    let defaults = VerificationLimits::default();
    let limits = |max_stack_depth| {
        VerificationLimits::new(
            defaults.max_bytecode_bytes_per_function(),
            defaults.max_instructions_per_function(),
            defaults.max_constants_per_function(),
            defaults.max_atom_pool_entries(),
            defaults.max_transfer_evaluations(),
            max_stack_depth,
        )
    };

    let verified = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode.clone(),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        limits(2),
    )
    .expect("the exact two-slot array input fits an inclusive two-slot budget");
    assert_eq!(verified.computed_stack_size(), 2);
    assert_eq!(verified.instructions()[2].entry_stack_depth(), Some(2));
    assert_eq!(verified.instructions()[3].entry_stack_depth(), Some(1));

    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            bytecode,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::default(),
        ),
        limits(1),
    )
    .expect_err("one fewer operand-stack slot must fail closed");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::StackLimitExceeded { depth: 2, limit: 1 }
    );
}

#[test]
fn unequal_reachable_join_depths_are_rejected() {
    let error = reject(
        encode(&[
            (FinalOpcode::PushTrue, Operands::None),
            (FinalOpcode::IfFalse, Operands::Label(5)),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        1,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::InconsistentStackAtJoin {
            target: BytecodePc::new(7),
            established_depth: 0,
            incoming_depth: 1,
            incoming_from: BytecodePc::new(6),
        }
    );
}

#[test]
fn unreachable_stack_underflow_is_recorded_but_not_evaluated() {
    let verified = verify(
        encode(&[
            (FinalOpcode::Goto8, Operands::Label8(2)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(verified.instructions()[1].entry_stack_depth(), None);
    assert_eq!(verified.instructions()[2].entry_stack_depth(), Some(0));
}

#[test]
fn unsupported_unreachable_instructions_are_still_rejected() {
    let error = reject(
        encode(&[
            (FinalOpcode::Goto8, Operands::Label8(6)),
            (FinalOpcode::PushConst, Operands::Const(0)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::new(0, 1, 0, 0, 0),
    );
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::UnsupportedOpcodeSemantics {
            feature: UnsupportedVerifierFeature::ConstantPoolTyping,
        }
    );
}

#[test]
fn computed_stack_limit_and_serialized_maximum_are_both_enforced() {
    let defaults = VerificationLimits::default();
    let stack_one = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        defaults.max_instructions_per_function(),
        defaults.max_constants_per_function(),
        defaults.max_atom_pool_entries(),
        defaults.max_transfer_evaluations(),
        1,
    );
    let stack_error = verify_control_flow(
        unverified(
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::Push1, Operands::NoneInt),
                (FinalOpcode::Return, Operands::None),
            ]),
            1,
            FunctionIndexDomains::default(),
        ),
        stack_one,
    )
    .expect_err("computed depth two must exceed limit one");
    assert_eq!(
        stack_error.kind(),
        &VerificationErrorKind::StackLimitExceeded { depth: 2, limit: 1 }
    );

    let mismatch = reject(
        encode(&[
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Return, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
    );
    assert_eq!(
        mismatch.kind(),
        &VerificationErrorKind::SerializedStackSizeMismatch {
            serialized: 0,
            computed: 1,
        }
    );
}

#[test]
fn pinned_structural_stack_maximum_is_inclusive() {
    let maximum = quickjs_bytecode::MAX_OPERAND_STACK_DEPTH;
    let maximum_usize = usize::try_from(maximum).expect("u32 fits usize on supported targets");
    let mut accepted = vec![FinalOpcode::Push0.encoded_byte(); maximum_usize];
    accepted.push(FinalOpcode::Return.encoded_byte());
    let verified = verify(accepted, maximum, FunctionIndexDomains::default());
    assert_eq!(verified.computed_stack_size(), maximum);

    let mut rejected = vec![FinalOpcode::Push0.encoded_byte(); maximum_usize + 1];
    rejected.push(FinalOpcode::Return.encoded_byte());
    let error = reject(rejected, maximum, FunctionIndexDomains::default());
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::StackLimitExceeded {
            depth: u64::from(maximum) + 1,
            limit: maximum,
        }
    );
}

#[test]
fn byte_instruction_and_transfer_budgets_fail_with_observed_counts() {
    let defaults = VerificationLimits::default();
    let byte_limited = VerificationLimits::new(
        0,
        defaults.max_instructions_per_function(),
        defaults.max_constants_per_function(),
        defaults.max_atom_pool_entries(),
        defaults.max_transfer_evaluations(),
        defaults.max_stack_depth(),
    );
    let byte_error = verify_control_flow(
        unverified(
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            0,
            FunctionIndexDomains::default(),
        ),
        byte_limited,
    )
    .expect_err("one byte must exceed a zero-byte budget");
    assert_eq!(
        byte_error.kind(),
        &VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::BytecodeBytes,
            limit: 0,
            observed: 1,
        }
    );

    let instruction_limited = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        1,
        defaults.max_constants_per_function(),
        defaults.max_atom_pool_entries(),
        defaults.max_transfer_evaluations(),
        defaults.max_stack_depth(),
    );
    let instruction_error = verify_control_flow(
        unverified(
            encode(&[
                (FinalOpcode::Nop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            0,
            FunctionIndexDomains::default(),
        ),
        instruction_limited,
    )
    .expect_err("two instructions must exceed a one-instruction budget");
    assert_eq!(
        instruction_error.kind(),
        &VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::Instructions,
            limit: 1,
            observed: 2,
        }
    );

    let transfer_limited = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        defaults.max_instructions_per_function(),
        defaults.max_constants_per_function(),
        defaults.max_atom_pool_entries(),
        2,
        defaults.max_stack_depth(),
    );
    let transfer_error = verify_control_flow(
        unverified(
            encode(&[
                (FinalOpcode::Nop, Operands::None),
                (FinalOpcode::Nop, Operands::None),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]),
            0,
            FunctionIndexDomains::default(),
        ),
        transfer_limited,
    )
    .expect_err("three reachable instructions must exceed two transfers");
    assert_eq!(
        transfer_error.kind(),
        &VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::TransferEvaluations,
            limit: 2,
            observed: 3,
        }
    );
}

#[test]
fn hard_metadata_counts_are_checked_before_decode() {
    let cases = [
        (
            0,
            FunctionIndexDomains::new(0, 0, 65_535, 0, 0),
            FunctionCountDomain::Arguments,
        ),
        (
            0,
            FunctionIndexDomains::new(0, 0, 0, 65_535, 0),
            FunctionCountDomain::Locals,
        ),
        (
            0,
            FunctionIndexDomains::new(0, 0, 0, 0, 65_535),
            FunctionCountDomain::ClosureVariables,
        ),
        (
            65_535,
            FunctionIndexDomains::default(),
            FunctionCountDomain::ExpectedStackSize,
        ),
    ];

    for (expected_stack_size, domains, domain) in cases {
        let error = reject(Vec::new(), expected_stack_size, domains);
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::MetadataCountOutOfRange {
                domain,
                value: 65_535,
                maximum: 65_534,
            }
        );
    }
}

#[test]
fn constant_atom_and_configured_stack_limits_are_checked_before_decode() {
    let defaults = VerificationLimits::default();
    let constant_limited = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        defaults.max_instructions_per_function(),
        0,
        defaults.max_atom_pool_entries(),
        defaults.max_transfer_evaluations(),
        defaults.max_stack_depth(),
    );
    let constant_error = verify_control_flow(
        unverified(Vec::new(), 0, FunctionIndexDomains::new(0, 1, 0, 0, 0)),
        constant_limited,
    )
    .expect_err("one constant must exceed a zero-constant budget");
    assert_eq!(
        constant_error.kind(),
        &VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::Constants,
            limit: 0,
            observed: 1,
        }
    );

    let atom_limited = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        defaults.max_instructions_per_function(),
        defaults.max_constants_per_function(),
        0,
        defaults.max_transfer_evaluations(),
        defaults.max_stack_depth(),
    );
    let atom_error = verify_control_flow(
        unverified(Vec::new(), 0, FunctionIndexDomains::new(1, 0, 0, 0, 0)),
        atom_limited,
    )
    .expect_err("one atom entry must exceed a zero-atom budget");
    assert_eq!(
        atom_error.kind(),
        &VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::AtomPoolEntries,
            limit: 0,
            observed: 1,
        }
    );

    let invalid_stack_limit = VerificationLimits::new(
        defaults.max_bytecode_bytes_per_function(),
        defaults.max_instructions_per_function(),
        defaults.max_constants_per_function(),
        defaults.max_atom_pool_entries(),
        defaults.max_transfer_evaluations(),
        65_535,
    );
    let stack_limit_error = verify_control_flow(
        unverified(Vec::new(), 0, FunctionIndexDomains::default()),
        invalid_stack_limit,
    )
    .expect_err("configured stack limit above the structural maximum must fail");
    assert_eq!(
        stack_limit_error.kind(),
        &VerificationErrorKind::InvalidStackLimit {
            value: 65_535,
            maximum: 65_534,
        }
    );
}
