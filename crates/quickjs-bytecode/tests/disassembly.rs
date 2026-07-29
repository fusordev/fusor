use std::fmt;

use quickjs_bytecode::{
    AtomPoolIndex, BytecodeBuilder, BytecodePc, DecodeError, DisassemblyError, DisassemblyLimits,
    FinalOpcode, FinalOpcodeDecodeError, InstructionDecoder, Operands, render_disassembly,
};

const GENEROUS_LIMITS: DisassemblyLimits = DisassemblyLimits::new(1_000, 1_000_000);

#[test]
fn disassembly_has_stable_exact_output_and_computed_dynamic_stack_effects() {
    let mut builder = BytecodeBuilder::new();
    builder
        .push(FinalOpcode::PushI32, Operands::I32(-42))
        .expect("push_i32");
    builder
        .push(FinalOpcode::Call, Operands::NPop { argument_count: 2 })
        .expect("call");
    builder
        .push(FinalOpcode::Goto, Operands::Label(-5))
        .expect("goto");
    builder
        .push(
            FinalOpcode::WithGetVar,
            Operands::AtomLabelU8 {
                atom: AtomPoolIndex::new(0x1234),
                label: 7,
                value: 1,
            },
        )
        .expect("with_get_var");
    builder
        .push(FinalOpcode::Call2, Operands::NPopX)
        .expect("call2");

    let mut output = String::new();
    let summary = render_disassembly(
        InstructionDecoder::new(builder.as_bytes()),
        &mut output,
        GENEROUS_LIMITS,
    )
    .expect("valid disassembly");

    let expected = concat!(
        "pc=0x00000000 opcode=push_i32 operands=i32(-42) stack={pops=0,pushes=1}\n",
        "pc=0x00000005 opcode=call operands=npop(argument_count=2) stack={pops=3,pushes=1}\n",
        "pc=0x00000008 opcode=goto operands=label(displacement=-5) stack={pops=0,pushes=0}\n",
        "pc=0x0000000d opcode=with_get_var operands=atom_label_u8(pool_index=0x00001234, displacement=+7, value=1) stack={pops=1,pushes=0}\n",
        "pc=0x00000017 opcode=call2 operands=npopx stack={pops=3,pushes=1}\n",
    );
    assert_eq!(output, expected);
    assert_eq!(summary.instruction_count(), 5);
    assert_eq!(summary.output_bytes(), expected.len());
}

#[test]
fn every_operand_variant_has_an_unambiguous_stable_rendering() {
    let cases = [
        (Operands::None, "none"),
        (Operands::NoneInt, "none_int"),
        (Operands::NoneLoc, "none_loc"),
        (Operands::NoneArg, "none_arg"),
        (Operands::NoneVarRef, "none_var_ref"),
        (Operands::U8(255), "u8(255)"),
        (Operands::I8(-1), "i8(-1)"),
        (Operands::Loc8(2), "loc8(index=2)"),
        (Operands::Const8(3), "const8(index=3)"),
        (Operands::Label8(-4), "label8(displacement=-4)"),
        (Operands::U16(5), "u16(5)"),
        (Operands::I16(-6), "i16(-6)"),
        (Operands::Label16(7), "label16(displacement=+7)"),
        (
            Operands::NPop { argument_count: 8 },
            "npop(argument_count=8)",
        ),
        (Operands::NPopX, "npopx"),
        (
            Operands::NPopU16 {
                argument_count: 9,
                scope_index: 10,
            },
            "npop_u16(argument_count=9, scope_index=10)",
        ),
        (Operands::Loc(11), "loc(index=11)"),
        (Operands::Arg(12), "arg(index=12)"),
        (Operands::VarRef(13), "var_ref(index=13)"),
        (Operands::U32(14), "u32(14)"),
        (Operands::I32(-15), "i32(-15)"),
        (Operands::Const(16), "const(index=16)"),
        (Operands::Label(17), "label(displacement=+17)"),
        (
            Operands::Atom(AtomPoolIndex::new(18)),
            "atom(pool_index=0x00000012)",
        ),
        (
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(19),
                value: 20,
            },
            "atom_u8(pool_index=0x00000013, value=20)",
        ),
        (
            Operands::AtomU16 {
                atom: AtomPoolIndex::new(21),
                value: 22,
            },
            "atom_u16(pool_index=0x00000015, value=22)",
        ),
        (
            Operands::AtomLabelU8 {
                atom: AtomPoolIndex::new(23),
                label: -24,
                value: 25,
            },
            "atom_label_u8(pool_index=0x00000017, displacement=-24, value=25)",
        ),
        (
            Operands::AtomLabelU16 {
                atom: AtomPoolIndex::new(26),
                label: 27,
                value: 28,
            },
            "atom_label_u16(pool_index=0x0000001a, displacement=+27, value=28)",
        ),
        (
            Operands::LabelU16 {
                label: -29,
                value: 30,
            },
            "label_u16(displacement=-29, value=30)",
        ),
    ];

    for (operands, expected) in cases {
        assert_eq!(operands.to_string(), expected, "{operands:?}");
    }
}

