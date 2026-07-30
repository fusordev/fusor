use quickjs_bytecode::{
    BytecodeBuilder, BytecodePc, DecodeError, FinalOpcode, FunctionBitField, FunctionHeaderFlag,
    FunctionIndexDomains, FunctionKind, FunctionKindRequirement, Operands,
    UnsupportedVerifierFeature, UnverifiedFunctionBody, UnverifiedFunctionHeader,
    VerificationError, VerificationErrorKind, VerificationLimits, verify_control_flow,
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

fn serialized_flags(kind: FunctionKind) -> u16 {
    (kind as u16) << 4
}

fn header(
    kind: FunctionKind,
    js_mode: u8,
    defined_argument_count: u32,
    variable_reference_count: u32,
) -> UnverifiedFunctionHeader {
    UnverifiedFunctionHeader::new(
        serialized_flags(kind),
        js_mode,
        defined_argument_count,
        variable_reference_count,
    )
}

fn verify(
    bytecode: Vec<u8>,
    expected_stack_size: u32,
    domains: FunctionIndexDomains,
    header: UnverifiedFunctionHeader,
) -> Result<quickjs_bytecode::VerifiedControlFlow, VerificationError> {
    verify_control_flow(
        UnverifiedFunctionBody::new(bytecode, expected_stack_size, domains, header),
        VerificationLimits::default(),
    )
}

fn ordinary_body() -> Vec<u8> {
    encode(&[(FinalOpcode::ReturnUndef, Operands::None)])
}

#[test]
fn verified_header_retains_typed_metadata() {
    let verified = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 3, 0, 0),
        UnverifiedFunctionHeader::new(1, 0x01, 2, 3),
    )
    .expect("valid header metadata must verify");
    let header = verified.function_header();
    let flags = header.flags();
    let mode = header.mode();

    assert_eq!(header.kind(), FunctionKind::Normal);
    assert_eq!(header.defined_argument_count(), 2);
    assert_eq!(header.variable_reference_count(), 3);
    assert_eq!(flags.bits(), 1);
    assert!(flags.has_prototype());
    assert_eq!(mode.bits(), 0x01);
    assert!(mode.is_strict());
}

#[test]
fn stripped_ordinary_source_function_header_has_the_quickjs_flag_contract() {
    let raw = UnverifiedFunctionHeader::stripped_ordinary_source_function(true, 2);

    assert_eq!(raw.serialized_flags(), 0x0243);
    assert_eq!(raw.js_mode(), 0x01);
    assert_eq!(raw.defined_argument_count(), 2);
    assert_eq!(raw.variable_reference_count(), 0);

    let verified = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 2, 0, 0),
        raw,
    )
    .expect("compiler-owned ordinary header must verify");
    let header = verified.function_header();

    assert_eq!(header.kind(), FunctionKind::Normal);
    assert!(header.flags().has_prototype());
    assert!(header.flags().has_simple_parameter_list());
    assert!(header.flags().new_target_allowed());
    assert!(header.flags().arguments_allowed());
    assert!(!header.flags().has_debug());
    assert!(!header.flags().is_eval());
    assert!(header.mode().is_strict());
}

#[test]
fn stripped_compiler_header_can_declare_typed_variable_references() {
    let raw = UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
        true, 2, 3,
    );

    assert_eq!(raw.serialized_flags(), 0x0243);
    assert_eq!(raw.js_mode(), 0x01);
    assert_eq!(raw.defined_argument_count(), 2);
    assert_eq!(raw.variable_reference_count(), 3);
}

#[test]
fn retained_source_header_sets_the_quickjs_debug_flag() {
    let raw =
        UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(true, 2, 3);

    assert_eq!(raw.serialized_flags(), 0x0643);
    assert_eq!(raw.js_mode(), 0x01);
    assert_eq!(raw.defined_argument_count(), 2);
    assert_eq!(raw.variable_reference_count(), 3);
}

