//! Bounded, deterministic rendering of checked instruction streams.

use std::{
    error::Error,
    fmt::{self, Write as _},
};

use crate::{
    BytecodePc, DecodeError, FinalOpcode, InstructionDecoder, Operands, StackEffect,
    StackEffectError,
};

/// Resource limits applied while rendering a disassembly.
///
/// Callers must choose both limits explicitly. A limit is inclusive: rendering
/// exactly the configured number of instructions or output bytes succeeds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DisassemblyLimits {
    max_instructions: usize,
    max_output_bytes: usize,
}

impl DisassemblyLimits {
    /// Creates instruction-count and UTF-8 output-byte limits.
    #[must_use]
    pub const fn new(max_instructions: usize, max_output_bytes: usize) -> Self {
        Self {
            max_instructions,
            max_output_bytes,
        }
    }

    /// Returns the maximum number of instructions that may be rendered.
    #[must_use]
    pub const fn max_instructions(self) -> usize {
        self.max_instructions
    }

    /// Returns the maximum number of UTF-8 bytes that may be written.
    #[must_use]
    pub const fn max_output_bytes(self) -> usize {
        self.max_output_bytes
    }
}

/// Counts returned only after an instruction stream was rendered completely.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DisassemblySummary {
    instruction_count: usize,
    output_bytes: usize,
}

impl DisassemblySummary {
    /// Returns the number of rendered instructions.
    #[must_use]
    pub const fn instruction_count(self) -> usize {
        self.instruction_count
    }

    /// Returns the number of UTF-8 bytes successfully written.
    #[must_use]
    pub const fn output_bytes(self) -> usize {
        self.output_bytes
    }
}

/// Renders a checked instruction decoder into a caller-provided text sink.
///
/// Each stable line contains an absolute bytecode PC, the exact opcode
/// mnemonic, a typed operand rendering, and the resolved opcode-table stack
/// effect, including dynamic argument counts. Relative displacements always
/// include an explicit sign.
///
/// This is an observational decoder, not the bytecode verifier. It does not
/// validate control-flow targets, indices, abstract stack shapes, or function
/// metadata, and it never executes bytecode. Source annotations are
/// intentionally absent; a future extension will accept typed PC-to-source
/// metadata rather than an unstructured callback.
///
/// On failure, `output` may contain an incomplete, untrusted prefix. Callers
/// must treat only an [`Ok`] [`DisassemblySummary`] as complete success. A
/// caller that needs atomic visibility can render into its own temporary
/// [`String`] and publish that string only after this function succeeds.
///
/// # Errors
///
/// Returns [`DisassemblyError::EmptyInstructionStream`] when the decoder has no
/// instruction, [`DisassemblyError::Decode`] for malformed bytes,
/// [`DisassemblyError::StackEffect`] for an opcode-schema inconsistency,
/// either limit variant before the next line is written, or
/// [`DisassemblyError::Formatting`] when the sink rejects output.
pub fn render_disassembly<W>(
    mut decoder: InstructionDecoder<'_>,
    output: &mut W,
    limits: DisassemblyLimits,
) -> Result<DisassemblySummary, DisassemblyError>
where
    W: fmt::Write + ?Sized,
{
    let initial_pc = decoder.next_pc();
    let mut instruction_count = 0_usize;
    let mut output_bytes = 0_usize;

    loop {
        let next_pc = decoder.next_pc();
        let Some(next_item) = decoder.next() else {
            if instruction_count == 0 {
                return Err(DisassemblyError::EmptyInstructionStream { pc: initial_pc });
            }
            return Ok(DisassemblySummary {
                instruction_count,
                output_bytes,
            });
        };

        if instruction_count >= limits.max_instructions {
            return Err(DisassemblyError::InstructionLimitExceeded {
                pc: next_pc,
                rendered_instructions: instruction_count,
                max_instructions: limits.max_instructions,
            });
        }

        let decoded_instruction =
            next_item.map_err(|source| DisassemblyError::Decode { source })?;
        let instruction = decoded_instruction.instruction();
        let opcode = instruction.opcode();
        let stack_effect =
            instruction
                .stack_effect()
                .map_err(|source| DisassemblyError::StackEffect {
                    pc: decoded_instruction.pc(),
                    opcode,
                    source,
                })?;
        let line = InstructionLine {
            pc: decoded_instruction.pc(),
            opcode,
            operands: instruction.operands(),
            stack_effect,
        };
        let next_line_bytes =
            line.encoded_len()
                .map_err(|source| DisassemblyError::Formatting {
                    pc: decoded_instruction.pc(),
                    source,
                })?;
        let remaining_bytes = limits.max_output_bytes.saturating_sub(output_bytes);
        if next_line_bytes > remaining_bytes {
            return Err(DisassemblyError::OutputLimitExceeded {
                pc: decoded_instruction.pc(),
                rendered_bytes: output_bytes,
                next_line_bytes,
                max_output_bytes: limits.max_output_bytes,
            });
        }

        write!(output, "{line}").map_err(|source| DisassemblyError::Formatting {
            pc: decoded_instruction.pc(),
            source,
        })?;
        instruction_count += 1;
        output_bytes += next_line_bytes;
    }
}

