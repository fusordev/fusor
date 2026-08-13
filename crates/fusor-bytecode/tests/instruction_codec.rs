use std::{any::TypeId, mem::size_of};

use fusor_bytecode::{
    ALL_FINAL_OPCODES, AtomPoolIndex, BytecodeBuilder, BytecodePc, DecodeError, EncodeError,
    FinalOpcode, FinalOpcodeDecodeError, Instruction, InstructionDecoder, InstructionError,
    MAX_ENCODED_OPERAND_BYTES, OperandDecodeError, OperandFormat, Operands, StackEffect,
};

#[test]
fn every_operand_format_round_trips_with_deterministic_little_endian_bytes() {
    let cases: [(Operands, &[u8]); 29] = [
        (Operands::None, &[]),
        (Operands::NoneInt, &[]),
        (Operands::NoneLoc, &[]),
        (Operands::NoneArg, &[]),
        (Operands::NoneVarRef, &[]),
        (Operands::U8(0xab), &[0xab]),
        (Operands::I8(-2), &[0xfe]),
        (Operands::Loc8(0x12), &[0x12]),
        (Operands::Const8(0x34), &[0x34]),
        (Operands::Label8(-128), &[0x80]),
        (Operands::U16(0x1234), &[0x34, 0x12]),
        (Operands::I16(-0x1234), &[0xcc, 0xed]),
        (Operands::Label16(-2), &[0xfe, 0xff]),
        (
            Operands::NPop {
                argument_count: 0x0201,
            },
            &[0x01, 0x02],
        ),
        (Operands::NPopX, &[]),
        (
            Operands::NPopU16 {
                argument_count: 0x1234,
                scope_index: 0xabcd,
            },
            &[0x34, 0x12, 0xcd, 0xab],
        ),
        (Operands::Loc(0x4567), &[0x67, 0x45]),
        (Operands::Arg(0x89ab), &[0xab, 0x89]),
        (Operands::VarRef(0xcdef), &[0xef, 0xcd]),
        (Operands::U32(0x1234_5678), &[0x78, 0x56, 0x34, 0x12]),
        (Operands::I32(-2), &[0xfe, 0xff, 0xff, 0xff]),
        (Operands::Const(0x89ab_cdef), &[0xef, 0xcd, 0xab, 0x89]),
        (Operands::Label(-0x0123_4568), &[0x98, 0xba, 0xdc, 0xfe]),
        (
            Operands::Atom(AtomPoolIndex::new(0x7654_3210)),
            &[0x10, 0x32, 0x54, 0x76],
        ),
        (
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0x1234_5678),
                value: 0x9a,
            },
            &[0x78, 0x56, 0x34, 0x12, 0x9a],
        ),
        (
            Operands::AtomU16 {
                atom: AtomPoolIndex::new(0x1234_5678),
                value: 0x9abc,
            },
            &[0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a],
        ),
        (
            Operands::AtomLabelU8 {
                atom: AtomPoolIndex::new(0x1234_5678),
                label: -2,
                value: 0x12,
            },
            &[0x78, 0x56, 0x34, 0x12, 0xfe, 0xff, 0xff, 0xff, 0x12],
        ),
        (
            Operands::AtomLabelU16 {
                atom: AtomPoolIndex::new(0x1234_5678),
                label: i32::MIN,
                value: 0x3456,
            },
            &[0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x80, 0x56, 0x34],
        ),
        (
            Operands::LabelU16 {
                label: 0x1234_5678,
                value: 0x9abc,
            },
            &[0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a],
        ),
    ];

    for (operands, expected) in cases {
        let encoded = operands.encode().expect("typed operands must encode");
        assert_eq!(encoded.as_bytes(), expected, "{operands:?}");
        assert_eq!(
            usize::from(operands.format().operand_width()),
            expected.len(),
            "{operands:?}"
        );
        assert_eq!(
            Operands::decode(operands.format(), expected),
            Ok(operands),
            "{operands:?}"
        );
    }
}

