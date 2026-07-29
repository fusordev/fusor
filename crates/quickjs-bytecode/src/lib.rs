//! Safe opcode metadata, checked instruction codec, and bounded disassembly
//! for the pure-Rust `QuickJS` port.
//!
//! The opcode order, encoded sizes, operand formats, and base stack effects are
//! translated from `quickjs-opcode.h` in the official 2026-06-04 release.
//! Temporary compiler opcodes deliberately use a separate type even though
//! their intermediate byte values overlap the final short-opcode range.
//!
//! The owned instruction codec uses deterministic little-endian operands.
//! Upstream private in-memory bytecode is native-endian and its object writer
//! has a separate versioned format, so binary compatibility is not claimed.
//!
//! This crate describes and renders bytecode. It does not execute or otherwise
//! trust it.

#![forbid(unsafe_code)]

use std::{error::Error, fmt};

mod assembler;
mod codec;
mod compiler_graph;
mod compiler_string;
mod disassembly;
mod function;
mod verifier;

pub use assembler::{
    AssembledBytecode, AssemblerError, AssemblerLabel, AssemblerLimits, AssemblerResource,
    BranchKind, BytecodeAssembler,
};
pub use codec::{
    AtomPoolIndex, BytecodeBuilder, BytecodePc, DecodeError, DecodedInstruction, EncodeError,
    EncodedOperands, Instruction, InstructionDecoder, InstructionError, MAX_ENCODED_OPERAND_BYTES,
    OperandDecodeError, OperandEncodeError, Operands, decode_instruction,
};
pub use compiler_graph::{
    Binary64Constant, CompilerClosureSource, CompilerConstant, CompilerConstantValue,
    FunctionGraphResource, FunctionGraphUsage, FunctionGraphVerificationError,
    FunctionGraphVerificationErrorKind, FunctionGraphVerificationLimits, FunctionTemplateId,
    MAX_FUNCTION_GRAPH_NESTING_DEPTH, MAX_FUNCTION_GRAPH_TEMPLATES, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionGraph, VerifiedCompilerFunction, VerifiedCompilerFunctionGraph,
    verify_compiler_function_graph,
};
pub use compiler_string::{
    CompilerAtom, CompilerString, CompilerStringCodeUnits, CompilerStringError,
    CompilerStringLengthError, MAX_COMPILER_STRING_CODE_UNITS,
};
pub use disassembly::{
    DisassemblyError, DisassemblyLimits, DisassemblySummary, render_disassembly,
};
pub use function::{
    FunctionBitField, FunctionHeaderFlag, FunctionHeaderFlags, FunctionKind,
    FunctionKindRequirement, FunctionMode, UnverifiedFunctionHeader, VerifiedFunctionHeader,
};
pub use verifier::{
    CompilerCaptureLayout, CompilerCapturedBinding, CompilerConstantKind, CompilerConstantLayout,
    ControlFlowEdge, FunctionCountDomain, FunctionIndexDomains, FunctionPath, InstructionIndex,
    InvalidControlFlowTargetReason, MAX_FUNCTION_INDEX_ENTRIES, MAX_OPERAND_STACK_DEPTH,
    OperandIndexDomain, SecondaryOperandField, UnsupportedVerifierFeature,
    UnverifiedCompilerFunctionBody, UnverifiedFunctionBody, VerificationError,
    VerificationErrorKind, VerificationLimits, VerificationResource, VerifiedControlFlow,
    VerifiedInstruction, VerifiedSuccessorKind, VerifiedSuccessors, verify_compiler_control_flow,
    verify_control_flow,
};

/// The official `QuickJS` release from which the opcode schema was translated.
pub const QUICKJS_COMPATIBILITY_RELEASE: &str = "2026-06-04";