impl fmt::Display for Operands {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::None => formatter.write_str("none"),
            Self::NoneInt => formatter.write_str("none_int"),
            Self::NoneLoc => formatter.write_str("none_loc"),
            Self::NoneArg => formatter.write_str("none_arg"),
            Self::NoneVarRef => formatter.write_str("none_var_ref"),
            Self::U8(value) => write!(formatter, "u8({value})"),
            Self::I8(value) => write!(formatter, "i8({value})"),
            Self::Loc8(index) => write!(formatter, "loc8(index={index})"),
            Self::Const8(index) => write!(formatter, "const8(index={index})"),
            Self::Label8(displacement) => {
                write!(formatter, "label8(displacement={displacement:+})")
            }
            Self::U16(value) => write!(formatter, "u16({value})"),
            Self::I16(value) => write!(formatter, "i16({value})"),
            Self::Label16(displacement) => {
                write!(formatter, "label16(displacement={displacement:+})")
            }
            Self::NPop { argument_count } => {
                write!(formatter, "npop(argument_count={argument_count})")
            }
            Self::NPopX => formatter.write_str("npopx"),
            Self::NPopU16 {
                argument_count,
                scope_index,
            } => write!(
                formatter,
                "npop_u16(argument_count={argument_count}, scope_index={scope_index})"
            ),
            Self::Loc(index) => write!(formatter, "loc(index={index})"),
            Self::Arg(index) => write!(formatter, "arg(index={index})"),
            Self::VarRef(index) => write!(formatter, "var_ref(index={index})"),
            Self::U32(value) => write!(formatter, "u32({value})"),
            Self::I32(value) => write!(formatter, "i32({value})"),
            Self::Const(index) => write!(formatter, "const(index={index})"),
            Self::Label(displacement) => {
                write!(formatter, "label(displacement={displacement:+})")
            }
            Self::Atom(index) => write!(formatter, "atom(pool_index=0x{index:08x})"),
            Self::AtomU8 { atom, value } => {
                write!(formatter, "atom_u8(pool_index=0x{atom:08x}, value={value})")
            }
            Self::AtomU16 { atom, value } => {
                write!(
                    formatter,
                    "atom_u16(pool_index=0x{atom:08x}, value={value})"
                )
            }
            Self::AtomLabelU8 { atom, label, value } => write!(
                formatter,
                "atom_label_u8(pool_index=0x{atom:08x}, displacement={label:+}, value={value})"
            ),
            Self::AtomLabelU16 { atom, label, value } => write!(
                formatter,
                "atom_label_u16(pool_index=0x{atom:08x}, displacement={label:+}, value={value})"
            ),
            Self::LabelU16 { label, value } => write!(
                formatter,
                "label_u16(displacement={label:+}, value={value})"
            ),
        }
    }
}

