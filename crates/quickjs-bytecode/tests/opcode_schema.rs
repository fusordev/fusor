use quickjs_bytecode::{
    ALL_FINAL_OPCODES, ALL_TEMPORARY_OPCODES, ArgumentCountSource, FINAL_OPCODE_COUNT,
    FINAL_OPCODE_METADATA, FIRST_SHORT_OPCODE_BYTE, FinalOpcode, FinalOpcodeDecodeError,
    NON_SHORT_FINAL_OPCODE_COUNT, OperandFormat, QUICKJS_COMPATIBILITY_RELEASE,
    SHORT_FINAL_OPCODE_COUNT, SHORT_OPCODES_ENABLED, StackEffect, StackEffectError,
    TEMPORARY_OPCODE_COUNT, TEMPORARY_OPCODE_END_EXCLUSIVE, TEMPORARY_OPCODE_METADATA,
    TEMPORARY_OPCODE_START, TemporaryOpcode,
};

// The source-level port keeps QuickJS opcode ordering, while the engine's
// `define_private_field` carries one U8 kind tag for data/method/accessor
// installation. Pin the resulting engine table instead of claiming byte-table
// identity with upstream.
const ENGINE_TABLE_FINGERPRINT: u64 = 0xf124_7020_c377_e8bf;
const _: () = assert!(SHORT_OPCODES_ENABLED);

#[test]
fn compatibility_target_and_opcode_counts_match_the_pinned_header() {
    assert_eq!(QUICKJS_COMPATIBILITY_RELEASE, "2026-06-04");
    assert_eq!(FINAL_OPCODE_COUNT, 244);
    assert_eq!(NON_SHORT_FINAL_OPCODE_COUNT, 178);
    assert_eq!(SHORT_FINAL_OPCODE_COUNT, 66);
    assert_eq!(TEMPORARY_OPCODE_COUNT, 19);

    assert_eq!(FinalOpcode::Invalid.encoded_byte(), 0);
    assert_eq!(FinalOpcode::Nop.encoded_byte(), 177);
    assert_eq!(FinalOpcode::PushMinus1.encoded_byte(), 178);
    assert_eq!(FinalOpcode::TypeofIsFunction.encoded_byte(), 243);

    assert_eq!(TEMPORARY_OPCODE_START, 178);
    assert_eq!(TEMPORARY_OPCODE_END_EXCLUSIVE, 197);
    assert_eq!(FIRST_SHORT_OPCODE_BYTE, TEMPORARY_OPCODE_START);
}

#[test]
fn final_discriminants_are_contiguous_and_metadata_sizes_are_exact() {
    assert_eq!(ALL_FINAL_OPCODES.len(), FINAL_OPCODE_METADATA.len());

    for (index, (&opcode, &metadata)) in ALL_FINAL_OPCODES
        .iter()
        .zip(FINAL_OPCODE_METADATA)
        .enumerate()
    {
        assert_eq!(usize::from(opcode.encoded_byte()), index);
        assert_eq!(opcode.metadata(), metadata);
        assert_eq!(
            metadata.instruction_size(),
            metadata.operand_format().instruction_size(),
            "{}",
            metadata.mnemonic()
        );
    }
}

#[test]
fn temporary_discriminants_are_contiguous_and_overlap_only_by_explicit_encoding() {
    assert_eq!(ALL_TEMPORARY_OPCODES.len(), TEMPORARY_OPCODE_METADATA.len());

    for (index, (&opcode, &metadata)) in ALL_TEMPORARY_OPCODES
        .iter()
        .zip(TEMPORARY_OPCODE_METADATA)
        .enumerate()
    {
        assert_eq!(
            usize::from(opcode.encoded_byte()),
            usize::from(TEMPORARY_OPCODE_START) + index
        );
        assert_eq!(opcode.metadata(), metadata);
        assert_eq!(
            metadata.instruction_size(),
            metadata.operand_format().instruction_size(),
            "{}",
            metadata.mnemonic()
        );
    }

    assert_eq!(
        TemporaryOpcode::EnterScope.encoded_byte(),
        FinalOpcode::PushMinus1.encoded_byte()
    );
    assert_eq!(
        TemporaryOpcode::LineNum.encoded_byte(),
        FinalOpcode::GetLoc1.encoded_byte()
    );
    assert_ne!(
        TemporaryOpcode::EnterScope.mnemonic(),
        FinalOpcode::PushMinus1.mnemonic()
    );
}