/// The encoding of an opcode's operands.
///
/// Every width is measured in bytes after the one-byte opcode. Multi-byte
/// integer byte order belongs to the bytecode reader/writer, not this schema.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum OperandFormat {
    /// No encoded operand.
    None,
    /// An integer implied by the opcode (`push_minus1` through `push_7`).
    NoneInt,
    /// A local index implied by the opcode.
    NoneLoc,
    /// An argument index implied by the opcode.
    NoneArg,
    /// A closure-variable index implied by the opcode.
    NoneVarRef,
    /// One unsigned 8-bit integer.
    U8,
    /// One signed 8-bit integer.
    I8,
    /// One unsigned 8-bit local index.
    Loc8,
    /// One unsigned 8-bit constant-pool index.
    Const8,
    /// One signed 8-bit relative label displacement.
    Label8,
    /// One unsigned 16-bit integer.
    U16,
    /// One signed 16-bit integer.
    I16,
    /// One signed 16-bit relative label displacement.
    Label16,
    /// A 16-bit argument count that adds to the base pop count.
    NPop,
    /// An argument count implied by the opcode (`call0` through `call3`).
    NPopX,
    /// Two 16-bit integers; the first adds to the base pop count.
    NPopU16,
    /// One unsigned 16-bit local index.
    Loc,
    /// One unsigned 16-bit argument index.
    Arg,
    /// One unsigned 16-bit closure-variable index.
    VarRef,
    /// One unsigned 32-bit integer.
    U32,
    /// One signed 32-bit integer.
    I32,
    /// One unsigned 32-bit constant-pool index.
    Const,
    /// One 32-bit label index or relative displacement, depending on phase.
    Label,
    /// One unsigned 32-bit function-local atom-pool index.
    Atom,
    /// A 32-bit function-local atom-pool index followed by an unsigned 8-bit
    /// integer.
    AtomU8,
    /// A 32-bit function-local atom-pool index followed by an unsigned 16-bit
    /// integer.
    AtomU16,
    /// A 32-bit function-local atom-pool index, 32-bit label, and unsigned
    /// 8-bit integer.
    AtomLabelU8,
    /// A 32-bit function-local atom-pool index, 32-bit label, and unsigned
    /// 16-bit integer.
    AtomLabelU16,
    /// A 32-bit label followed by an unsigned 16-bit integer.
    LabelU16,
}

impl OperandFormat {
    /// Returns the number of encoded operand bytes.
    #[must_use]
    pub const fn operand_width(self) -> u8 {
        match self {
            Self::None
            | Self::NoneInt
            | Self::NoneLoc
            | Self::NoneArg
            | Self::NoneVarRef
            | Self::NPopX => 0,
            Self::U8 | Self::I8 | Self::Loc8 | Self::Const8 | Self::Label8 => 1,
            Self::U16
            | Self::I16
            | Self::Label16
            | Self::NPop
            | Self::Loc
            | Self::Arg
            | Self::VarRef => 2,
            Self::NPopU16 | Self::U32 | Self::I32 | Self::Const | Self::Label | Self::Atom => 4,
            Self::AtomU8 => 5,
            Self::AtomU16 | Self::LabelU16 => 6,
            Self::AtomLabelU8 => 9,
            Self::AtomLabelU16 => 10,
        }
    }

    /// Returns the full encoded instruction size, including its opcode byte.
    #[must_use]
    pub const fn instruction_size(self) -> u8 {
        self.operand_width() + 1
    }

    /// Returns the spelling used by the upstream macro table.
    #[must_use]
    pub const fn upstream_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NoneInt => "none_int",
            Self::NoneLoc => "none_loc",
            Self::NoneArg => "none_arg",
            Self::NoneVarRef => "none_var_ref",
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::Loc8 => "loc8",
            Self::Const8 => "const8",
            Self::Label8 => "label8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::Label16 => "label16",
            Self::NPop => "npop",
            Self::NPopX => "npopx",
            Self::NPopU16 => "npop_u16",
            Self::Loc => "loc",
            Self::Arg => "arg",
            Self::VarRef => "var_ref",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::Const => "const",
            Self::Label => "label",
            Self::Atom => "atom",
            Self::AtomU8 => "atom_u8",
            Self::AtomU16 => "atom_u16",
            Self::AtomLabelU8 => "atom_label_u8",
            Self::AtomLabelU16 => "atom_label_u16",
            Self::LabelU16 => "label_u16",
        }
    }
}

