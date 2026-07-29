//! Owned instruction encoding and checked decoding.
//!
//! This port's internal instruction stream uses deterministic little-endian
//! fixed-width operands on every host. The C engine's private in-memory
//! bytecode uses native-endian operands, while its object writer applies a
//! separate versioned serialization format. Consequently, these bytes are not
//! claimed to be binary-compatible with upstream private bytecode or bytecode
//! objects.

use std::{error::Error, fmt, iter::FusedIterator};

use crate::{FinalOpcode, FinalOpcodeDecodeError, OperandFormat, StackEffect, StackEffectError};

/// Maximum encoded operand width in the pinned opcode schema.
pub const MAX_ENCODED_OPERAND_BYTES: usize = 10;

/// A byte offset into a bytecode stream.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct BytecodePc(u32);

impl BytecodePc {
    /// The first bytecode position.
    pub const ZERO: Self = Self(0);

    /// Creates a program counter from its byte offset.
    #[must_use]
    pub const fn new(offset: u32) -> Self {
        Self(offset)
    }

    /// Returns the byte offset.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Adds an encoded byte count, returning `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, bytes: u32) -> Option<Self> {
        match self.0.checked_add(bytes) {
            Some(offset) => Some(Self(offset)),
            None => None,
        }
    }
}

impl fmt::Display for BytecodePc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Lossless typed operands for every [`OperandFormat`].
///
/// Variants without fields still matter: their operand is encoded in the
/// opcode itself and their distinct type prevents confusing local, argument,
/// closure, integer, and dynamic-pop short forms.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Operands {
    /// [`OperandFormat::None`].
    None,
    /// [`OperandFormat::NoneInt`].
    NoneInt,
    /// [`OperandFormat::NoneLoc`].
    NoneLoc,
    /// [`OperandFormat::NoneArg`].
    NoneArg,
    /// [`OperandFormat::NoneVarRef`].
    NoneVarRef,
    /// [`OperandFormat::U8`].
    U8(u8),
    /// [`OperandFormat::I8`].
    I8(i8),
    /// [`OperandFormat::Loc8`].
    Loc8(u8),
    /// [`OperandFormat::Const8`].
    Const8(u8),
    /// [`OperandFormat::Label8`].
    Label8(i8),
    /// [`OperandFormat::U16`].
    U16(u16),
    /// [`OperandFormat::I16`].
    I16(i16),
    /// [`OperandFormat::Label16`].
    Label16(i16),
    /// [`OperandFormat::NPop`].
    NPop {
        /// Number of arguments additionally removed from the stack.
        argument_count: u16,
    },
    /// [`OperandFormat::NPopX`].
    NPopX,
    /// [`OperandFormat::NPopU16`].
    NPopU16 {
        /// Number of arguments additionally removed from the stack.
        argument_count: u16,
        /// Scope index used by the pinned `eval` instruction.
        scope_index: u16,
    },
    /// [`OperandFormat::Loc`].
    Loc(u16),
    /// [`OperandFormat::Arg`].
    Arg(u16),
    /// [`OperandFormat::VarRef`].
    VarRef(u16),
    /// [`OperandFormat::U32`].
    U32(u32),
    /// [`OperandFormat::I32`].
    I32(i32),
    /// [`OperandFormat::Const`].
    Const(u32),
    /// [`OperandFormat::Label`].
    ///
    /// Final bytecode stores a signed relative displacement. Compiler label
    /// identifiers belong to a separate typed IR and cannot be encoded here.
    Label(i32),
    /// [`OperandFormat::Atom`].
    Atom(u32),
    /// [`OperandFormat::AtomU8`].
    AtomU8 {
        /// Raw atom identifier.
        atom: u32,
        /// Trailing instruction-specific byte.
        value: u8,
    },
    /// [`OperandFormat::AtomU16`].
    AtomU16 {
        /// Raw atom identifier.
        atom: u32,
        /// Trailing instruction-specific word.
        value: u16,
    },
    /// [`OperandFormat::AtomLabelU8`].
    AtomLabelU8 {
        /// Raw atom identifier.
        atom: u32,
        /// Signed relative label displacement.
        label: i32,
        /// Trailing instruction-specific byte.
        value: u8,
    },
    /// [`OperandFormat::AtomLabelU16`].
    AtomLabelU16 {
        /// Raw atom identifier.
        atom: u32,
        /// Signed relative label displacement.
        label: i32,
        /// Trailing instruction-specific word.
        value: u16,
    },
    /// [`OperandFormat::LabelU16`].
    LabelU16 {
        /// Signed relative label displacement.
        label: i32,
        /// Trailing instruction-specific word.
        value: u16,
    },
}