struct InstructionLine {
    pc: BytecodePc,
    opcode: FinalOpcode,
    operands: Operands,
    stack_effect: StackEffect,
}

impl InstructionLine {
    fn encoded_len(&self) -> Result<usize, fmt::Error> {
        let mut counter = ByteCounter::default();
        write!(counter, "{self}")?;
        Ok(counter.bytes)
    }
}

impl fmt::Display for InstructionLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "pc=0x{:08x} opcode={} operands={} stack={{pops={},pushes={}}}",
            self.pc.get(),
            self.opcode,
            self.operands,
            self.stack_effect.pops(),
            self.stack_effect.pushes(),
        )
    }
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl fmt::Write for ByteCounter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.bytes = self.bytes.checked_add(value.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

/// Failure to render a complete instruction stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisassemblyError {
    /// The decoder yielded no instruction.
    EmptyInstructionStream {
        /// Position at which the empty stream or fragment begins.
        pc: BytecodePc,
    },
    /// Checked instruction decoding failed.
    Decode {
        /// Exact bytecode decoding failure.
        source: DecodeError,
    },
    /// Resolved stack-effect calculation found an opcode-schema inconsistency.
    StackEffect {
        /// Instruction position.
        pc: BytecodePc,
        /// Instruction whose stack effect failed.
        opcode: FinalOpcode,
        /// Exact stack-effect failure.
        source: StackEffectError,
    },
    /// Rendering another instruction would exceed the instruction limit.
    InstructionLimitExceeded {
        /// Position of the instruction that was not rendered.
        pc: BytecodePc,
        /// Number of instructions already rendered.
        rendered_instructions: usize,
        /// Configured maximum instruction count.
        max_instructions: usize,
    },
    /// Rendering the next complete line would exceed the output-byte limit.
    OutputLimitExceeded {
        /// Position of the instruction that was not rendered.
        pc: BytecodePc,
        /// Number of UTF-8 bytes already written successfully.
        rendered_bytes: usize,
        /// Exact number of UTF-8 bytes needed by the next line.
        next_line_bytes: usize,
        /// Configured maximum UTF-8 output byte count.
        max_output_bytes: usize,
    },
    /// Formatting or writing an instruction line failed.
    Formatting {
        /// Position of the line that failed.
        pc: BytecodePc,
        /// Formatting failure returned by the sink.
        source: fmt::Error,
    },
}

impl fmt::Display for DisassemblyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInstructionStream { pc } => {
                write!(formatter, "instruction stream at bytecode PC {pc} is empty")
            }
            Self::Decode { source } => write!(formatter, "cannot disassemble bytecode: {source}"),
            Self::StackEffect { pc, opcode, source } => write!(
                formatter,
                "cannot compute stack effect for {opcode} at bytecode PC {pc}: {source}"
            ),
            Self::InstructionLimitExceeded {
                pc,
                rendered_instructions,
                max_instructions,
            } => write!(
                formatter,
                "instruction limit {max_instructions} exceeded at bytecode PC {pc} after rendering {rendered_instructions} instructions"
            ),
            Self::OutputLimitExceeded {
                pc,
                rendered_bytes,
                next_line_bytes,
                max_output_bytes,
            } => write!(
                formatter,
                "output limit {max_output_bytes} bytes exceeded at bytecode PC {pc}: {rendered_bytes} bytes rendered and the next line needs {next_line_bytes} bytes"
            ),
            Self::Formatting { pc, source } => {
                write!(
                    formatter,
                    "cannot write disassembly at bytecode PC {pc}: {source}"
                )
            }
        }
    }
}

impl Error for DisassemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decode { source } => Some(source),
            Self::StackEffect { source, .. } => Some(source),
            Self::Formatting { source, .. } => Some(source),
            Self::EmptyInstructionStream { .. }
            | Self::InstructionLimitExceeded { .. }
            | Self::OutputLimitExceeded { .. } => None,
        }
    }
}
