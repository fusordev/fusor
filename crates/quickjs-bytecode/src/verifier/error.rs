use std::{error::Error, fmt};

use crate::{
    BytecodePc, DecodeError, DecodedInstruction, FinalOpcode, OperandFormat, StackEffectError,
    function::{FunctionBitField, FunctionHeaderFlag, FunctionKind, FunctionKindRequirement},
};

use super::{CompilerCapturedBinding, CompilerConstantKind, FunctionPath, VerificationResource};

/// A function metadata count subject to a structural maximum.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionCountDomain {
    /// Function arguments.
    Arguments,
    /// Function locals.
    Locals,
    /// Function-owned variable-reference cells.
    VariableReferences,
    /// Closure-variable descriptors.
    ClosureVariables,
    /// Serialized or compiler-declared stack size.
    ExpectedStackSize,
}

/// Operand index namespace used by an out-of-bounds error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperandIndexDomain {
    /// Function-local atom pool.
    AtomPool,
    /// Function constant pool.
    ConstantPool,
    /// Function local variables.
    Local,
    /// Function arguments.
    Argument,
    /// Function closure-variable table.
    ClosureVariable,
}

/// An instruction-specific secondary operand.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SecondaryOperandField {
    /// `special_object` selector.
    SpecialObjectKind,
    /// `rest` first-argument index.
    RestFirstArgument,
    /// `apply` magic.
    ApplyMagic,
    /// `throw_error` kind.
    ThrowErrorKind,
    /// `define_method` kind and flags.
    DefineMethodFlags,
    /// `define_private_field` element kind.
    DefinePrivateFieldKind,
    /// `define_class` flags.
    DefineClassFlags,
    /// `with_*`'s `is_with` byte.
    IsWith,
    /// `iterator_call` flags.
    IteratorCallFlags,
}

/// Kind of control-flow edge being validated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ControlFlowEdge {
    /// Ordinary next-instruction edge.
    Fallthrough,
    /// Conditional taken edge.
    Branch,
    /// Unconditional jump edge.
    Jump,
    /// Exception-handler edge installed by `catch`.
    CatchHandler,
    /// Finally-subroutine edge installed by `gosub`.
    FinallySubroutine,
    /// Return continuation installed by `gosub`.
    FinallyContinuation,
    /// Resolved binding edge used by `with_*`.
    WithBinding,
}

/// Why a computed target is invalid.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InvalidControlFlowTargetReason {
    /// The target is negative or at/beyond the bytecode end.
    OutsideBytecode,
    /// The target points into an instruction's operand payload.
    NotInstructionBoundary,
    /// A catch target used reserved PC zero.
    CatchTargetZero,
}

/// Missing semantic component that keeps an opcode fail-closed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnsupportedVerifierFeature {
    /// Constant kind and nested function verification.
    ConstantPoolTyping,
    /// Raw function stack slots used by class construction.
    RawFunctionStack,
    /// Eval scope chains.
    EvalScopeMetadata,
    /// Captured vardef and closure metadata.
    CapturedBindingMetadata,
    /// Typed catch markers.
    CatchMarkers,
    /// Typed finally return addresses.
    FinallyReturnAddresses,
    /// Split `with` environment transfers.
    WithEnvironmentBranches,
    /// Typed iterator catch markers.
    IteratorMarkers,
    /// Packed stack-relative property-copy offsets.
    PackedStackOffsets,
}