#[test]
fn ordinary_method_header_is_nonconstructable_without_claiming_a_home_object() {
    let raw = UnverifiedFunctionHeader::ordinary_method_with_variable_references(false, 1, 2);

    assert_eq!(raw.serialized_flags(), 0x0742);
    assert_eq!(raw.js_mode(), 0);
    assert_eq!(raw.defined_argument_count(), 1);
    assert_eq!(raw.variable_reference_count(), 2);

    let verified = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 1, 2, 0),
        raw,
    )
    .expect("ordinary object method header must verify");
    let flags = verified.function_header().flags();

    assert!(!flags.has_prototype());
    assert!(flags.has_simple_parameter_list());
    assert!(!flags.needs_home_object());
    assert!(flags.new_target_allowed());
    assert!(flags.super_allowed());
    assert!(flags.arguments_allowed());
    assert!(flags.has_debug());
}

#[test]
fn dynamic_function_script_header_is_debug_only_and_never_eval() {
    let raw = UnverifiedFunctionHeader::dynamic_function_script(3);

    assert_eq!(raw.serialized_flags(), 0x0400);
    assert_eq!(raw.js_mode(), 0);
    assert_eq!(raw.defined_argument_count(), 0);
    assert_eq!(raw.variable_reference_count(), 3);

    let verified = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 0, 3, 0),
        raw,
    )
    .expect("a non-eval Script header is structurally valid");
    let header = verified.function_header();

    assert_eq!(header.kind(), FunctionKind::Normal);
    assert_eq!(header.flags().bits(), 0x0400);
    assert!(header.flags().has_debug());
    assert!(!header.flags().is_eval());
    assert!(!header.flags().has_prototype());
    assert!(!header.flags().has_simple_parameter_list());
    assert!(!header.flags().arguments_allowed());
    assert!(!header.mode().is_strict());
}

#[test]
fn each_defined_boolean_header_flag_has_a_typed_getter() {
    for bit in [0, 1, 2, 3, 6, 7, 8, 9, 10, 11] {
        let verified = verify(
            ordinary_body(),
            0,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::new(1 << bit, 0, 0, 0),
        )
        .expect("each defined flag is valid independently for a normal function");
        let flags = verified.function_header().flags();

        assert_eq!(flags.bits(), 1 << bit);
        match bit {
            0 => assert!(flags.has_prototype()),
            1 => assert!(flags.has_simple_parameter_list()),
            2 => assert!(flags.is_derived_class_constructor()),
            3 => assert!(flags.needs_home_object()),
            6 => assert!(flags.new_target_allowed()),
            7 => assert!(flags.super_call_allowed()),
            8 => assert!(flags.super_allowed()),
            9 => assert!(flags.arguments_allowed()),
            10 => assert!(flags.has_debug()),
            11 => assert!(flags.is_eval()),
            _ => unreachable!("the table contains only defined boolean flags"),
        }
    }
}

#[test]
fn reserved_serialized_flag_bits_are_rejected_individually() {
    for reserved_bit in 12..16 {
        let value = 1_u16 << reserved_bit;
        let error = verify(
            ordinary_body(),
            0,
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::new(value, 0, 0, 0),
        )
        .expect_err("reserved serialized-function bits must fail closed");

        assert_eq!(
            error.kind(),
            &VerificationErrorKind::DisallowedFunctionBits {
                field: FunctionBitField::SerializedFlags,
                value,
                allowed_mask: 0x0fff,
                disallowed_bits: value,
            }
        );
        assert_eq!(error.pc(), None);
        assert_eq!(error.opcode(), None);
    }
}

#[test]
fn only_stored_js_mode_bits_are_accepted() {
    for js_mode in [0, 1] {
        verify(
            ordinary_body(),
            0,
            FunctionIndexDomains::default(),
            header(FunctionKind::Normal, js_mode, 0, 0),
        )
        .expect("stored JS mode bits must verify");
    }

    for js_mode in [0x02, 0x04, 0x08, 0x09, 0x0d, 0x10, 0xf0] {
        let error = verify(
            ordinary_body(),
            0,
            FunctionIndexDomains::default(),
            header(FunctionKind::Normal, js_mode, 0, 0),
        )
        .expect_err("bits outside the stored function-mode mask must fail closed");
        let value = u16::from(js_mode);

        assert_eq!(
            error.kind(),
            &VerificationErrorKind::DisallowedFunctionBits {
                field: FunctionBitField::JsMode,
                value,
                allowed_mask: 0x0001,
                disallowed_bits: value & !0x0001,
            }
        );
    }
}