impl Operands {
    /// Returns this value's exact operand format.
    #[must_use]
    pub const fn format(self) -> OperandFormat {
        match self {
            Self::None => OperandFormat::None,
            Self::NoneInt => OperandFormat::NoneInt,
            Self::NoneLoc => OperandFormat::NoneLoc,
            Self::NoneArg => OperandFormat::NoneArg,
            Self::NoneVarRef => OperandFormat::NoneVarRef,
            Self::U8(_) => OperandFormat::U8,
            Self::I8(_) => OperandFormat::I8,
            Self::Loc8(_) => OperandFormat::Loc8,
            Self::Const8(_) => OperandFormat::Const8,
            Self::Label8(_) => OperandFormat::Label8,
            Self::U16(_) => OperandFormat::U16,
            Self::I16(_) => OperandFormat::I16,
            Self::Label16(_) => OperandFormat::Label16,
            Self::NPop { .. } => OperandFormat::NPop,
            Self::NPopX => OperandFormat::NPopX,
            Self::NPopU16 { .. } => OperandFormat::NPopU16,
            Self::Loc(_) => OperandFormat::Loc,
            Self::Arg(_) => OperandFormat::Arg,
            Self::VarRef(_) => OperandFormat::VarRef,
            Self::U32(_) => OperandFormat::U32,
            Self::I32(_) => OperandFormat::I32,
            Self::Const(_) => OperandFormat::Const,
            Self::Label(_) => OperandFormat::Label,
            Self::Atom(_) => OperandFormat::Atom,
            Self::AtomU8 { .. } => OperandFormat::AtomU8,
            Self::AtomU16 { .. } => OperandFormat::AtomU16,
            Self::AtomLabelU8 { .. } => OperandFormat::AtomLabelU8,
            Self::AtomLabelU16 { .. } => OperandFormat::AtomLabelU16,
            Self::LabelU16 { .. } => OperandFormat::LabelU16,
        }
    }

    /// Returns a dynamic argument count when one is stored in the operands.
    ///
    /// `NPopX` returns `None` because its count is encoded by the opcode.
    #[must_use]
    pub const fn dynamic_argument_count(self) -> Option<u16> {
        match self {
            Self::NPop { argument_count } | Self::NPopU16 { argument_count, .. } => {
                Some(argument_count)
            }
            _ => None,
        }
    }