/// A structured control-flow verifier failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationErrorKind {
    /// The bytecode stream is empty.
    EmptyBytecode,
    /// A configured resource limit was exceeded.
    LimitExceeded {
        /// Limited resource.
        resource: VerificationResource,
        /// Inclusive configured limit.
        limit: u64,
        /// Observed value.
        observed: u64,
    },
    /// A verifier-owned collection could not reserve capacity.
    AllocationFailed {
        /// Collection resource.
        resource: VerificationResource,
        /// Additional elements requested.
        requested: u64,
    },
    /// A fixed function count exceeded the pinned structural maximum.
    MetadataCountOutOfRange {
        /// Count namespace.
        domain: FunctionCountDomain,
        /// Rejected count.
        value: u32,
        /// Inclusive maximum.
        maximum: u32,
    },
    /// A serialized function field contains disallowed bits.
    DisallowedFunctionBits {
        /// Rejected bit field.
        field: FunctionBitField,
        /// Complete rejected value.
        value: u16,
        /// Mask of bits allowed in this stored field.
        allowed_mask: u16,
        /// Rejected bits outside `allowed_mask`.
        disallowed_bits: u16,
    },
    /// More source arguments were marked defined than the function owns.
    DefinedArgumentCountOutOfRange {
        /// Count of source-defined arguments.
        defined: u32,
        /// Complete function argument count.
        argument_count: u32,
    },
    /// More variable-reference cells were declared than bindings can own.
    VariableReferenceCountOutOfRange {
        /// Declared variable-reference cell count.
        variable_references: u32,
        /// Function argument count.
        argument_count: u32,
        /// Function local count.
        local_count: u32,
    },
    /// The compiler capture table does not define every declared
    /// variable-reference cell exactly once.
    CompilerCaptureCountMismatch {
        /// Variable-reference cells declared by the function header.
        variable_references: u32,
        /// Entries supplied by the compiler capture layout.
        captures: u64,
    },
    /// Compiler output declared variable-reference cells without supplying
    /// their typed capture layout.
    MissingCompilerCaptureLayout {
        /// Variable-reference cells declared by the function header.
        variable_references: u32,
    },
    /// The compiler constant layout does not define every declared
    /// constant-pool entry exactly once.
    CompilerConstantCountMismatch {
        /// Constant-pool length declared by the function domains.
        declared: u32,
        /// Entries supplied by the compiler constant layout.
        entries: u64,
    },
    /// Compiler output declared constants without supplying their type layout.
    MissingCompilerConstantLayout {
        /// Constant-pool length declared by the function domains.
        constants: u32,
    },
    /// A constant-consuming opcode does not match the compiler-declared kind.
    CompilerConstantKindMismatch {
        /// Rejected constant-pool index.
        index: u32,
        /// Kind required by the opcode.
        expected: CompilerConstantKind,
        /// Kind supplied by the compiler layout.
        actual: CompilerConstantKind,
    },
    /// A compiler capture names a binding outside its frame domain.
    CompilerCaptureIndexOutOfBounds {
        /// Rejected binding.
        binding: CompilerCapturedBinding,
        /// Length of the binding's argument or local domain.
        len: u32,
    },
    /// Two variable-reference cells name the same frame binding.
    DuplicateCompilerCapture {
        /// Repeated binding. Function-local and scoped-local variants with the
        /// same index identify the same frame binding.
        binding: CompilerCapturedBinding,
    },
    /// A mapped arguments position is outside the function argument domain.
    CompilerMappedArgumentIndexOutOfBounds {
        /// Rejected formal position.
        index: u32,
        /// Function argument-domain length.
        len: u32,
    },
    /// Mapped arguments positions are not strictly ascending.
    CompilerMappedArgumentsNotAscending {
        /// Earlier position.
        previous: u32,
        /// Repeated or descending position.
        index: u32,
    },
    /// `close_loc` does not name an explicitly scoped captured local.
    CloseLocRequiresScopedCapture {
        /// Rejected local index.
        local: u32,
    },
    /// A packed flag is invalid for the decoded function kind.
    FunctionFlagNotAllowedForKind {
        /// Rejected packed flag.
        flag: FunctionHeaderFlag,
        /// Actual decoded function kind.
        kind: FunctionKind,
        /// Required function-kind family.
        requirement: FunctionKindRequirement,
    },
    /// Two packed function flags cannot describe the same function.
    ConflictingFunctionFlags {
        /// First conflicting flag.
        first: FunctionHeaderFlag,
        /// Second conflicting flag.
        second: FunctionHeaderFlag,
    },
    /// A configured stack limit exceeded the pinned structural maximum.
    InvalidStackLimit {
        /// Rejected limit.
        value: u32,
        /// Structural maximum.
        maximum: u32,
    },
    /// Checked instruction decoding failed.
    Decode(DecodeError),
    /// An operand index is outside its function-local domain.
    IndexOutOfBounds {
        /// Operand namespace.
        domain: OperandIndexDomain,
        /// Rejected index.
        index: u32,
        /// Domain length.
        len: u32,
    },
    /// An instruction-specific field contains an unknown value.
    InvalidSecondaryOperand {
        /// Secondary field.
        field: SecondaryOperandField,
        /// Rejected raw value.
        value: u32,
    },
    /// Relative-target arithmetic overflowed.
    ControlFlowTargetOverflow {
        /// Edge being computed.
        edge: ControlFlowEdge,
        /// Target base.
        base: i64,
        /// Signed displacement.
        displacement: i64,
    },
    /// A control-flow target is outside the valid instruction-start set.
    InvalidControlFlowTarget {
        /// Edge being checked.
        edge: ControlFlowEdge,
        /// Computed signed target.
        target: i64,
        /// Complete bytecode length.
        bytecode_len: u32,
        /// Exact rejection reason.
        reason: InvalidControlFlowTargetReason,
    },
    /// An opcode's declared operand format did not provide its required
    /// control-flow displacement.
    MissingControlFlowOperand {
        /// Required format.
        expected: OperandFormat,
    },
    /// This partial verifier deliberately rejects the opcode.
    UnsupportedOpcodeSemantics {
        /// Semantic component still required.
        feature: UnsupportedVerifierFeature,
    },
    /// An opcode is invalid for the enclosing function kind.
    OpcodeNotAllowedForFunctionKind {
        /// Actual enclosing function kind.
        kind: FunctionKind,
        /// Function-kind family required by the opcode.
        requirement: FunctionKindRequirement,
    },
    /// Resolving the opcode's dynamic stack effect exposed a schema error.
    StackEffect(StackEffectError),
    /// An instruction removes more ordinary values than are available.
    StackUnderflow {
        /// Values required by the instruction.
        required: u32,
        /// Values available at entry.
        available: u32,
    },
    /// An instruction's output depth exceeded the configured limit.
    StackLimitExceeded {
        /// Computed output depth.
        depth: u64,
        /// Inclusive configured limit.
        limit: u32,
    },
    /// Two reachable predecessors supply different ordinary-value depths.
    InconsistentStackAtJoin {
        /// Join instruction PC.
        target: BytecodePc,
        /// Previously established depth.
        established_depth: u32,
        /// New incoming depth.
        incoming_depth: u32,
        /// Instruction that supplied the new edge.
        incoming_from: BytecodePc,
    },
    /// Compiler-generated reachable termination left ordinary values behind.
    NonEmptyCompilerExitStack {
        /// Values remaining after the terminal instruction's stack effect.
        remaining: u32,
    },
    /// The stored maximum does not equal the recomputed reachable maximum.
    SerializedStackSizeMismatch {
        /// Serialized or compiler-declared maximum.
        serialized: u32,
        /// Recomputed maximum.
        computed: u32,
    },
}