#[test]
fn stored_function_mode_does_not_embed_async_frame_state() {
    let (async_body, async_stack_size, _) = suspension_body(FinalOpcode::ReturnAsync);
    let async_without_frame = verify(
        async_body,
        async_stack_size,
        FunctionIndexDomains::default(),
        header(FunctionKind::Async, 0, 0, 0),
    )
    .expect("QuickJS installs async-frame mode during execution");
    assert_eq!(
        async_without_frame.function_header().kind(),
        FunctionKind::Async
    );

    let error = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::default(),
        header(FunctionKind::Normal, 4, 0, 0),
    )
    .expect_err("the async bit belongs only to a synthesized runtime frame mode");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::DisallowedFunctionBits {
            field: FunctionBitField::JsMode,
            value: 4,
            allowed_mask: 0x0001,
            disallowed_bits: 4,
        }
    );
}

#[test]
fn prototype_and_derived_constructor_flags_require_a_normal_function() {
    let cases = [
        (0, FunctionHeaderFlag::HasPrototype),
        (2, FunctionHeaderFlag::DerivedClassConstructor),
    ];

    for (bit, flag) in cases {
        for kind in [
            FunctionKind::Generator,
            FunctionKind::Async,
            FunctionKind::AsyncGenerator,
        ] {
            let error = verify(
                suspension_body(FinalOpcode::ReturnAsync).0,
                1,
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::new(serialized_flags(kind) | (1 << bit), 0, 0, 0),
            )
            .expect_err("constructor-only flags must not describe suspendable functions");

            assert_eq!(
                error.kind(),
                &VerificationErrorKind::FunctionFlagNotAllowedForKind {
                    flag,
                    kind,
                    requirement: FunctionKindRequirement::Normal,
                }
            );
        }
    }
}

#[test]
fn prototype_and_derived_constructor_flags_are_mutually_exclusive() {
    let error = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::default(),
        UnverifiedFunctionHeader::new((1 << 0) | (1 << 2), 0, 0, 0),
    )
    .expect_err("QuickJS class constructors never use the ordinary prototype flag");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::ConflictingFunctionFlags {
            first: FunctionHeaderFlag::HasPrototype,
            second: FunctionHeaderFlag::DerivedClassConstructor,
        }
    );
}

#[test]
fn eval_header_flag_does_not_enable_deferred_eval_opcodes() {
    let error = verify(
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
        0,
        FunctionIndexDomains::default(),
        UnverifiedFunctionHeader::new(1 << 11, 0, 0, 0),
    )
    .expect_err("direct eval remains outside this verifier slice");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::UnsupportedOpcodeSemantics {
            feature: UnsupportedVerifierFeature::EvalScopeMetadata,
        }
    );
}

#[test]
fn defined_argument_count_cannot_exceed_argument_count() {
    verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 2, 0, 0),
        header(FunctionKind::Normal, 0, 2, 0),
    )
    .expect("all declared arguments may be defined");

    let error = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 2, 0, 0),
        header(FunctionKind::Normal, 0, 3, 0),
    )
    .expect_err("defined arguments cannot exceed the argument domain");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::DefinedArgumentCountOutOfRange {
            defined: 3,
            argument_count: 2,
        }
    );
}

#[test]
fn variable_reference_count_obeys_the_quickjs_structural_maximum() {
    verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 65_534, 0, 0),
        header(FunctionKind::Normal, 0, 0, 65_534),
    )
    .expect("QuickJS permits 65,534 variable references");

    let error = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::default(),
        header(FunctionKind::Normal, 0, 0, 65_535),
    )
    .expect_err("QuickJS reserves the all-ones 16-bit index");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::MetadataCountOutOfRange {
            domain: quickjs_bytecode::FunctionCountDomain::VariableReferences,
            value: 65_535,
            maximum: 65_534,
        }
    );
}

#[test]
fn variable_reference_count_cannot_exceed_arguments_and_locals() {
    let error = verify(
        ordinary_body(),
        0,
        FunctionIndexDomains::new(0, 0, 2, 1, 0),
        header(FunctionKind::Normal, 0, 0, 4),
    )
    .expect_err("each variable-reference cell belongs to a captured argument or local");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::VariableReferenceCountOutOfRange {
            variable_references: 4,
            argument_count: 2,
            local_count: 1,
        }
    );
}