    /// Encodes these operands into a fixed-capacity owned buffer.
    ///
    /// # Errors
    ///
    /// Returns an internal schema error if a future format exceeds
    /// [`MAX_ENCODED_OPERAND_BYTES`] or its declared width.
    pub fn encode(self) -> Result<EncodedOperands, OperandEncodeError> {
        let format = self.format();
        let mut output = EncodedOperands::new();

        match self {
            Self::None
            | Self::NoneInt
            | Self::NoneLoc
            | Self::NoneArg
            | Self::NoneVarRef
            | Self::NPopX => {}
            Self::U8(value) | Self::Loc8(value) | Self::Const8(value) => {
                output.push_bytes(format, [value])?;
            }
            Self::I8(value) | Self::Label8(value) => {
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::U16(value)
            | Self::Loc(value)
            | Self::Arg(value)
            | Self::VarRef(value)
            | Self::NPop {
                argument_count: value,
            } => {
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::I16(value) | Self::Label16(value) => {
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::NPopU16 {
                argument_count,
                scope_index,
            } => {
                output.push_bytes(format, argument_count.to_le_bytes())?;
                output.push_bytes(format, scope_index.to_le_bytes())?;
            }
            Self::U32(value) | Self::Const(value) | Self::Atom(value) => {
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::I32(value) | Self::Label(value) => {
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::AtomU8 { atom, value } => {
                output.push_bytes(format, atom.to_le_bytes())?;
                output.push_bytes(format, [value])?;
            }
            Self::AtomU16 { atom, value } => {
                output.push_bytes(format, atom.to_le_bytes())?;
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::AtomLabelU8 { atom, label, value } => {
                output.push_bytes(format, atom.to_le_bytes())?;
                output.push_bytes(format, label.to_le_bytes())?;
                output.push_bytes(format, [value])?;
            }
            Self::AtomLabelU16 { atom, label, value } => {
                output.push_bytes(format, atom.to_le_bytes())?;
                output.push_bytes(format, label.to_le_bytes())?;
                output.push_bytes(format, value.to_le_bytes())?;
            }
            Self::LabelU16 { label, value } => {
                output.push_bytes(format, label.to_le_bytes())?;
                output.push_bytes(format, value.to_le_bytes())?;
            }
        }

        let expected = format.operand_width();
        if output.len != expected {
            return Err(OperandEncodeError::SchemaWidthMismatch {
                format,
                expected_bytes: expected,
                actual_bytes: output.len,
            });
        }
        Ok(output)
    }

    /// Decodes exactly one operand payload using deterministic little endian.
    ///
    /// The input must contain exactly the selected format's operand bytes; both
    /// truncation and trailing data are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`OperandDecodeError::LengthMismatch`] when `bytes` does not
    /// have the format's exact width.
    #[allow(clippy::too_many_lines)]
    pub fn decode(format: OperandFormat, bytes: &[u8]) -> Result<Self, OperandDecodeError> {
        let expected = format.operand_width();
        if bytes.len() != usize::from(expected) {
            return Err(OperandDecodeError::LengthMismatch {
                format,
                expected_bytes: expected,
                actual_bytes: bytes.len(),
            });
        }

        let malformed = || OperandDecodeError::LengthMismatch {
            format,
            expected_bytes: expected,
            actual_bytes: bytes.len(),
        };
        let u8_at = |offset| read_u8(bytes, offset).ok_or_else(malformed);
        let u16_at = |offset| read_u16(bytes, offset).ok_or_else(malformed);
        let i16_at = |offset| read_i16(bytes, offset).ok_or_else(malformed);
        let u32_at = |offset| read_u32(bytes, offset).ok_or_else(malformed);

        match format {
            OperandFormat::None => Ok(Self::None),
            OperandFormat::NoneInt => Ok(Self::NoneInt),
            OperandFormat::NoneLoc => Ok(Self::NoneLoc),
            OperandFormat::NoneArg => Ok(Self::NoneArg),
            OperandFormat::NoneVarRef => Ok(Self::NoneVarRef),
            OperandFormat::U8 => Ok(Self::U8(u8_at(0)?)),
            OperandFormat::I8 => Ok(Self::I8(i8::from_le_bytes([u8_at(0)?]))),
            OperandFormat::Loc8 => Ok(Self::Loc8(u8_at(0)?)),
            OperandFormat::Const8 => Ok(Self::Const8(u8_at(0)?)),
            OperandFormat::Label8 => Ok(Self::Label8(i8::from_le_bytes([u8_at(0)?]))),
            OperandFormat::U16 => Ok(Self::U16(u16_at(0)?)),
            OperandFormat::I16 => Ok(Self::I16(i16_at(0)?)),
            OperandFormat::Label16 => Ok(Self::Label16(i16_at(0)?)),
            OperandFormat::NPop => Ok(Self::NPop {
                argument_count: u16_at(0)?,
            }),
            OperandFormat::NPopX => Ok(Self::NPopX),
            OperandFormat::NPopU16 => Ok(Self::NPopU16 {
                argument_count: u16_at(0)?,
                scope_index: u16_at(2)?,
            }),
            OperandFormat::Loc => Ok(Self::Loc(u16_at(0)?)),
            OperandFormat::Arg => Ok(Self::Arg(u16_at(0)?)),
            OperandFormat::VarRef => Ok(Self::VarRef(u16_at(0)?)),
            OperandFormat::U32 => Ok(Self::U32(u32_at(0)?)),
            OperandFormat::I32 => Ok(Self::I32(read_i32(bytes, 0).ok_or_else(malformed)?)),
            OperandFormat::Const => Ok(Self::Const(u32_at(0)?)),
            OperandFormat::Label => Ok(Self::Label(read_i32(bytes, 0).ok_or_else(malformed)?)),
            OperandFormat::Atom => Ok(Self::Atom(u32_at(0)?)),
            OperandFormat::AtomU8 => Ok(Self::AtomU8 {
                atom: u32_at(0)?,
                value: u8_at(4)?,
            }),
            OperandFormat::AtomU16 => Ok(Self::AtomU16 {
                atom: u32_at(0)?,
                value: u16_at(4)?,
            }),
            OperandFormat::AtomLabelU8 => Ok(Self::AtomLabelU8 {
                atom: u32_at(0)?,
                label: read_i32(bytes, 4).ok_or_else(malformed)?,
                value: u8_at(8)?,
            }),
            OperandFormat::AtomLabelU16 => Ok(Self::AtomLabelU16 {
                atom: u32_at(0)?,
                label: read_i32(bytes, 4).ok_or_else(malformed)?,
                value: u16_at(8)?,
            }),
            OperandFormat::LabelU16 => Ok(Self::LabelU16 {
                label: read_i32(bytes, 0).ok_or_else(malformed)?,
                value: u16_at(4)?,
            }),
        }
    }
}

/// Fixed-capacity encoded operand bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodedOperands {
    bytes: [u8; MAX_ENCODED_OPERAND_BYTES],
    len: u8,
}

impl EncodedOperands {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_ENCODED_OPERAND_BYTES],
            len: 0,
        }
    }

    /// Returns the encoded bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or_default()
    }

    /// Returns the encoded width.
    #[must_use]
    pub const fn len(&self) -> u8 {
        self.len
    }

    /// Returns whether this payload has no encoded bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push_bytes<const N: usize>(
        &mut self,
        format: OperandFormat,
        bytes: [u8; N],
    ) -> Result<(), OperandEncodeError> {
        for byte in bytes {
            let index = usize::from(self.len);
            let Some(slot) = self.bytes.get_mut(index) else {
                return Err(OperandEncodeError::BufferCapacityExceeded {
                    format,
                    capacity_bytes: MAX_ENCODED_OPERAND_BYTES,
                });
            };
            *slot = byte;
            self.len =
                self.len
                    .checked_add(1)
                    .ok_or(OperandEncodeError::BufferCapacityExceeded {
                        format,
                        capacity_bytes: MAX_ENCODED_OPERAND_BYTES,
                    })?;
        }
        Ok(())
    }
}