#[test]
fn upstream_order_constraints_remain_encoded_in_discriminants() {
    assert_adjacent(FinalOpcode::PushConst, FinalOpcode::FClosure);
    assert_adjacent(FinalOpcode::GetVarUndef, FinalOpcode::GetVar);
    assert_adjacent(FinalOpcode::GetVar, FinalOpcode::PutVar);
    assert_adjacent(FinalOpcode::PutVar, FinalOpcode::PutVarInit);
    assert_adjacent(FinalOpcode::IfFalse, FinalOpcode::IfTrue);
    assert_adjacent(FinalOpcode::IfTrue, FinalOpcode::Goto);
    assert_adjacent(FinalOpcode::WithGetVar, FinalOpcode::WithPutVar);
    assert_adjacent(FinalOpcode::WithPutVar, FinalOpcode::WithDeleteVar);
    assert_adjacent(FinalOpcode::WithDeleteVar, FinalOpcode::WithMakeRef);
    assert_adjacent(FinalOpcode::WithMakeRef, FinalOpcode::WithGetRef);
    assert_adjacent(FinalOpcode::Nop, FinalOpcode::PushMinus1);
    assert_adjacent(FinalOpcode::PushConst8, FinalOpcode::FClosure8);
    assert_adjacent(FinalOpcode::IfFalse8, FinalOpcode::IfTrue8);
    assert_adjacent(FinalOpcode::IfTrue8, FinalOpcode::Goto8);
    assert_adjacent(FinalOpcode::Goto8, FinalOpcode::Goto16);
    assert_adjacent(FinalOpcode::Call0, FinalOpcode::Call1);
    assert_adjacent(FinalOpcode::Call1, FinalOpcode::Call2);
    assert_adjacent(FinalOpcode::Call2, FinalOpcode::Call3);

    assert_temp_adjacent(
        TemporaryOpcode::ScopeGetVarUndef,
        TemporaryOpcode::ScopeGetVar,
    );
    assert_temp_adjacent(TemporaryOpcode::ScopeGetVar, TemporaryOpcode::ScopePutVar);
    assert_temp_adjacent(
        TemporaryOpcode::ScopePutVar,
        TemporaryOpcode::ScopeDeleteVar,
    );
    assert_temp_adjacent(
        TemporaryOpcode::ScopeDeleteVar,
        TemporaryOpcode::ScopeMakeRef,
    );
    assert_temp_adjacent(TemporaryOpcode::ScopeMakeRef, TemporaryOpcode::ScopeGetRef);
    assert_temp_adjacent(
        TemporaryOpcode::ScopeGetRef,
        TemporaryOpcode::ScopePutVarInit,
    );
    assert_temp_adjacent(
        TemporaryOpcode::ScopePutVarInit,
        TemporaryOpcode::ScopeGetVarCheckThis,
    );
}

#[test]
fn checked_final_decoding_rejects_the_sentinel_and_unknown_bytes() {
    assert_eq!(
        FinalOpcode::decode(0),
        Err(FinalOpcodeDecodeError::ReservedInvalid)
    );
    assert_eq!(
        FinalOpcodeDecodeError::ReservedInvalid.to_string(),
        "opcode byte 0x00 is the reserved invalid opcode"
    );

    for byte in 1..=243 {
        let opcode = FinalOpcode::decode(byte).expect("known final opcode");
        assert_eq!(opcode.encoded_byte(), byte);
    }

    for byte in 244..=u8::MAX {
        let error = FinalOpcode::decode(byte).expect_err("unknown opcode");
        assert_eq!(error, FinalOpcodeDecodeError::Unknown { byte });
        assert_eq!(error.byte(), byte);
    }
}

#[test]
fn temporary_decoding_is_phase_specific_and_checked() {
    for byte in TEMPORARY_OPCODE_START..TEMPORARY_OPCODE_END_EXCLUSIVE {
        let opcode = TemporaryOpcode::decode(byte).expect("known temporary opcode");
        assert_eq!(opcode.encoded_byte(), byte);
        assert!(
            FinalOpcode::decode(byte).is_ok(),
            "the overlap is intentional"
        );
    }

    assert!(TemporaryOpcode::decode(TEMPORARY_OPCODE_START - 1).is_err());
    assert!(TemporaryOpcode::decode(TEMPORARY_OPCODE_END_EXCLUSIVE).is_err());
}

#[test]
fn dynamic_call_eval_and_array_effects_include_argument_counts() {
    assert_eq!(
        FinalOpcode::CallConstructor.stack_effect(Some(2)),
        Ok(StackEffect::new(4, 1))
    );
    assert_eq!(
        FinalOpcode::Call.stack_effect(Some(2)),
        Ok(StackEffect::new(3, 1))
    );
    assert_eq!(
        FinalOpcode::TailCall.stack_effect(Some(2)),
        Ok(StackEffect::new(3, 0))
    );
    assert_eq!(
        FinalOpcode::CallMethod.stack_effect(Some(2)),
        Ok(StackEffect::new(4, 1))
    );
    assert_eq!(
        FinalOpcode::TailCallMethod.stack_effect(Some(2)),
        Ok(StackEffect::new(4, 0))
    );
    assert_eq!(
        FinalOpcode::ArrayFrom.stack_effect(Some(2)),
        Ok(StackEffect::new(2, 1))
    );
    assert_eq!(
        FinalOpcode::Eval.stack_effect(Some(3)),
        Ok(StackEffect::new(4, 1))
    );

    for (opcode, expected_pops) in [
        (FinalOpcode::Call0, 1),
        (FinalOpcode::Call1, 2),
        (FinalOpcode::Call2, 3),
        (FinalOpcode::Call3, 4),
    ] {
        assert_eq!(opcode.argument_count_source(), ArgumentCountSource::Opcode);
        assert_eq!(
            opcode.stack_effect(None),
            Ok(StackEffect::new(expected_pops, 1))
        );
    }

    assert_eq!(
        FinalOpcode::Call.argument_count_source(),
        ArgumentCountSource::FirstU16Operand
    );
    assert_eq!(
        FinalOpcode::ArrayFrom.argument_count_source(),
        ArgumentCountSource::FirstU16Operand
    );
    assert_eq!(
        FinalOpcode::Eval.argument_count_source(),
        ArgumentCountSource::FirstU16Operand
    );
    assert_eq!(
        FinalOpcode::Add.argument_count_source(),
        ArgumentCountSource::None
    );
    assert_eq!(
        FinalOpcode::Add.stack_effect(None),
        Ok(StackEffect::new(2, 1))
    );
    assert_eq!(StackEffect::new(2, 1).net_change(), -1);
}