/// Verifier error with function and instruction context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationError {
    function_path: FunctionPath,
    pc: Option<BytecodePc>,
    opcode: Option<FinalOpcode>,
    kind: VerificationErrorKind,
}

impl VerificationError {
    /// Returns the function path.
    #[must_use]
    pub const fn function_path(&self) -> FunctionPath {
        self.function_path
    }

    /// Returns the relevant bytecode PC.
    #[must_use]
    pub const fn pc(&self) -> Option<BytecodePc> {
        self.pc
    }

    /// Returns the relevant decoded opcode.
    #[must_use]
    pub const fn opcode(&self) -> Option<FinalOpcode> {
        self.opcode
    }

    /// Returns the exact violated invariant.
    #[must_use]
    pub const fn kind(&self) -> &VerificationErrorKind {
        &self.kind
    }

    pub(super) const fn root(kind: VerificationErrorKind) -> Self {
        Self {
            function_path: FunctionPath::ROOT,
            pc: None,
            opcode: None,
            kind,
        }
    }

    pub(super) const fn at_instruction(
        decoded: DecodedInstruction,
        kind: VerificationErrorKind,
    ) -> Self {
        Self {
            function_path: FunctionPath::ROOT,
            pc: Some(decoded.pc()),
            opcode: Some(decoded.instruction().opcode()),
            kind,
        }
    }

    pub(super) fn from_decode(source: DecodeError) -> Self {
        let (pc, opcode) = decode_error_location(source);
        Self {
            function_path: FunctionPath::ROOT,
            pc: Some(pc),
            opcode,
            kind: VerificationErrorKind::Decode(source),
        }
    }
}

impl fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "bytecode verification failed in function {}",
            self.function_path
        )?;
        if let Some(pc) = self.pc {
            write!(formatter, " at PC {pc}")?;
        }
        if let Some(opcode) = self.opcode {
            write!(formatter, " ({opcode})")?;
        }
        write!(formatter, ": {}", self.kind)
    }
}