/// Static metadata for one final or temporary opcode.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OpcodeMetadata {
    mnemonic: &'static str,
    instruction_size: u8,
    base_pops: u8,
    base_pushes: u8,
    operand_format: OperandFormat,
}

impl OpcodeMetadata {
    const fn new(
        mnemonic: &'static str,
        instruction_size: u8,
        base_pops: u8,
        base_pushes: u8,
        operand_format: OperandFormat,
    ) -> Self {
        Self {
            mnemonic,
            instruction_size,
            base_pops,
            base_pushes,
            operand_format,
        }
    }

    /// Returns the exact upstream mnemonic.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        self.mnemonic
    }

    /// Returns the full encoded instruction size in bytes.
    #[must_use]
    pub const fn instruction_size(self) -> u8 {
        self.instruction_size
    }

    /// Returns the fixed number of stack values removed by the opcode.
    ///
    /// Dynamic argument counts are intentionally excluded.
    #[must_use]
    pub const fn base_pops(self) -> u8 {
        self.base_pops
    }

    /// Returns the fixed number of stack values pushed by the opcode.
    #[must_use]
    pub const fn base_pushes(self) -> u8 {
        self.base_pushes
    }

    /// Returns the opcode's operand encoding.
    #[must_use]
    pub const fn operand_format(self) -> OperandFormat {
        self.operand_format
    }

    /// Returns the fixed portion of this opcode's stack effect.
    #[must_use]
    pub fn base_stack_effect(self) -> StackEffect {
        StackEffect::new(u32::from(self.base_pops), u32::from(self.base_pushes))
    }
}

macro_rules! define_opcode_tables {
    (
        final {
            $(
                $final_variant:ident => (
                    $final_name:literal,
                    $final_size:literal,
                    $final_pops:literal,
                    $final_pushes:literal,
                    $final_format:ident
                ),
            )+
        }
        temporary {
            $(
                $temporary_variant:ident => (
                    $temporary_name:literal,
                    $temporary_size:literal,
                    $temporary_pops:literal,
                    $temporary_pushes:literal,
                    $temporary_format:ident
                ),
            )+
        }
        short {
            $(
                $short_variant:ident => (
                    $short_name:literal,
                    $short_size:literal,
                    $short_pops:literal,
                    $short_pushes:literal,
                    $short_format:ident
                ),
            )+
        }
    ) => {
        /// A final bytecode opcode.
        ///
        /// Discriminants are the exact one-byte values used by the 2026-06-04
        /// `QuickJS` release. The reserved [`FinalOpcode::Invalid`] sentinel is
        /// represented for table parity but rejected by checked decoding.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(u8)]
        pub enum FinalOpcode {
            $($final_variant,)+
            $($short_variant,)+
        }

        /// Every final opcode in encoded-byte order, including `Invalid`.
        pub const ALL_FINAL_OPCODES: &[FinalOpcode] = &[
            $(FinalOpcode::$final_variant,)+
            $(FinalOpcode::$short_variant,)+
        ];

        /// Metadata for every final opcode in encoded-byte order.
        pub const FINAL_OPCODE_METADATA: &[OpcodeMetadata] = &[
            $(
                OpcodeMetadata::new(
                    $final_name,
                    $final_size,
                    $final_pops,
                    $final_pushes,
                    OperandFormat::$final_format,
                ),
            )+
            $(
                OpcodeMetadata::new(
                    $short_name,
                    $short_size,
                    $short_pops,
                    $short_pushes,
                    OperandFormat::$short_format,
                ),
            )+
        ];

        /// A compiler-only opcode used before final bytecode emission.
        ///
        /// Temporary opcode values overlap final short opcodes. Keeping them in
        /// a separate type prevents phase confusion in safe Rust.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(u8)]
        pub enum TemporaryOpcode {
            $($temporary_variant,)+
        }

        /// Every temporary opcode in upstream order.
        pub const ALL_TEMPORARY_OPCODES: &[TemporaryOpcode] = &[
            $(TemporaryOpcode::$temporary_variant,)+
        ];

        /// Metadata for every temporary opcode in upstream order.
        pub const TEMPORARY_OPCODE_METADATA: &[OpcodeMetadata] = &[
            $(
                OpcodeMetadata::new(
                    $temporary_name,
                    $temporary_size,
                    $temporary_pops,
                    $temporary_pushes,
                    OperandFormat::$temporary_format,
                ),
            )+
        ];
    };
}