/// A validated, owned final instruction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Instruction {
    opcode: FinalOpcode,
    operands: Operands,
}

impl Instruction {
    /// Creates an instruction whose operands match its opcode metadata.
    ///
    /// # Errors
    ///
    /// Rejects the reserved invalid opcode and any operand-format mismatch.
    pub fn new(opcode: FinalOpcode, operands: Operands) -> Result<Self, InstructionError> {
        if opcode == FinalOpcode::Invalid {
            return Err(InstructionError::ReservedOpcode);
        }
        let expected = opcode.metadata().operand_format();
        let actual = operands.format();
        if expected != actual {
            return Err(InstructionError::OperandFormatMismatch {
                opcode,
                expected,
                actual,
            });
        }
        Ok(Self { opcode, operands })
    }

    /// Returns the opcode.
    #[must_use]
    pub const fn opcode(self) -> FinalOpcode {
        self.opcode
    }

    /// Returns the operands.
    #[must_use]
    pub const fn operands(self) -> Operands {
        self.operands
    }

    /// Returns the instruction's encoded width.
    #[must_use]
    pub fn encoded_size(self) -> u8 {
        self.opcode.metadata().instruction_size()
    }

    /// Returns an explicitly encoded dynamic argument count.
    #[must_use]
    pub const fn dynamic_argument_count(self) -> Option<u16> {
        self.operands.dynamic_argument_count()
    }

    /// Resolves the complete stack effect, including dynamic arguments.
    ///
    /// # Errors
    ///
    /// Propagates a schema inconsistency from [`FinalOpcode::stack_effect`].
    pub fn stack_effect(self) -> Result<StackEffect, StackEffectError> {
        self.opcode
            .stack_effect(self.operands.dynamic_argument_count())
    }
}

/// A decoded instruction and its bytecode positions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DecodedInstruction {
    pc: BytecodePc,
    instruction: Instruction,
    next_pc: BytecodePc,
}

impl DecodedInstruction {
    /// Returns the instruction's starting position.
    #[must_use]
    pub const fn pc(self) -> BytecodePc {
        self.pc
    }

    /// Returns the decoded owned instruction.
    #[must_use]
    pub const fn instruction(self) -> Instruction {
        self.instruction
    }

    /// Returns the first position after this instruction.
    #[must_use]
    pub const fn next_pc(self) -> BytecodePc {
        self.next_pc
    }
}