impl Error for VerificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            VerificationErrorKind::Decode(source) => Some(source),
            VerificationErrorKind::StackEffect(source) => Some(source),
            _ => None,
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "each structured verifier invariant has one explicit display arm"
)]
impl fmt::Display for VerificationErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBytecode => formatter.write_str("the bytecode stream is empty"),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} exceeded by observed value {observed}"
            ),
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                formatter,
                "cannot reserve {requested} additional {resource}"
            ),
            Self::MetadataCountOutOfRange {
                domain,
                value,
                maximum,
            } => write!(
                formatter,
                "{domain} count {value} exceeds structural maximum {maximum}"
            ),
            Self::DisallowedFunctionBits {
                field,
                value,
                allowed_mask,
                disallowed_bits,
            } => write!(
                formatter,
                "{field} value {value:#06x} contains disallowed bits {disallowed_bits:#06x}; allowed mask is {allowed_mask:#06x}"
            ),
            Self::DefinedArgumentCountOutOfRange {
                defined,
                argument_count,
            } => write!(
                formatter,
                "defined argument count {defined} exceeds function argument count {argument_count}"
            ),
            Self::VariableReferenceCountOutOfRange {
                variable_references,
                argument_count,
                local_count,
            } => write!(
                formatter,
                "variable-reference count {variable_references} exceeds {argument_count} arguments plus {local_count} locals"
            ),
            Self::CompilerCaptureCountMismatch {
                variable_references,
                captures,
            } => write!(
                formatter,
                "compiler capture count {captures} does not equal declared variable-reference count {variable_references}"
            ),
            Self::MissingCompilerCaptureLayout {
                variable_references,
            } => write!(
                formatter,
                "compiler function declares {variable_references} variable-reference cells without a capture layout"
            ),
            Self::CompilerConstantCountMismatch { declared, entries } => write!(
                formatter,
                "compiler constant-layout count {entries} does not equal declared constant-pool length {declared}"
            ),
            Self::MissingCompilerConstantLayout { constants } => write!(
                formatter,
                "compiler function declares {constants} constants without a constant-pool type layout"
            ),
            Self::CompilerConstantKindMismatch {
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "compiler constant {index} has kind {actual}, but the opcode requires {expected}"
            ),
            Self::CompilerCaptureIndexOutOfBounds { binding, len } => write!(
                formatter,
                "compiler capture {binding} is outside its frame domain length {len}"
            ),
            Self::DuplicateCompilerCapture { binding } => {
                write!(
                    formatter,
                    "compiler capture {binding} names a duplicate frame binding"
                )
            }
            Self::CompilerMappedArgumentIndexOutOfBounds { index, len } => write!(
                formatter,
                "mapped arguments position {index} is outside argument domain length {len}"
            ),
            Self::CompilerMappedArgumentsNotAscending { previous, index } => write!(
                formatter,
                "mapped arguments positions are not strictly ascending at {previous}, {index}"
            ),
            Self::CloseLocRequiresScopedCapture { local } => write!(
                formatter,
                "close_loc local {local} is not an explicitly scoped compiler capture"
            ),
            Self::FunctionFlagNotAllowedForKind {
                flag,
                kind,
                requirement,
            } => write!(
                formatter,
                "function flag {flag} requires {requirement}, but the decoded function kind is {kind}"
            ),
            Self::ConflictingFunctionFlags { first, second } => {
                write!(formatter, "function flags {first} and {second} conflict")
            }
            Self::InvalidStackLimit { value, maximum } => write!(
                formatter,
                "configured stack-depth limit {value} exceeds structural maximum {maximum}"
            ),
            Self::Decode(source) => write!(formatter, "cannot predecode bytecode: {source}"),
            Self::IndexOutOfBounds { domain, index, len } => write!(
                formatter,
                "{domain} index {index} is outside domain length {len}"
            ),
            Self::InvalidSecondaryOperand { field, value } => {
                write!(formatter, "invalid {field} value {value}")
            }
            Self::ControlFlowTargetOverflow {
                edge,
                base,
                displacement,
            } => write!(
                formatter,
                "{edge} target base {base} plus displacement {displacement} overflows i64"
            ),
            Self::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len,
                reason,
            } => write!(
                formatter,
                "invalid {edge} target {target} for bytecode length {bytecode_len}: {reason}"
            ),
            Self::MissingControlFlowOperand { expected } => write!(
                formatter,
                "control-flow opcode is missing operand format {}",
                expected.upstream_name()
            ),
            Self::UnsupportedOpcodeSemantics { feature } => {
                write!(
                    formatter,
                    "opcode requires unsupported verifier feature {feature}"
                )
            }
            Self::OpcodeNotAllowedForFunctionKind { kind, requirement } => write!(
                formatter,
                "opcode requires {requirement}, but the enclosing function kind is {kind}"
            ),
            Self::StackEffect(source) => {
                write!(
                    formatter,
                    "cannot resolve instruction stack effect: {source}"
                )
            }
            Self::StackUnderflow {
                required,
                available,
            } => write!(
                formatter,
                "stack underflow: instruction requires {required} values, {available} available"
            ),
            Self::StackLimitExceeded { depth, limit } => write!(
                formatter,
                "computed stack depth {depth} exceeds configured limit {limit}"
            ),
            Self::InconsistentStackAtJoin {
                target,
                established_depth,
                incoming_depth,
                incoming_from,
            } => write!(
                formatter,
                "inconsistent stack depth at PC {target}: established {established_depth}, incoming {incoming_depth} from PC {incoming_from}"
            ),
            Self::NonEmptyCompilerExitStack { remaining } => write!(
                formatter,
                "compiler-generated terminal leaves {remaining} ordinary stack values"
            ),
            Self::SerializedStackSizeMismatch {
                serialized,
                computed,
            } => write!(
                formatter,
                "serialized stack size {serialized} does not equal computed size {computed}"
            ),
        }
    }
}