#[test]
fn atom_pool_indices_are_distinct_typed_operands_with_boundary_stable_bytes() {
    fn require_atom_pool_index(index: AtomPoolIndex) -> u32 {
        index.get()
    }

    assert_ne!(TypeId::of::<AtomPoolIndex>(), TypeId::of::<u32>());
    assert_eq!(size_of::<AtomPoolIndex>(), size_of::<u32>());
    let formatted_index = AtomPoolIndex::new(18);
    assert_eq!(formatted_index.to_string(), "18");
    assert_eq!(format!("{formatted_index:08x}"), "00000012");

    for raw in [u32::MIN, u32::MAX] {
        let index = AtomPoolIndex::new(raw);
        assert_eq!(require_atom_pool_index(index), raw);
        assert_eq!(AtomPoolIndex::from_le_bytes(index.to_le_bytes()), index);

        let expected = raw.to_le_bytes();
        let cases = [
            Operands::Atom(index),
            Operands::AtomU8 {
                atom: index,
                value: u8::MAX,
            },
            Operands::AtomU16 {
                atom: index,
                value: u16::MAX,
            },
            Operands::AtomLabelU8 {
                atom: index,
                label: i32::MIN,
                value: u8::MAX,
            },
            Operands::AtomLabelU16 {
                atom: index,
                label: i32::MAX,
                value: u16::MAX,
            },
        ];

        for operands in cases {
            assert_eq!(operands.atom_pool_index(), Some(index));
            let encoded = operands.encode().expect("atom operands must encode");
            assert_eq!(&encoded.as_bytes()[..4], &expected);
            assert_eq!(
                Operands::decode(operands.format(), encoded.as_bytes()),
                Ok(operands)
            );
        }
    }

    assert_eq!(Operands::U32(0).atom_pool_index(), None);
}

#[test]
fn every_operand_format_rejects_each_truncation_and_trailing_bytes() {
    for operands in all_operand_samples() {
        let encoded = operands.encode().expect("typed operands must encode");
        let expected = operands.format().operand_width();

        for actual in 0..encoded.as_bytes().len() {
            assert_eq!(
                Operands::decode(operands.format(), &encoded.as_bytes()[..actual]),
                Err(OperandDecodeError::LengthMismatch {
                    format: operands.format(),
                    expected_bytes: expected,
                    actual_bytes: actual,
                }),
                "{operands:?} truncated at {actual}"
            );
        }

        let mut trailing = encoded.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            Operands::decode(operands.format(), &trailing),
            Err(OperandDecodeError::LengthMismatch {
                format: operands.format(),
                expected_bytes: expected,
                actual_bytes: trailing.len(),
            }),
            "{operands:?} with trailing byte"
        );
    }
}

#[test]
fn every_executable_opcode_round_trips_and_rejects_truncation_at_every_byte() {
    for &opcode in ALL_FINAL_OPCODES {
        if opcode == FinalOpcode::Invalid {
            continue;
        }

        let operands = sample_operands(opcode.metadata().operand_format());
        let expected_instruction =
            Instruction::new(opcode, operands).expect("matching operand format");
        let mut builder = BytecodeBuilder::new();
        assert_eq!(
            builder.push(opcode, operands),
            Ok(BytecodePc::ZERO),
            "{opcode}"
        );
        let bytes = builder.as_bytes();

        let decoded = fusor_bytecode::decode_instruction(bytes, BytecodePc::ZERO)
            .expect("encoded instruction must decode");
        assert_eq!(decoded.pc(), BytecodePc::ZERO);
        assert_eq!(decoded.instruction(), expected_instruction);
        assert_eq!(
            decoded.next_pc().get(),
            u32::from(opcode.metadata().instruction_size())
        );

        for cut in 0..bytes.len() {
            let error = fusor_bytecode::decode_instruction(&bytes[..cut], BytecodePc::ZERO)
                .expect_err("every strict prefix is truncated");
            if cut == 0 {
                assert_eq!(
                    error,
                    DecodeError::MissingOpcode {
                        pc: BytecodePc::ZERO,
                        expected_bytes: 1,
                        remaining_bytes: 0,
                    },
                    "{opcode}"
                );
            } else {
                assert_eq!(
                    error,
                    DecodeError::TruncatedOperands {
                        pc: BytecodePc::ZERO,
                        opcode,
                        expected_bytes: opcode.metadata().operand_format().operand_width(),
                        remaining_bytes: cut - 1,
                    },
                    "{opcode} truncated at {cut}"
                );
            }
        }
    }
}

#[test]
fn decoder_rejects_reserved_unknown_and_out_of_bounds_program_counters() {
    assert_eq!(
        fusor_bytecode::decode_instruction(&[0], BytecodePc::ZERO),
        Err(DecodeError::InvalidOpcode {
            pc: BytecodePc::ZERO,
            opcode_byte: 0,
            source: FinalOpcodeDecodeError::ReservedInvalid,
        })
    );
    assert_eq!(
        fusor_bytecode::decode_instruction(&[248], BytecodePc::ZERO),
        Err(DecodeError::InvalidOpcode {
            pc: BytecodePc::ZERO,
            opcode_byte: 248,
            source: FinalOpcodeDecodeError::Unknown { byte: 248 },
        })
    );
    assert_eq!(
        fusor_bytecode::decode_instruction(
            &[FinalOpcode::Nop.encoded_byte()],
            BytecodePc::new(1)
        ),
        Err(DecodeError::MissingOpcode {
            pc: BytecodePc::new(1),
            expected_bytes: 1,
            remaining_bytes: 0,
        })
    );
    assert_eq!(
        fusor_bytecode::decode_instruction(
            &[FinalOpcode::Nop.encoded_byte()],
            BytecodePc::new(2)
        ),
        Err(DecodeError::PcOutOfBounds {
            pc: BytecodePc::new(2),
            bytecode_len: 1,
        })
    );
}