#[test]
fn suspension_opcodes_require_the_exact_quickjs_function_kinds() {
    let cases = [
        (
            FinalOpcode::InitialYield,
            FunctionKindRequirement::Generator,
        ),
        (FinalOpcode::Yield, FunctionKindRequirement::Generator),
        (
            FinalOpcode::YieldStar,
            FunctionKindRequirement::SynchronousGenerator,
        ),
        (
            FinalOpcode::AsyncYieldStar,
            FunctionKindRequirement::AsyncGenerator,
        ),
        (FinalOpcode::Await, FunctionKindRequirement::Async),
        (FinalOpcode::ReturnAsync, FunctionKindRequirement::NonNormal),
    ];
    let kinds = [
        FunctionKind::Normal,
        FunctionKind::Generator,
        FunctionKind::Async,
        FunctionKind::AsyncGenerator,
    ];

    for (opcode, requirement) in cases {
        for kind in kinds {
            let (bytecode, expected_stack_size, opcode_pc) = suspension_body(opcode);
            let result = verify(
                bytecode,
                expected_stack_size,
                FunctionIndexDomains::default(),
                header(kind, 0, 0, 0),
            );
            let accepted = match opcode {
                FinalOpcode::InitialYield | FinalOpcode::Yield => {
                    matches!(kind, FunctionKind::Generator | FunctionKind::AsyncGenerator)
                }
                FinalOpcode::YieldStar => matches!(kind, FunctionKind::Generator),
                FinalOpcode::AsyncYieldStar => matches!(kind, FunctionKind::AsyncGenerator),
                FinalOpcode::Await => {
                    matches!(kind, FunctionKind::Async | FunctionKind::AsyncGenerator)
                }
                FinalOpcode::ReturnAsync => !matches!(kind, FunctionKind::Normal),
                _ => unreachable!("the case table contains only suspension opcodes"),
            };

            if accepted {
                let verified = result.expect("legal function-kind/opcode pair must verify");
                assert_eq!(verified.computed_stack_size(), expected_stack_size);
            } else {
                let error = result.expect_err("illegal function-kind/opcode pair must fail closed");
                assert_eq!(error.pc(), Some(opcode_pc));
                assert_eq!(error.opcode(), Some(opcode));
                assert_eq!(
                    error.kind(),
                    &VerificationErrorKind::OpcodeNotAllowedForFunctionKind { kind, requirement }
                );
            }
        }
    }
}

#[test]
fn ordinary_and_tail_returns_require_a_normal_function() {
    let kinds = [
        FunctionKind::Normal,
        FunctionKind::Generator,
        FunctionKind::Async,
        FunctionKind::AsyncGenerator,
    ];

    for opcode in [
        FinalOpcode::Return,
        FinalOpcode::ReturnUndef,
        FinalOpcode::TailCall,
        FinalOpcode::TailCallMethod,
    ] {
        for kind in kinds {
            let (bytecode, expected_stack_size, opcode_pc) = ordinary_terminator_body(opcode);
            let result = verify(
                bytecode,
                expected_stack_size,
                FunctionIndexDomains::default(),
                header(kind, 0, 0, 0),
            );

            if matches!(kind, FunctionKind::Normal) {
                result.expect("ordinary terminators belong to normal functions");
            } else {
                let error =
                    result.expect_err("ordinary cleanup must never run for a suspendable function");
                assert_eq!(error.pc(), Some(opcode_pc));
                assert_eq!(error.opcode(), Some(opcode));
                assert_eq!(
                    error.kind(),
                    &VerificationErrorKind::OpcodeNotAllowedForFunctionKind {
                        kind,
                        requirement: FunctionKindRequirement::Normal,
                    }
                );
            }
        }
    }
}

#[test]
fn unreachable_ordinary_return_is_rejected_in_a_generator() {
    let error = verify(
        encode(&[
            (FinalOpcode::Goto8, Operands::Label8(2)),
            (FinalOpcode::ReturnUndef, Operands::None),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::ReturnAsync, Operands::None),
        ]),
        1,
        FunctionIndexDomains::default(),
        header(FunctionKind::Generator, 0, 0, 0),
    )
    .expect_err("unreachable terminators remain inside the trust boundary");

    assert_eq!(error.pc(), Some(BytecodePc::new(2)));
    assert_eq!(error.opcode(), Some(FinalOpcode::ReturnUndef));
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::OpcodeNotAllowedForFunctionKind {
            kind: FunctionKind::Generator,
            requirement: FunctionKindRequirement::Normal,
        }
    );
}