/// Decodes one final instruction at `pc`.
///
/// # Errors
///
/// Rejects unrepresentable or out-of-bounds positions, missing/truncated
/// bytes, reserved or unknown opcodes, and a next position above `u32::MAX`.
pub fn decode_instruction(
    bytecode: &[u8],
    pc: BytecodePc,
) -> Result<DecodedInstruction, DecodeError> {
    let index = usize::try_from(pc.get()).map_err(|_| DecodeError::PcNotRepresentable { pc })?;
    if index > bytecode.len() {
        return Err(DecodeError::PcOutOfBounds {
            pc,
            bytecode_len: bytecode.len(),
        });
    }
    let remaining = bytecode.len() - index;
    let Some(opcode_byte) = bytecode.get(index).copied() else {
        return Err(DecodeError::MissingOpcode {
            pc,
            expected_bytes: 1,
            remaining_bytes: remaining,
        });
    };
    let opcode = FinalOpcode::decode(opcode_byte).map_err(|source| DecodeError::InvalidOpcode {
        pc,
        opcode_byte,
        source,
    })?;
    let metadata = opcode.metadata();
    let expected_operands = metadata.operand_format().operand_width();
    let operand_start = index
        .checked_add(1)
        .ok_or(DecodeError::PcNotRepresentable { pc })?;
    let remaining_operands = bytecode.len().saturating_sub(operand_start);
    if remaining_operands < usize::from(expected_operands) {
        return Err(DecodeError::TruncatedOperands {
            pc,
            opcode,
            expected_bytes: expected_operands,
            remaining_bytes: remaining_operands,
        });
    }
    let operand_end = operand_start
        .checked_add(usize::from(expected_operands))
        .ok_or(DecodeError::PcNotRepresentable { pc })?;
    let Some(operand_bytes) = bytecode.get(operand_start..operand_end) else {
        return Err(DecodeError::TruncatedOperands {
            pc,
            opcode,
            expected_bytes: expected_operands,
            remaining_bytes: remaining_operands,
        });
    };
    let operands = Operands::decode(metadata.operand_format(), operand_bytes)
        .map_err(|source| DecodeError::OperandDecoding { pc, opcode, source })?;
    let instruction = Instruction { opcode, operands };
    let instruction_size = u32::from(metadata.instruction_size());
    let next_pc = pc
        .checked_add(instruction_size)
        .ok_or(DecodeError::NextPcOverflow {
            pc,
            instruction_size: metadata.instruction_size(),
        })?;

    Ok(DecodedInstruction {
        pc,
        instruction,
        next_pc,
    })
}

/// A checked iterator over a final instruction stream.
#[derive(Clone, Debug)]
pub struct InstructionDecoder<'a> {
    bytecode: &'a [u8],
    next_pc: BytecodePc,
    finished: bool,
}

impl<'a> InstructionDecoder<'a> {
    /// Creates a decoder starting at bytecode position zero.
    #[must_use]
    pub const fn new(bytecode: &'a [u8]) -> Self {
        Self {
            bytecode,
            next_pc: BytecodePc::ZERO,
            finished: false,
        }
    }

    /// Creates a decoder starting at `pc`.
    #[must_use]
    pub const fn with_pc(bytecode: &'a [u8], pc: BytecodePc) -> Self {
        Self {
            bytecode,
            next_pc: pc,
            finished: false,
        }
    }

    /// Returns the next position that will be decoded.
    #[must_use]
    pub const fn next_pc(&self) -> BytecodePc {
        self.next_pc
    }
}