impl fmt::Display for VerificationResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BytecodeBytes => "bytecode bytes",
            Self::Instructions => "instructions",
            Self::Constants => "constants",
            Self::AtomPoolEntries => "atom-pool entries",
            Self::TransferEvaluations => "transfer evaluations",
            Self::StackDepth => "stack entries",
            Self::InstructionBoundaryWords => "instruction-boundary words",
            Self::WorklistEntries => "worklist entries",
            Self::CompilerCaptures => "compiler capture entries",
        })
    }
}

impl fmt::Display for CompilerCapturedBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(index) => write!(formatter, "argument {index}"),
            Self::FunctionLocal(index) => write!(formatter, "function local {index}"),
            Self::ScopedLocal(index) => write!(formatter, "scoped local {index}"),
        }
    }
}

impl fmt::Display for CompilerConstantKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Value => "value",
            Self::Function => "function",
        })
    }
}

impl fmt::Display for FunctionCountDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Arguments => "argument",
            Self::Locals => "local",
            Self::VariableReferences => "variable-reference",
            Self::ClosureVariables => "closure-variable",
            Self::ExpectedStackSize => "expected stack-size",
        })
    }
}

impl fmt::Display for OperandIndexDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AtomPool => "atom-pool",
            Self::ConstantPool => "constant-pool",
            Self::Local => "local",
            Self::Argument => "argument",
            Self::ClosureVariable => "closure-variable",
        })
    }
}

impl fmt::Display for SecondaryOperandField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SpecialObjectKind => "special-object kind",
            Self::RestFirstArgument => "rest first-argument index",
            Self::ApplyMagic => "apply magic",
            Self::ThrowErrorKind => "throw-error kind",
            Self::DefineMethodFlags => "define-method flags",
            Self::DefinePrivateFieldKind => "define-private-field kind",
            Self::DefineClassFlags => "define-class flags",
            Self::IsWith => "is-with flag",
            Self::IteratorCallFlags => "iterator-call flags",
        })
    }
}

impl fmt::Display for ControlFlowEdge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fallthrough => "fallthrough",
            Self::Branch => "branch",
            Self::Jump => "jump",
            Self::CatchHandler => "catch-handler",
            Self::FinallySubroutine => "finally-subroutine",
            Self::FinallyContinuation => "finally-continuation",
            Self::WithBinding => "with-binding",
        })
    }
}

impl fmt::Display for InvalidControlFlowTargetReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutsideBytecode => "target is outside the bytecode",
            Self::NotInstructionBoundary => "target is not an instruction boundary",
            Self::CatchTargetZero => "PC zero is reserved as an iterator-unwind sentinel",
        })
    }
}

impl fmt::Display for UnsupportedVerifierFeature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ConstantPoolTyping => "constant-pool-typing",
            Self::RawFunctionStack => "raw-function-stack",
            Self::EvalScopeMetadata => "eval-scope-metadata",
            Self::CapturedBindingMetadata => "captured-binding-metadata",
            Self::CatchMarkers => "catch-markers",
            Self::FinallyReturnAddresses => "finally-return-addresses",
            Self::WithEnvironmentBranches => "with-environment-branches",
            Self::IteratorMarkers => "iterator-markers",
            Self::PackedStackOffsets => "packed-stack-offsets",
        })
    }
}

fn decode_error_location(error: DecodeError) -> (BytecodePc, Option<FinalOpcode>) {
    match error {
        DecodeError::PcNotRepresentable { pc }
        | DecodeError::PcOutOfBounds { pc, .. }
        | DecodeError::MissingOpcode { pc, .. }
        | DecodeError::InvalidOpcode { pc, .. }
        | DecodeError::NextPcOverflow { pc, .. } => (pc, None),
        DecodeError::TruncatedOperands { pc, opcode, .. }
        | DecodeError::OperandDecoding { pc, opcode, .. } => (pc, Some(opcode)),
    }
}