#[test]
fn dynamic_stack_effect_errors_are_structured() {
    assert_eq!(
        FinalOpcode::Call.stack_effect(None),
        Err(StackEffectError::MissingArgumentCount {
            opcode: FinalOpcode::Call
        })
    );
    assert_eq!(
        FinalOpcode::Add.stack_effect(Some(4)),
        Err(StackEffectError::UnexpectedArgumentCount {
            opcode: FinalOpcode::Add,
            argument_count: 4
        })
    );
    assert_eq!(
        FinalOpcode::Call0.stack_effect(Some(0)),
        Err(StackEffectError::UnexpectedArgumentCount {
            opcode: FinalOpcode::Call0,
            argument_count: 0
        })
    );

    assert_eq!(
        FinalOpcode::Call.stack_effect(Some(u16::MAX)),
        Ok(StackEffect::new(65_536, 1))
    );
    assert_eq!(
        FinalOpcode::CallConstructor.stack_effect(Some(u16::MAX)),
        Ok(StackEffect::new(65_537, 1))
    );
}

#[test]
fn engine_opcode_table_fingerprint_is_stable() {
    assert_eq!(table_fingerprint(), ENGINE_TABLE_FINGERPRINT);
}

fn assert_adjacent(left: FinalOpcode, right: FinalOpcode) {
    assert_eq!(left.encoded_byte() + 1, right.encoded_byte());
}

fn assert_temp_adjacent(left: TemporaryOpcode, right: TemporaryOpcode) {
    assert_eq!(left.encoded_byte() + 1, right.encoded_byte());
}

fn table_fingerprint() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;

    for (&opcode, &metadata) in ALL_FINAL_OPCODES.iter().zip(FINAL_OPCODE_METADATA) {
        hash_row(&mut hash, b'F', opcode.mnemonic(), metadata);
    }
    for (&opcode, &metadata) in ALL_TEMPORARY_OPCODES.iter().zip(TEMPORARY_OPCODE_METADATA) {
        hash_row(&mut hash, b'T', opcode.mnemonic(), metadata);
    }

    hash
}

fn hash_row(hash: &mut u64, kind: u8, mnemonic: &str, metadata: quickjs_bytecode::OpcodeMetadata) {
    hash_bytes(hash, &[kind, b'|']);
    hash_bytes(hash, mnemonic.as_bytes());
    hash_bytes(hash, b"|");
    hash_bytes(hash, metadata.instruction_size().to_string().as_bytes());
    hash_bytes(hash, b"|");
    hash_bytes(hash, metadata.base_pops().to_string().as_bytes());
    hash_bytes(hash, b"|");
    hash_bytes(hash, metadata.base_pushes().to_string().as_bytes());
    hash_bytes(hash, b"|");
    hash_bytes(hash, metadata.operand_format().upstream_name().as_bytes());
    hash_bytes(hash, b"\n");
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[test]
fn every_operand_format_reports_the_expected_width() {
    for (format, width) in [
        (OperandFormat::None, 0),
        (OperandFormat::NoneInt, 0),
        (OperandFormat::NoneLoc, 0),
        (OperandFormat::NoneArg, 0),
        (OperandFormat::NoneVarRef, 0),
        (OperandFormat::NPopX, 0),
        (OperandFormat::U8, 1),
        (OperandFormat::I8, 1),
        (OperandFormat::Loc8, 1),
        (OperandFormat::Const8, 1),
        (OperandFormat::Label8, 1),
        (OperandFormat::U16, 2),
        (OperandFormat::I16, 2),
        (OperandFormat::Label16, 2),
        (OperandFormat::NPop, 2),
        (OperandFormat::Loc, 2),
        (OperandFormat::Arg, 2),
        (OperandFormat::VarRef, 2),
        (OperandFormat::NPopU16, 4),
        (OperandFormat::U32, 4),
        (OperandFormat::I32, 4),
        (OperandFormat::Const, 4),
        (OperandFormat::Label, 4),
        (OperandFormat::Atom, 4),
        (OperandFormat::AtomU8, 5),
        (OperandFormat::AtomU16, 6),
        (OperandFormat::LabelU16, 6),
        (OperandFormat::AtomLabelU8, 9),
        (OperandFormat::AtomLabelU16, 10),
    ] {
        assert_eq!(format.operand_width(), width, "{format:?}");
    }
}