#[test]
fn unreachable_suspension_opcode_is_still_kind_checked() {
    let error = verify(
        encode(&[
            (FinalOpcode::Goto8, Operands::Label8(2)),
            (FinalOpcode::Await, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
        header(FunctionKind::Normal, 0, 0, 0),
    )
    .expect_err("unreachable instructions remain inside the trust boundary");

    assert_eq!(error.pc(), Some(BytecodePc::new(2)));
    assert_eq!(error.opcode(), Some(FinalOpcode::Await));
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::OpcodeNotAllowedForFunctionKind {
            kind: FunctionKind::Normal,
            requirement: FunctionKindRequirement::Async,
        }
    );
}

#[test]
fn complete_predecode_precedes_function_header_validation() {
    let error = verify(
        vec![FinalOpcode::PushI32.encoded_byte()],
        0,
        FunctionIndexDomains::default(),
        UnverifiedFunctionHeader::new(0x1000, 0, 0, 0),
    )
    .expect_err("truncated bytecode must be diagnosed before reserved header bits");

    assert!(matches!(
        error.kind(),
        VerificationErrorKind::Decode(DecodeError::TruncatedOperands { .. })
    ));
}

#[test]
fn complete_static_validation_precedes_function_kind_rejection() {
    let error = verify(
        encode(&[
            (FinalOpcode::GetLoc, Operands::Loc(0)),
            (FinalOpcode::Await, Operands::None),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]),
        0,
        FunctionIndexDomains::default(),
        header(FunctionKind::Normal, 0, 0, 0),
    )
    .expect_err("operand domains must be checked before opcode capabilities");

    assert_eq!(
        error.kind(),
        &VerificationErrorKind::IndexOutOfBounds {
            domain: quickjs_bytecode::OperandIndexDomain::Local,
            index: 0,
            len: 0,
        }
    );
}

fn suspension_body(opcode: FinalOpcode) -> (Vec<u8>, u32, BytecodePc) {
    match opcode {
        FinalOpcode::InitialYield => (
            encode(&[
                (FinalOpcode::InitialYield, Operands::None),
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::ReturnAsync, Operands::None),
            ]),
            1,
            BytecodePc::ZERO,
        ),
        FinalOpcode::Yield | FinalOpcode::YieldStar | FinalOpcode::AsyncYieldStar => (
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (opcode, Operands::None),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::ReturnAsync, Operands::None),
            ]),
            2,
            BytecodePc::new(1),
        ),
        FinalOpcode::Await => (
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::Await, Operands::None),
                (FinalOpcode::ReturnAsync, Operands::None),
            ]),
            1,
            BytecodePc::new(1),
        ),
        FinalOpcode::ReturnAsync => (
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::ReturnAsync, Operands::None),
            ]),
            1,
            BytecodePc::new(1),
        ),
        _ => panic!("test helper requires a suspension opcode"),
    }
}

fn ordinary_terminator_body(opcode: FinalOpcode) -> (Vec<u8>, u32, BytecodePc) {
    match opcode {
        FinalOpcode::Return => (
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::Return, Operands::None),
            ]),
            1,
            BytecodePc::new(1),
        ),
        FinalOpcode::ReturnUndef => (
            encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
            0,
            BytecodePc::ZERO,
        ),
        FinalOpcode::TailCall => (
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::TailCall, Operands::NPop { argument_count: 0 }),
            ]),
            1,
            BytecodePc::new(1),
        ),
        FinalOpcode::TailCallMethod => (
            encode(&[
                (FinalOpcode::Push0, Operands::NoneInt),
                (FinalOpcode::Push0, Operands::NoneInt),
                (
                    FinalOpcode::TailCallMethod,
                    Operands::NPop { argument_count: 0 },
                ),
            ]),
            2,
            BytecodePc::new(2),
        ),
        _ => panic!("test helper requires an ordinary function terminator"),
    }
}