#[test]
fn decoder_iterator_tracks_typed_pcs_and_stops_after_an_error() {
    let mut builder = BytecodeBuilder::new();
    builder
        .push(FinalOpcode::PushI32, Operands::I32(-123_456))
        .expect("push_i32");
    builder
        .push(FinalOpcode::Call2, Operands::NPopX)
        .expect("call2");
    builder
        .push(
            FinalOpcode::ThrowError,
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0x1234_5678),
                value: 7,
            },
        )
        .expect("throw_error");

    let decoded = InstructionDecoder::new(builder.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("valid stream");
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[0].pc(), BytecodePc::new(0));
    assert_eq!(decoded[0].next_pc(), BytecodePc::new(5));
    assert_eq!(decoded[1].pc(), BytecodePc::new(5));
    assert_eq!(decoded[1].next_pc(), BytecodePc::new(6));
    assert_eq!(decoded[2].pc(), BytecodePc::new(6));
    assert_eq!(decoded[2].next_pc(), BytecodePc::new(12));
    assert_eq!(
        decoded[1].instruction().stack_effect(),
        Ok(StackEffect::new(3, 1))
    );

    let malformed_bytes = [248, FinalOpcode::Nop.encoded_byte()];
    let mut malformed = InstructionDecoder::new(&malformed_bytes);
    assert!(malformed.next().is_some_and(|result| result.is_err()));
    assert!(malformed.next().is_none());
}

#[test]
fn instruction_constructor_and_builder_reject_operand_format_mismatches() {
    assert_eq!(
        Instruction::new(FinalOpcode::Add, Operands::U8(1)),
        Err(InstructionError::OperandFormatMismatch {
            opcode: FinalOpcode::Add,
            expected: OperandFormat::None,
            actual: OperandFormat::U8,
        })
    );
    assert_eq!(
        Instruction::new(FinalOpcode::Invalid, Operands::None),
        Err(InstructionError::ReservedOpcode)
    );

    let mut builder = BytecodeBuilder::new();
    assert_eq!(
        builder.push(FinalOpcode::Add, Operands::U8(1)),
        Err(EncodeError::InvalidInstruction {
            pc: BytecodePc::ZERO,
            source: InstructionError::OperandFormatMismatch {
                opcode: FinalOpcode::Add,
                expected: OperandFormat::None,
                actual: OperandFormat::U8,
            },
        })
    );
    assert!(builder.as_bytes().is_empty());
}

#[test]
fn builder_enforces_resource_limits_and_u32_pc_overflow_without_partial_writes() {
    let mut limited = BytecodeBuilder::with_byte_limit(4);
    assert_eq!(
        limited.push(FinalOpcode::Nop, Operands::None),
        Ok(BytecodePc::ZERO)
    );
    let limited_prefix = limited.as_bytes().to_vec();
    assert_eq!(
        limited.push(FinalOpcode::PushI32, Operands::I32(1)),
        Err(EncodeError::ByteLimitExceeded {
            pc: BytecodePc::new(1),
            instruction_size: 5,
            encoded_bytes: 1,
            byte_limit: 4,
        })
    );
    assert_eq!(limited.as_bytes(), limited_prefix);
    assert_eq!(limited.len(), 1);
    assert_eq!(limited.next_pc(), BytecodePc::new(1));

    let mut near_end = BytecodeBuilder::with_origin(BytecodePc::new(u32::MAX - 1));
    assert_eq!(
        near_end.push(FinalOpcode::Nop, Operands::None),
        Ok(BytecodePc::new(u32::MAX - 1))
    );
    let near_end_prefix = near_end.as_bytes().to_vec();
    assert_eq!(
        near_end.push(FinalOpcode::Nop, Operands::None),
        Err(EncodeError::PcOverflow {
            pc: BytecodePc::new(u32::MAX),
            instruction_size: 1,
        })
    );
    assert_eq!(near_end.as_bytes(), near_end_prefix);
    assert_eq!(near_end.len(), 1);
    assert_eq!(near_end.next_pc(), BytecodePc::new(u32::MAX));
}