impl Iterator for InstructionDecoder<'_> {
    type Item = Result<DecodedInstruction, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let Ok(index) = usize::try_from(self.next_pc.get()) else {
            self.finished = true;
            return Some(Err(DecodeError::PcNotRepresentable { pc: self.next_pc }));
        };
        if index == self.bytecode.len() {
            self.finished = true;
            return None;
        }

        match decode_instruction(self.bytecode, self.next_pc) {
            Ok(decoded) => {
                self.next_pc = decoded.next_pc();
                Some(Ok(decoded))
            }
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

impl FusedIterator for InstructionDecoder<'_> {}

/// A bounded encoder for final instructions.
#[derive(Clone, Debug)]
pub struct BytecodeBuilder {
    bytes: Vec<u8>,
    next_pc: BytecodePc,
    encoded_len: u32,
    byte_limit: u32,
}

impl BytecodeBuilder {
    /// Creates an empty stream at position zero with the `u32` bytecode limit.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_origin_and_byte_limit(BytecodePc::ZERO, u32::MAX)
    }

    /// Creates an empty stream at position zero with a resource limit.
    #[must_use]
    pub const fn with_byte_limit(byte_limit: u32) -> Self {
        Self::with_origin_and_byte_limit(BytecodePc::ZERO, byte_limit)
    }

    /// Creates a fragment encoder whose first instruction starts at `origin`.
    ///
    /// The origin enables relocation-aware builders and makes `u32` position
    /// overflow testable without allocating several gigabytes.
    #[must_use]
    pub const fn with_origin(origin: BytecodePc) -> Self {
        Self::with_origin_and_byte_limit(origin, u32::MAX)
    }

    /// Creates a fragment encoder with an origin and encoded-byte limit.
    #[must_use]
    pub const fn with_origin_and_byte_limit(origin: BytecodePc, byte_limit: u32) -> Self {
        Self {
            bytes: Vec::new(),
            next_pc: origin,
            encoded_len: 0,
            byte_limit,
        }
    }

    /// Validates and appends an opcode and its operands.
    ///
    /// The builder is unchanged if validation, limits, or allocation fail.
    ///
    /// # Errors
    ///
    /// Rejects invalid instructions, format mismatches, position or encoded
    /// length overflow, the configured byte limit, and allocation failure.
    pub fn push(
        &mut self,
        opcode: FinalOpcode,
        operands: Operands,
    ) -> Result<BytecodePc, EncodeError> {
        let pc = self.next_pc;
        let instruction = Instruction::new(opcode, operands)
            .map_err(|source| EncodeError::InvalidInstruction { pc, source })?;
        self.push_instruction(instruction)
    }

    /// Appends an already validated instruction.
    ///
    /// The builder is unchanged if limits or allocation fail.
    ///
    /// # Errors
    ///
    /// Rejects position or encoded-length overflow, schema inconsistency, the
    /// configured byte limit, and allocation failure.
    pub fn push_instruction(
        &mut self,
        instruction: Instruction,
    ) -> Result<BytecodePc, EncodeError> {
        let pc = self.next_pc;
        let opcode = instruction.opcode();
        let encoded_operands = instruction
            .operands()
            .encode()
            .map_err(|source| EncodeError::OperandEncoding { pc, opcode, source })?;
        let Some(actual_size) = encoded_operands.len().checked_add(1) else {
            return Err(EncodeError::SchemaSizeMismatch {
                pc,
                opcode,
                declared_size: opcode.metadata().instruction_size(),
                actual_size: u8::MAX,
            });
        };
        let declared_size = opcode.metadata().instruction_size();
        if actual_size != declared_size {
            return Err(EncodeError::SchemaSizeMismatch {
                pc,
                opcode,
                declared_size,
                actual_size,
            });
        }
        let next_pc = pc
            .checked_add(u32::from(actual_size))
            .ok_or(EncodeError::PcOverflow {
                pc,
                instruction_size: actual_size,
            })?;
        let next_encoded_len = self.encoded_len.checked_add(u32::from(actual_size)).ok_or(
            EncodeError::EncodedLengthOverflow {
                encoded_bytes: self.encoded_len,
                instruction_size: actual_size,
            },
        )?;
        if next_encoded_len > self.byte_limit {
            return Err(EncodeError::ByteLimitExceeded {
                pc,
                instruction_size: actual_size,
                encoded_bytes: self.encoded_len,
                byte_limit: self.byte_limit,
            });
        }

        self.bytes
            .try_reserve(usize::from(actual_size))
            .map_err(|_| EncodeError::AllocationFailed {
                pc,
                additional_bytes: actual_size,
            })?;
        self.bytes.push(opcode.encoded_byte());
        self.bytes.extend_from_slice(encoded_operands.as_bytes());
        self.next_pc = next_pc;
        self.encoded_len = next_encoded_len;
        Ok(pc)
    }

    /// Returns the encoded bytes accumulated so far.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the next instruction position.
    #[must_use]
    pub const fn next_pc(&self) -> BytecodePc {
        self.next_pc
    }

    /// Returns the encoded byte count, excluding the fragment origin.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.encoded_len
    }

    /// Returns whether no instructions have been encoded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.encoded_len == 0
    }

    /// Returns the configured encoded-byte limit.
    #[must_use]
    pub const fn byte_limit(&self) -> u32 {
        self.byte_limit
    }

    /// Consumes the builder and returns its owned bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Default for BytecodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Failure to construct a validated instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionError {
    /// The upstream byte-zero sentinel cannot be emitted.
    ReservedOpcode,
    /// The operand variant does not match the opcode metadata.
    OperandFormatMismatch {
        /// Opcode being constructed.
        opcode: FinalOpcode,
        /// Format required by the opcode.
        expected: OperandFormat,
        /// Format supplied by the caller.
        actual: OperandFormat,
    },
}

impl fmt::Display for InstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedOpcode => {
                formatter.write_str("the reserved invalid opcode cannot be emitted")
            }
            Self::OperandFormatMismatch {
                opcode,
                expected,
                actual,
            } => write!(
                formatter,
                "opcode {opcode} requires operand format {}, got {}",
                expected.upstream_name(),
                actual.upstream_name()
            ),
        }
    }
}

impl Error for InstructionError {}