include!("opcode_table.rs");

/// Number of final opcodes, including the reserved `Invalid` sentinel.
pub const FINAL_OPCODE_COUNT: usize = ALL_FINAL_OPCODES.len();

/// Whether the pinned upstream build includes its final short-opcode table.
///
/// `quickjs.c` in the 2026-06-04 release defines `SHORT_OPCODES` to `1`.
pub const SHORT_OPCODES_ENABLED: bool = true;

/// Encoded byte of the first final short opcode.
pub const FIRST_SHORT_OPCODE_BYTE: u8 = FinalOpcode::PushMinus1 as u8;

/// Number of non-short final opcodes, including `Invalid`.
pub const NON_SHORT_FINAL_OPCODE_COUNT: usize = FIRST_SHORT_OPCODE_BYTE as usize;

/// Number of final short opcodes.
pub const SHORT_FINAL_OPCODE_COUNT: usize = FINAL_OPCODE_COUNT - NON_SHORT_FINAL_OPCODE_COUNT;

/// Number of temporary compiler opcodes.
pub const TEMPORARY_OPCODE_COUNT: usize = ALL_TEMPORARY_OPCODES.len();

/// First intermediate byte occupied by temporary compiler opcodes.
pub const TEMPORARY_OPCODE_START: u8 = FinalOpcode::Nop as u8 + 1;

/// First byte after the temporary compiler-opcode range.
pub const TEMPORARY_OPCODE_END_EXCLUSIVE: u8 =
    TEMPORARY_OPCODE_START + TemporaryOpcode::LineNum as u8 + 1;

const _: () = assert!(TEMPORARY_OPCODE_START == FIRST_SHORT_OPCODE_BYTE);

impl FinalOpcode {
    /// Returns the encoded opcode byte.
    #[must_use]
    pub const fn encoded_byte(self) -> u8 {
        self as u8
    }

    /// Returns this opcode's static metadata.
    #[must_use]
    pub fn metadata(self) -> OpcodeMetadata {
        FINAL_OPCODE_METADATA[usize::from(self.encoded_byte())]
    }