#[test]
fn operand_argument_counts_feed_the_existing_stack_effect_rules() {
    let call = Instruction::new(FinalOpcode::Call, Operands::NPop { argument_count: 7 })
        .expect("call operands");
    assert_eq!(call.dynamic_argument_count(), Some(7));
    assert_eq!(call.stack_effect(), Ok(StackEffect::new(8, 1)));

    let eval = Instruction::new(
        FinalOpcode::Eval,
        Operands::NPopU16 {
            argument_count: 3,
            scope_index: 99,
        },
    )
    .expect("eval operands");
    assert_eq!(eval.dynamic_argument_count(), Some(3));
    assert_eq!(eval.stack_effect(), Ok(StackEffect::new(4, 1)));

    let short_call =
        Instruction::new(FinalOpcode::Call3, Operands::NPopX).expect("short call operands");
    assert_eq!(short_call.dynamic_argument_count(), None);
    assert_eq!(short_call.stack_effect(), Ok(StackEffect::new(4, 1)));
}

#[test]
fn derived_define_class_carries_the_certified_heritage_pair() {
    let base = Instruction::new(
        FinalOpcode::DefineClass,
        Operands::AtomU8 {
            atom: AtomPoolIndex::new(0),
            value: 0,
        },
    )
    .expect("base define_class operands");
    let derived = Instruction::new(
        FinalOpcode::DefineClass,
        Operands::AtomU8 {
            atom: AtomPoolIndex::new(0),
            value: 1,
        },
    )
    .expect("derived define_class operands");

    assert_eq!(base.stack_effect(), Ok(StackEffect::new(2, 2)));
    assert_eq!(derived.stack_effect(), Ok(StackEffect::new(3, 2)));
}

#[test]
fn arbitrary_bytes_and_positions_are_total_and_error_iteration_is_fused() {
    const MAX_INSTRUCTION_BYTES: usize = MAX_ENCODED_OPERAND_BYTES + 1;

    for leading_byte in u8::MIN..=u8::MAX {
        for byte_len in 0..=MAX_INSTRUCTION_BYTES {
            let mut storage = [0_u8; MAX_INSTRUCTION_BYTES];
            for (index, byte) in storage.iter_mut().enumerate() {
                *byte = leading_byte.wrapping_add(u8::try_from(index).expect("bounded test index"));
            }
            let bytes = &storage[..byte_len];

            for pc in 0..=u32::try_from(byte_len + 1).expect("bounded test length") {
                let _ = fusor_bytecode::decode_instruction(bytes, BytecodePc::new(pc));
            }
            let _ = fusor_bytecode::decode_instruction(bytes, BytecodePc::new(u32::MAX));

            let mut decoder = InstructionDecoder::new(bytes);
            let mut terminated = false;
            for _ in 0..=bytes.len() {
                if decoder.next().is_none() {
                    terminated = true;
                    break;
                }
            }
            assert!(
                terminated,
                "decoder did not terminate for leading byte {leading_byte:#04x}, len {byte_len}"
            );
            assert!(decoder.next().is_none(), "decoder must remain fused");
            assert!(decoder.next().is_none(), "decoder must remain fused");
        }
    }
}

fn all_operand_samples() -> [Operands; 29] {
    [
        Operands::None,
        Operands::NoneInt,
        Operands::NoneLoc,
        Operands::NoneArg,
        Operands::NoneVarRef,
        Operands::U8(1),
        Operands::I8(-1),
        Operands::Loc8(2),
        Operands::Const8(3),
        Operands::Label8(-4),
        Operands::U16(5),
        Operands::I16(-6),
        Operands::Label16(-7),
        Operands::NPop { argument_count: 8 },
        Operands::NPopX,
        Operands::NPopU16 {
            argument_count: 9,
            scope_index: 10,
        },
        Operands::Loc(11),
        Operands::Arg(12),
        Operands::VarRef(13),
        Operands::U32(14),
        Operands::I32(-15),
        Operands::Const(16),
        Operands::Label(17),
        Operands::Atom(AtomPoolIndex::new(18)),
        Operands::AtomU8 {
            atom: AtomPoolIndex::new(19),
            value: 20,
        },
        Operands::AtomU16 {
            atom: AtomPoolIndex::new(21),
            value: 22,
        },
        Operands::AtomLabelU8 {
            atom: AtomPoolIndex::new(23),
            label: 24,
            value: 25,
        },
        Operands::AtomLabelU16 {
            atom: AtomPoolIndex::new(26),
            label: 27,
            value: 28,
        },
        Operands::LabelU16 {
            label: 29,
            value: 30,
        },
    ]
}

fn sample_operands(format: OperandFormat) -> Operands {
    all_operand_samples()
        .into_iter()
        .find(|operands| operands.format() == format)
        .expect("all operand formats have a sample")
}