/// Failure to encode typed operands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandEncodeError {
    /// A format exceeded the fixed operand buffer.
    BufferCapacityExceeded {
        /// Format being encoded.
        format: OperandFormat,
        /// Available fixed capacity.
        capacity_bytes: usize,
    },
    /// The encoded width disagreed with the pinned format table.
    SchemaWidthMismatch {
        /// Format being encoded.
        format: OperandFormat,
        /// Width declared by the table.
        expected_bytes: u8,
        /// Width produced by the encoder.
        actual_bytes: u8,
    },
}

impl fmt::Display for OperandEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferCapacityExceeded {
                format,
                capacity_bytes,
            } => write!(
                formatter,
                "operand format {} exceeds encoder capacity of {capacity_bytes} bytes",
                format.upstream_name()
            ),
            Self::SchemaWidthMismatch {
                format,
                expected_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "operand format {} declares {expected_bytes} bytes but encoded {actual_bytes}",
                format.upstream_name()
            ),
        }
    }
}

impl Error for OperandEncodeError {}

/// Failure to decode a typed operand payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandDecodeError {
    /// The payload is shorter or longer than the selected format.
    LengthMismatch {
        /// Selected operand format.
        format: OperandFormat,
        /// Required width.
        expected_bytes: u8,
        /// Supplied width.
        actual_bytes: usize,
    },
}

impl fmt::Display for OperandDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::LengthMismatch {
            format,
            expected_bytes,
            actual_bytes,
        } = self;
        write!(
            formatter,
            "operand format {} requires {expected_bytes} bytes, got {actual_bytes}",
            format.upstream_name()
        )
    }
}

impl Error for OperandDecodeError {}

/// Failure to decode a final instruction stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// The typed PC cannot be represented as a host slice index.
    PcNotRepresentable {
        /// Rejected position.
        pc: BytecodePc,
    },
    /// The position is beyond the input slice.
    PcOutOfBounds {
        /// Rejected position.
        pc: BytecodePc,
        /// Complete input length.
        bytecode_len: usize,
    },
    /// No opcode byte remains at the requested position.
    MissingOpcode {
        /// Instruction position.
        pc: BytecodePc,
        /// Number of opcode bytes required.
        expected_bytes: u8,
        /// Bytes remaining at the position.
        remaining_bytes: usize,
    },
    /// The opcode byte is reserved or outside the final table.
    InvalidOpcode {
        /// Instruction position.
        pc: BytecodePc,
        /// Rejected byte.
        opcode_byte: u8,
        /// Exact opcode-table error.
        source: FinalOpcodeDecodeError,
    },
    /// The opcode is known but its complete operands are unavailable.
    TruncatedOperands {
        /// Instruction position.
        pc: BytecodePc,
        /// Decoded opcode.
        opcode: FinalOpcode,
        /// Required operand bytes.
        expected_bytes: u8,
        /// Operand bytes remaining after the opcode.
        remaining_bytes: usize,
    },
    /// Typed operand decoding failed after length validation.
    OperandDecoding {
        /// Instruction position.
        pc: BytecodePc,
        /// Decoded opcode.
        opcode: FinalOpcode,
        /// Exact operand error.
        source: OperandDecodeError,
    },
    /// Advancing past the instruction would overflow `u32`.
    NextPcOverflow {
        /// Instruction position.
        pc: BytecodePc,
        /// Encoded instruction width.
        instruction_size: u8,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PcNotRepresentable { pc } => {
                write!(
                    formatter,
                    "bytecode PC {pc} is not representable on this host"
                )
            }
            Self::PcOutOfBounds { pc, bytecode_len } => {
                write!(
                    formatter,
                    "bytecode PC {pc} is beyond input length {bytecode_len}"
                )
            }
            Self::MissingOpcode {
                pc,
                expected_bytes,
                remaining_bytes,
            } => write!(
                formatter,
                "missing opcode at PC {pc}: expected {expected_bytes} byte, {remaining_bytes} remaining"
            ),
            Self::InvalidOpcode {
                pc, opcode_byte, ..
            } => write!(
                formatter,
                "invalid opcode 0x{opcode_byte:02x} at bytecode PC {pc}"
            ),
            Self::TruncatedOperands {
                pc,
                opcode,
                expected_bytes,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated {opcode} operands at PC {pc}: expected {expected_bytes} bytes, {remaining_bytes} remaining"
            ),
            Self::OperandDecoding { pc, opcode, source } => write!(
                formatter,
                "cannot decode {opcode} operands at bytecode PC {pc}: {source}"
            ),
            Self::NextPcOverflow {
                pc,
                instruction_size,
            } => write!(
                formatter,
                "instruction of {instruction_size} bytes at PC {pc} overflows the bytecode PC"
            ),
        }
    }
}