    /// Returns the exact upstream mnemonic.
    #[must_use]
    pub fn mnemonic(self) -> &'static str {
        self.metadata().mnemonic()
    }

    /// Returns whether this opcode is in the final short-opcode range.
    #[must_use]
    pub const fn is_short(self) -> bool {
        self.encoded_byte() >= FIRST_SHORT_OPCODE_BYTE
    }

    /// Converts a table byte to an opcode, including the reserved sentinel.
    ///
    /// Bytecode readers should use [`FinalOpcode::decode`] instead.
    #[must_use]
    pub fn from_table_byte(byte: u8) -> Option<Self> {
        ALL_FINAL_OPCODES.get(usize::from(byte)).copied()
    }

    /// Checks and decodes one final opcode byte.
    ///
    /// # Errors
    ///
    /// Returns [`FinalOpcodeDecodeError::ReservedInvalid`] for byte zero and
    /// [`FinalOpcodeDecodeError::Unknown`] for bytes outside the final table.
    pub fn decode(byte: u8) -> Result<Self, FinalOpcodeDecodeError> {
        match Self::from_table_byte(byte) {
            Some(Self::Invalid) => Err(FinalOpcodeDecodeError::ReservedInvalid),
            Some(opcode) => Ok(opcode),
            None => Err(FinalOpcodeDecodeError::Unknown { byte }),
        }
    }

    /// Returns where this opcode obtains a dynamic argument count.
    #[must_use]
    pub fn argument_count_source(self) -> ArgumentCountSource {
        match self.metadata().operand_format() {
            OperandFormat::NPop | OperandFormat::NPopU16 => ArgumentCountSource::FirstU16Operand,
            OperandFormat::NPopX => ArgumentCountSource::Opcode,
            _ => ArgumentCountSource::None,
        }
    }

    /// Resolves the full stack effect for this opcode.
    ///
    /// `argument_count` is required for `npop` and `npop_u16` instructions,
    /// including calls, `array_from`, and `eval`. It must be omitted for fixed
    /// instructions and `call0` through `call3`, whose count is in the opcode.
    ///
    /// # Errors
    ///
    /// Returns a structured error when a required count is missing, an
    /// unexpected count is supplied, or an invalid opcode is used with the
    /// implicit-count format.
    pub fn stack_effect(
        self,
        argument_count: Option<u16>,
    ) -> Result<StackEffect, StackEffectError> {
        let metadata = self.metadata();
        let base = metadata.base_stack_effect();

        match metadata.operand_format() {
            OperandFormat::NPop | OperandFormat::NPopU16 => {
                let count = argument_count
                    .ok_or(StackEffectError::MissingArgumentCount { opcode: self })?;
                Ok(base.with_additional_pops(u32::from(count)))
            }
            OperandFormat::NPopX => {
                if let Some(count) = argument_count {
                    return Err(StackEffectError::UnexpectedArgumentCount {
                        opcode: self,
                        argument_count: count,
                    });
                }
                let count = match self {
                    Self::Call0 => 0,
                    Self::Call1 => 1,
                    Self::Call2 => 2,
                    Self::Call3 => 3,
                    _ => {
                        return Err(StackEffectError::InvalidImplicitArgumentOpcode {
                            opcode: self,
                        });
                    }
                };
                Ok(base.with_additional_pops(count))
            }
            _ => {
                if let Some(count) = argument_count {
                    return Err(StackEffectError::UnexpectedArgumentCount {
                        opcode: self,
                        argument_count: count,
                    });
                }
                Ok(base)
            }
        }
    }
}

impl TryFrom<u8> for FinalOpcode {
    type Error = FinalOpcodeDecodeError;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::decode(byte)
    }
}

impl fmt::Display for FinalOpcode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.mnemonic())
    }
}

impl TemporaryOpcode {
    /// Returns the intermediate byte used by the upstream compiler.
    ///
    /// This byte intentionally overlaps a final short opcode.
    #[must_use]
    pub const fn encoded_byte(self) -> u8 {
        TEMPORARY_OPCODE_START + self as u8
    }

    /// Returns this temporary opcode's static metadata.
    #[must_use]
    pub fn metadata(self) -> OpcodeMetadata {
        TEMPORARY_OPCODE_METADATA[usize::from(self as u8)]
    }

    /// Returns the exact upstream mnemonic.
    #[must_use]
    pub fn mnemonic(self) -> &'static str {
        self.metadata().mnemonic()
    }

    /// Checks and decodes an intermediate compiler-opcode byte.
    ///
    /// # Errors
    ///
    /// Returns an error when `byte` is outside the temporary range.
    pub fn decode(byte: u8) -> Result<Self, TemporaryOpcodeDecodeError> {
        let Some(index) = byte.checked_sub(TEMPORARY_OPCODE_START) else {
            return Err(TemporaryOpcodeDecodeError { byte });
        };
        ALL_TEMPORARY_OPCODES
            .get(usize::from(index))
            .copied()
            .ok_or(TemporaryOpcodeDecodeError { byte })
    }
}

impl TryFrom<u8> for TemporaryOpcode {
    type Error = TemporaryOpcodeDecodeError;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::decode(byte)
    }
}