#[test]
fn malformed_or_truncated_input_returns_an_explicit_error_with_only_an_untrusted_prefix() {
    let bytes = [
        FinalOpcode::Nop.encoded_byte(),
        FinalOpcode::PushI32.encoded_byte(),
        0xaa,
    ];
    let mut output = String::new();
    let error = render_disassembly(
        InstructionDecoder::new(&bytes),
        &mut output,
        GENEROUS_LIMITS,
    )
    .expect_err("truncated instruction");

    assert_eq!(
        error,
        DisassemblyError::Decode {
            source: DecodeError::TruncatedOperands {
                pc: BytecodePc::new(1),
                opcode: FinalOpcode::PushI32,
                expected_bytes: 4,
                remaining_bytes: 1,
            },
        }
    );
    assert_eq!(
        error.to_string(),
        "cannot disassemble bytecode: truncated push_i32 operands at PC 1: expected 4 bytes, 1 remaining"
    );
    assert_eq!(
        output,
        "pc=0x00000000 opcode=nop operands=none stack={pops=0,pushes=0}\n"
    );

    let mut invalid_output = String::new();
    assert_eq!(
        render_disassembly(
            InstructionDecoder::new(&[244]),
            &mut invalid_output,
            GENEROUS_LIMITS,
        ),
        Err(DisassemblyError::Decode {
            source: DecodeError::InvalidOpcode {
                pc: BytecodePc::ZERO,
                opcode_byte: 244,
                source: FinalOpcodeDecodeError::Unknown { byte: 244 },
            },
        })
    );
    assert!(invalid_output.is_empty());
}

#[test]
fn instruction_and_output_limits_fail_before_rendering_the_next_line() {
    let bytes = [
        FinalOpcode::Nop.encoded_byte(),
        FinalOpcode::Nop.encoded_byte(),
    ];
    let line = "pc=0x00000000 opcode=nop operands=none stack={pops=0,pushes=0}\n";

    let mut zero_instruction_output = String::new();
    assert_eq!(
        render_disassembly(
            InstructionDecoder::new(&bytes),
            &mut zero_instruction_output,
            DisassemblyLimits::new(0, usize::MAX),
        ),
        Err(DisassemblyError::InstructionLimitExceeded {
            pc: BytecodePc::ZERO,
            rendered_instructions: 0,
            max_instructions: 0,
        })
    );
    assert!(zero_instruction_output.is_empty());

    let mut short_output = String::new();
    assert_eq!(
        render_disassembly(
            InstructionDecoder::new(&bytes),
            &mut short_output,
            DisassemblyLimits::new(1, line.len() - 1),
        ),
        Err(DisassemblyError::OutputLimitExceeded {
            pc: BytecodePc::ZERO,
            rendered_bytes: 0,
            next_line_bytes: line.len(),
            max_output_bytes: line.len() - 1,
        })
    );
    assert!(short_output.is_empty());

    let mut instruction_limited = String::new();
    let instruction_error = render_disassembly(
        InstructionDecoder::new(&bytes),
        &mut instruction_limited,
        DisassemblyLimits::new(1, usize::MAX),
    )
    .expect_err("second instruction exceeds limit");
    assert_eq!(
        instruction_error,
        DisassemblyError::InstructionLimitExceeded {
            pc: BytecodePc::new(1),
            rendered_instructions: 1,
            max_instructions: 1,
        }
    );
    assert_eq!(
        instruction_error.to_string(),
        "instruction limit 1 exceeded at bytecode PC 1 after rendering 1 instructions"
    );
    assert_eq!(instruction_limited, line);

    let mut output_limited = String::new();
    let output_limits = DisassemblyLimits::new(2, line.len());
    assert_eq!(output_limits.max_instructions(), 2);
    assert_eq!(output_limits.max_output_bytes(), line.len());
    let output_error = render_disassembly(
        InstructionDecoder::new(&bytes),
        &mut output_limited,
        output_limits,
    )
    .expect_err("second line exceeds output limit");
    assert_eq!(
        output_error,
        DisassemblyError::OutputLimitExceeded {
            pc: BytecodePc::new(1),
            rendered_bytes: line.len(),
            next_line_bytes: line.len(),
            max_output_bytes: line.len(),
        }
    );
    assert_eq!(
        output_error.to_string(),
        format!(
            "output limit {} bytes exceeded at bytecode PC 1: {} bytes rendered and the next line needs {} bytes",
            line.len(),
            line.len(),
            line.len(),
        )
    );
    assert_eq!(output_limited, line);

    let mut exact_output = String::new();
    let exact_summary = render_disassembly(
        InstructionDecoder::new(&bytes[..1]),
        &mut exact_output,
        DisassemblyLimits::new(1, line.len()),
    )
    .expect("exact limits are inclusive");
    assert_eq!(exact_output, line);
    assert_eq!(exact_summary.instruction_count(), 1);
    assert_eq!(exact_summary.output_bytes(), line.len());
}

#[test]
fn formatting_failure_and_empty_stream_are_structured_errors() {
    let mut failing = FailingSink;
    assert!(matches!(
        render_disassembly(
            InstructionDecoder::new(&[FinalOpcode::Nop.encoded_byte()]),
            &mut failing,
            GENEROUS_LIMITS,
        ),
        Err(DisassemblyError::Formatting {
            pc: BytecodePc::ZERO,
            ..
        })
    ));

    let mut output = String::new();
    let empty_error =
        render_disassembly(InstructionDecoder::new(&[]), &mut output, GENEROUS_LIMITS)
            .expect_err("empty stream");
    assert_eq!(
        empty_error,
        DisassemblyError::EmptyInstructionStream {
            pc: BytecodePc::ZERO
        }
    );
    assert_eq!(
        empty_error.to_string(),
        "instruction stream at bytecode PC 0 is empty"
    );
    assert!(output.is_empty());
}

struct FailingSink;

impl fmt::Write for FailingSink {
    fn write_str(&mut self, _value: &str) -> fmt::Result {
        Err(fmt::Error)
    }
}