impl Error for DecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidOpcode { source, .. } => Some(source),
            Self::OperandDecoding { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Failure to append a final instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// Instruction validation failed before any bytes were written.
    InvalidInstruction {
        /// Position where the instruction would have started.
        pc: BytecodePc,
        /// Exact validation failure.
        source: InstructionError,
    },
    /// Typed operand encoding failed.
    OperandEncoding {
        /// Position where the instruction would have started.
        pc: BytecodePc,
        /// Opcode being encoded.
        opcode: FinalOpcode,
        /// Exact operand error.
        source: OperandEncodeError,
    },
    /// Operand and opcode metadata produced different instruction sizes.
    SchemaSizeMismatch {
        /// Position where the instruction would have started.
        pc: BytecodePc,
        /// Opcode being encoded.
        opcode: FinalOpcode,
        /// Width declared by opcode metadata.
        declared_size: u8,
        /// Width produced by the operand encoder.
        actual_size: u8,
    },
    /// The next absolute bytecode position would exceed `u32`.
    PcOverflow {
        /// Position where the instruction would have started.
        pc: BytecodePc,
        /// Encoded instruction width.
        instruction_size: u8,
    },
    /// The fragment's encoded length would exceed `u32`.
    EncodedLengthOverflow {
        /// Bytes already encoded.
        encoded_bytes: u32,
        /// Encoded instruction width.
        instruction_size: u8,
    },
    /// The configured byte resource limit would be exceeded.
    ByteLimitExceeded {
        /// Position where the instruction would have started.
        pc: BytecodePc,
        /// Encoded instruction width.
        instruction_size: u8,
        /// Bytes already encoded.
        encoded_bytes: u32,
        /// Maximum encoded fragment size.
        byte_limit: u32,
    },
    /// The backing vector could not reserve space.
    AllocationFailed {
        /// Position where the instruction would have started.
        pc: BytecodePc,
        /// Additional capacity requested.
        additional_bytes: u8,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInstruction { pc, source } => {
                write!(
                    formatter,
                    "invalid instruction at bytecode PC {pc}: {source}"
                )
            }
            Self::OperandEncoding { pc, opcode, source } => write!(
                formatter,
                "cannot encode {opcode} operands at bytecode PC {pc}: {source}"
            ),
            Self::SchemaSizeMismatch {
                pc,
                opcode,
                declared_size,
                actual_size,
            } => write!(
                formatter,
                "opcode {opcode} at PC {pc} declares {declared_size} bytes but encoded {actual_size}"
            ),
            Self::PcOverflow {
                pc,
                instruction_size,
            } => write!(
                formatter,
                "instruction of {instruction_size} bytes at PC {pc} overflows the bytecode PC"
            ),
            Self::EncodedLengthOverflow {
                encoded_bytes,
                instruction_size,
            } => write!(
                formatter,
                "adding {instruction_size} bytes to encoded length {encoded_bytes} overflows u32"
            ),
            Self::ByteLimitExceeded {
                pc,
                instruction_size,
                encoded_bytes,
                byte_limit,
            } => write!(
                formatter,
                "instruction of {instruction_size} bytes at PC {pc} exceeds byte limit {byte_limit} after {encoded_bytes} encoded bytes"
            ),
            Self::AllocationFailed {
                pc,
                additional_bytes,
            } => write!(
                formatter,
                "cannot reserve {additional_bytes} bytes for instruction at bytecode PC {pc}"
            ),
        }
    }
}

impl Error for EncodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidInstruction { source, .. } => Some(source),
            Self::OperandEncoding { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn read_u8(bytes: &[u8], offset: usize) -> Option<u8> {
    bytes.get(offset).copied()
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        read_u8(bytes, offset)?,
        read_u8(bytes, offset.checked_add(1)?)?,
    ]))
}

fn read_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    Some(i16::from_le_bytes([
        read_u8(bytes, offset)?,
        read_u8(bytes, offset.checked_add(1)?)?,
    ]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        read_u8(bytes, offset)?,
        read_u8(bytes, offset.checked_add(1)?)?,
        read_u8(bytes, offset.checked_add(2)?)?,
        read_u8(bytes, offset.checked_add(3)?)?,
    ]))
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes([
        read_u8(bytes, offset)?,
        read_u8(bytes, offset.checked_add(1)?)?,
        read_u8(bytes, offset.checked_add(2)?)?,
        read_u8(bytes, offset.checked_add(3)?)?,
    ]))
}