impl fmt::Display for TemporaryOpcode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.mnemonic())
    }
}

/// The location of an opcode's dynamic argument count.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArgumentCountSource {
    /// The opcode has no dynamic argument count.
    None,
    /// The argument count is the first unsigned 16-bit operand.
    FirstU16Operand,
    /// The argument count is encoded by the opcode itself.
    Opcode,
}

/// The number of stack values popped and pushed by an instruction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StackEffect {
    pops: u32,
    pushes: u32,
}

impl StackEffect {
    /// Creates a stack effect.
    #[must_use]
    pub const fn new(pops: u32, pushes: u32) -> Self {
        Self { pops, pushes }
    }

    /// Returns the number of values removed from the operand stack.
    #[must_use]
    pub const fn pops(self) -> u32 {
        self.pops
    }

    /// Returns the number of values added to the operand stack.
    #[must_use]
    pub const fn pushes(self) -> u32 {
        self.pushes
    }

    /// Returns `pushes - pops`.
    #[must_use]
    pub fn net_change(self) -> i64 {
        i64::from(self.pushes) - i64::from(self.pops)
    }

    #[must_use]
    const fn with_additional_pops(self, additional_pops: u32) -> Self {
        Self::new(self.pops + additional_pops, self.pushes)
    }
}

/// Failure to resolve an opcode's complete stack effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackEffectError {
    /// A dynamic instruction did not receive its encoded argument count.
    MissingArgumentCount {
        /// Opcode whose argument count is missing.
        opcode: FinalOpcode,
    },
    /// A count was supplied to an opcode that does not take one externally.
    UnexpectedArgumentCount {
        /// Opcode that rejected the count.
        opcode: FinalOpcode,
        /// Rejected count.
        argument_count: u16,
    },
    /// An opcode other than `call0` through `call3` used `npopx`.
    InvalidImplicitArgumentOpcode {
        /// Malformed opcode-table entry.
        opcode: FinalOpcode,
    },
}

impl fmt::Display for StackEffectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingArgumentCount { opcode } => {
                write!(formatter, "opcode {opcode} requires an argument count")
            }
            Self::UnexpectedArgumentCount {
                opcode,
                argument_count,
            } => write!(
                formatter,
                "opcode {opcode} does not accept argument count {argument_count}"
            ),
            Self::InvalidImplicitArgumentOpcode { opcode } => write!(
                formatter,
                "opcode {opcode} has an invalid implicit argument-count format"
            ),
        }
    }
}

impl Error for StackEffectError {}

/// Failure to decode a byte as a final opcode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalOpcodeDecodeError {
    /// Byte zero names the upstream sentinel and is never executable.
    ReservedInvalid,
    /// The byte is outside the final opcode table.
    Unknown {
        /// Unrecognized opcode byte.
        byte: u8,
    },
}

impl FinalOpcodeDecodeError {
    /// Returns the rejected byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::ReservedInvalid => 0,
            Self::Unknown { byte } => byte,
        }
    }
}

impl fmt::Display for FinalOpcodeDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedInvalid => {
                formatter.write_str("opcode byte 0x00 is the reserved invalid opcode")
            }
            Self::Unknown { byte } => write!(
                formatter,
                "opcode byte 0x{byte:02x} is outside the final opcode table"
            ),
        }
    }
}

impl Error for FinalOpcodeDecodeError {}

/// Failure to decode a byte as a temporary compiler opcode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TemporaryOpcodeDecodeError {
    byte: u8,
}

impl TemporaryOpcodeDecodeError {
    /// Returns the rejected byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self.byte
    }
}

impl fmt::Display for TemporaryOpcodeDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "opcode byte 0x{:02x} is outside the temporary opcode range 0x{:02x}..0x{:02x}",
            self.byte, TEMPORARY_OPCODE_START, TEMPORARY_OPCODE_END_EXCLUSIVE
        )
    }
}

impl Error for TemporaryOpcodeDecodeError {}
