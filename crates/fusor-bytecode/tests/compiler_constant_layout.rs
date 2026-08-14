use std::sync::Arc;

use fusor_bytecode::{
    BytecodeBuilder, CompilerConstantKind, CompilerConstantLayout, DecodeError, FinalOpcode,
    FunctionIndexDomains, OperandIndexDomain, Operands, UnsupportedVerifierFeature,
    UnverifiedCompilerFunctionBody, UnverifiedFunctionBody, UnverifiedFunctionHeader,
    VerificationErrorKind, VerificationLimits, verify_compiler_control_flow, verify_control_flow,
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

fn layout(kinds: &[CompilerConstantKind]) -> CompilerConstantLayout {
    CompilerConstantLayout::new(Arc::from(kinds))
}

fn compiler_body(bytecode: Vec<u8>, constant_count: u32) -> UnverifiedCompilerFunctionBody {
    UnverifiedCompilerFunctionBody::new(
        bytecode,
        FunctionIndexDomains::new(0, constant_count, 0, 0, 0),
        UnverifiedFunctionHeader::default(),
    )
}

#[test]
fn compiler_function_constants_authorize_both_fclosure_widths() {
    let wide_layout = layout(&[CompilerConstantKind::Function]);
    let wide = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (FinalOpcode::FClosure, Operands::Const(0)),
                (FinalOpcode::Return, Operands::None),
            ]),
            1,
        )
        .with_constant_layout(wide_layout.clone()),
        VerificationLimits::default(),
    )
    .expect("typed wide function constant must verify");
    assert_eq!(wide.computed_stack_size(), 1);
    assert_eq!(wide.compiler_constant_layout(), Some(&wide_layout));

    let compact_layout = layout(&[CompilerConstantKind::Value, CompilerConstantKind::Function]);
    let compact = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (FinalOpcode::FClosure8, Operands::Const8(1)),
                (FinalOpcode::Return, Operands::None),
            ]),
            2,
        )
        .with_constant_layout(compact_layout.clone()),
        VerificationLimits::default(),
    )
    .expect("typed compact function constant must verify");
    assert_eq!(compact.compiler_constant_layout(), Some(&compact_layout));
}

#[test]
fn compiler_value_constants_authorize_both_push_const_widths() {
    for (opcode, operands) in [
        (FinalOpcode::PushConst, Operands::Const(0)),
        (FinalOpcode::PushConst8, Operands::Const8(0)),
    ] {
        let verified = verify_compiler_control_flow(
            compiler_body(
                encode(&[(opcode, operands), (FinalOpcode::Return, Operands::None)]),
                1,
            )
            .with_constant_layout(layout(&[CompilerConstantKind::Value])),
            VerificationLimits::default(),
        )
        .expect("typed value constant must verify");
        assert_eq!(verified.computed_stack_size(), 1, "{opcode}");
    }
}

#[test]
fn absent_and_explicit_empty_constant_layouts_remain_distinct() {
    let absent = verify_compiler_control_flow(
        compiler_body(encode(&[(FinalOpcode::ReturnUndef, Operands::None)]), 0),
        VerificationLimits::default(),
    )
    .expect("pool-free compiler body does not require a layout");
    assert_eq!(absent.compiler_constant_layout(), None);

    let explicit = layout(&[]);
    let explicit_empty = verify_compiler_control_flow(
        compiler_body(encode(&[(FinalOpcode::ReturnUndef, Operands::None)]), 0)
            .with_constant_layout(explicit.clone()),
        VerificationLimits::default(),
    )
    .expect("an explicit empty layout is valid");
    assert_eq!(explicit_empty.compiler_constant_layout(), Some(&explicit));
}

#[test]
fn nonempty_compiler_constant_domain_requires_a_layout() {
    let error = verify_compiler_control_flow(
        compiler_body(encode(&[(FinalOpcode::ReturnUndef, Operands::None)]), 1),
        VerificationLimits::default(),
    )
    .expect_err("declared constants without compiler typing must fail closed");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::MissingCompilerConstantLayout { constants: 1 }
    );
}

#[test]
fn compiler_constant_count_must_equal_the_declared_domain() {
    for (declared, kinds) in [
        (2, &[CompilerConstantKind::Function][..]),
        (
            1,
            &[CompilerConstantKind::Function, CompilerConstantKind::Value][..],
        ),
    ] {
        let error = verify_compiler_control_flow(
            compiler_body(
                encode(&[(FinalOpcode::ReturnUndef, Operands::None)]),
                declared,
            )
            .with_constant_layout(layout(kinds)),
            VerificationLimits::default(),
        )
        .expect_err("under- or over-supplied compiler typing must fail closed");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::CompilerConstantCountMismatch {
                declared,
                entries: u64::try_from(kinds.len()).expect("fixture length fits u64"),
            }
        );
    }
}

#[test]
fn compiler_constant_opcode_must_match_the_typed_entry() {
    for (opcode, operands, skip) in [
        (FinalOpcode::FClosure, Operands::Const(0), 6),
        (FinalOpcode::FClosure8, Operands::Const8(0), 3),
    ] {
        let error = verify_compiler_control_flow(
            compiler_body(
                encode(&[
                    (FinalOpcode::Goto8, Operands::Label8(skip)),
                    (opcode, operands),
                    (FinalOpcode::ReturnUndef, Operands::None),
                ]),
                1,
            )
            .with_constant_layout(layout(&[CompilerConstantKind::Value])),
            VerificationLimits::default(),
        )
        .expect_err("wrongly typed constants fail even when unreachable");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::CompilerConstantKindMismatch {
                index: 0,
                expected: CompilerConstantKind::Function,
                actual: CompilerConstantKind::Value,
            }
        );
        assert_eq!(error.pc(), Some(fusor_bytecode::BytecodePc::new(2)));
        assert_eq!(error.opcode(), Some(opcode));
    }
}

#[test]
fn raw_function_constants_on_the_stack_remain_fail_closed() {
    for (opcode, operands, skip) in [
        (FinalOpcode::PushConst, Operands::Const(0), 6),
        (FinalOpcode::PushConst8, Operands::Const8(0), 3),
    ] {
        let error = verify_compiler_control_flow(
            compiler_body(
                encode(&[
                    (FinalOpcode::Goto8, Operands::Label8(skip)),
                    (opcode, operands),
                    (FinalOpcode::ReturnUndef, Operands::None),
                ]),
                1,
            )
            .with_constant_layout(layout(&[CompilerConstantKind::Function])),
            VerificationLimits::default(),
        )
        .expect_err("raw function templates need class-stack verification");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::UnsupportedOpcodeSemantics {
                feature: UnsupportedVerifierFeature::RawFunctionStack,
            }
        );
        assert_eq!(error.pc(), Some(fusor_bytecode::BytecodePc::new(2)));
        assert_eq!(error.opcode(), Some(opcode));
    }
}

#[test]
fn constant_operand_bounds_are_checked_before_constant_kind() {
    let error = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (FinalOpcode::FClosure, Operands::Const(1)),
                (FinalOpcode::Return, Operands::None),
            ]),
            1,
        )
        .with_constant_layout(layout(&[CompilerConstantKind::Value])),
        VerificationLimits::default(),
    )
    .expect_err("out-of-range constant must fail before type resolution");
    assert_eq!(
        error.kind(),
        &VerificationErrorKind::IndexOutOfBounds {
            domain: OperandIndexDomain::ConstantPool,
            index: 1,
            len: 1,
        }
    );
}

#[test]
fn complete_predecode_and_static_bounds_keep_priority_over_kind_errors() {
    let mut truncated = encode(&[(FinalOpcode::FClosure, Operands::Const(0))]);
    let truncated_pc =
        fusor_bytecode::BytecodePc::new(u32::try_from(truncated.len()).expect("fixture fits u32"));
    truncated.push(FinalOpcode::FClosure.encoded_byte());
    let decode_error = verify_compiler_control_flow(
        compiler_body(truncated, 1).with_constant_layout(layout(&[CompilerConstantKind::Value])),
        VerificationLimits::default(),
    )
    .expect_err("later truncation must win over earlier kind mismatch");
    assert!(matches!(
        decode_error.kind(),
        VerificationErrorKind::Decode(DecodeError::TruncatedOperands {
            pc,
            opcode: FinalOpcode::FClosure,
            ..
        }) if *pc == truncated_pc
    ));

    let bounds_error = verify_compiler_control_flow(
        compiler_body(
            encode(&[
                (FinalOpcode::FClosure, Operands::Const(0)),
                (FinalOpcode::FClosure, Operands::Const(1)),
                (FinalOpcode::Return, Operands::None),
            ]),
            1,
        )
        .with_constant_layout(layout(&[CompilerConstantKind::Value])),
        VerificationLimits::default(),
    )
    .expect_err("later bounds failure must win over earlier kind mismatch");
    assert_eq!(
        bounds_error.kind(),
        &VerificationErrorKind::IndexOutOfBounds {
            domain: OperandIndexDomain::ConstantPool,
            index: 1,
            len: 1,
        }
    );
    assert_eq!(bounds_error.pc(), Some(fusor_bytecode::BytecodePc::new(5)));
    assert_eq!(bounds_error.opcode(), Some(FinalOpcode::FClosure));
}

#[test]
fn serialized_constant_opcodes_remain_fail_closed() {
    for (opcode, operands) in [
        (FinalOpcode::PushConst, Operands::Const(0)),
        (FinalOpcode::FClosure, Operands::Const(0)),
        (FinalOpcode::PushConst8, Operands::Const8(0)),
        (FinalOpcode::FClosure8, Operands::Const8(0)),
    ] {
        let body = UnverifiedFunctionBody::new(
            encode(&[(opcode, operands), (FinalOpcode::Return, Operands::None)]),
            1,
            FunctionIndexDomains::new(0, 1, 0, 0, 0),
            UnverifiedFunctionHeader::default(),
        );
        let error = verify_control_flow(body, VerificationLimits::default())
            .expect_err("serialized constants require whole-function typing");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::UnsupportedOpcodeSemantics {
                feature: UnsupportedVerifierFeature::ConstantPoolTyping,
            },
            "{opcode}"
        );
        assert_eq!(error.pc(), Some(fusor_bytecode::BytecodePc::ZERO));
        assert_eq!(error.opcode(), Some(opcode));
    }
}

#[test]
fn compiler_constant_metadata_is_owned_and_thread_safe() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<CompilerConstantLayout>();
}
