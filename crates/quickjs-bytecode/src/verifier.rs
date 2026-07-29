/*
 * QuickJS bytecode control-flow verification
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Fail-closed verification of final bytecode control flow and ordinary
//! JavaScript-value stack depths.
//!
//! This module intentionally produces [`VerifiedControlFlow`], not an
//! execution-authorizing `VerifiedBytecode`. Atom namespaces, constant kinds,
//! function metadata, nested functions, handlers, iterator markers, finally
//! return addresses, and source tables still require later verification.

use std::{collections::VecDeque, error::Error, fmt};

use crate::{
    BytecodePc, DecodeError, DecodedInstruction, FinalOpcode, InstructionDecoder, OperandFormat,
    Operands, StackEffectError,
};

/// Maximum argument, local, closure-reference, or stack count accepted by the
/// pinned `QuickJS` release.
pub const MAX_FUNCTION_INDEX_ENTRIES: u32 = 65_534;

/// Structural maximum operand-stack depth accepted by the pinned `QuickJS`
/// release.
pub const MAX_OPERAND_STACK_DEPTH: u32 = 65_534;

/// Function-local index-domain lengths needed for body verification.
///
/// These counts do not prove that any corresponding pool entry or metadata
/// record is valid. A later whole-function verifier must own and validate the
/// actual pools before execution.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FunctionIndexDomains {
    atom_pool_len: u32,
    constant_pool_len: u32,
    argument_count: u32,
    local_count: u32,
    closure_var_count: u32,
}

impl FunctionIndexDomains {
    /// Creates the index domains for one bytecode function.
    #[must_use]
    pub const fn new(
        atom_pool_len: u32,
        constant_pool_len: u32,
        argument_count: u32,
        local_count: u32,
        closure_var_count: u32,
    ) -> Self {
        Self {
            atom_pool_len,
            constant_pool_len,
            argument_count,
            local_count,
            closure_var_count,
        }
    }

    /// Returns the number of entries in the function-local atom pool.
    #[must_use]
    pub const fn atom_pool_len(self) -> u32 {
        self.atom_pool_len
    }

    /// Returns the number of entries in the constant pool.
    #[must_use]
    pub const fn constant_pool_len(self) -> u32 {
        self.constant_pool_len
    }

    /// Returns the function's argument count.
    #[must_use]
    pub const fn argument_count(self) -> u32 {
        self.argument_count
    }

    /// Returns the function's local-variable count.
    #[must_use]
    pub const fn local_count(self) -> u32 {
        self.local_count
    }

    /// Returns the number of closure-variable entries.
    #[must_use]
    pub const fn closure_var_count(self) -> u32 {
        self.closure_var_count
    }
}

/// Owned bytecode and structural counts that have not been verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedFunctionBody {
    bytecode: Vec<u8>,
    expected_stack_size: u32,
    domains: FunctionIndexDomains,
}

impl UnverifiedFunctionBody {
    /// Creates an unverified body.
    #[must_use]
    pub const fn new(
        bytecode: Vec<u8>,
        expected_stack_size: u32,
        domains: FunctionIndexDomains,
    ) -> Self {
        Self {
            bytecode,
            expected_stack_size,
            domains,
        }
    }

    /// Returns the raw final-bytecode bytes.
    #[must_use]
    pub fn bytecode(&self) -> &[u8] {
        &self.bytecode
    }

    /// Returns the serialized or compiler-declared maximum stack size.
    #[must_use]
    pub const fn expected_stack_size(&self) -> u32 {
        self.expected_stack_size
    }

    /// Returns the declared index-domain lengths.
    #[must_use]
    pub const fn domains(&self) -> FunctionIndexDomains {
        self.domains
    }
}

/// Resource limits for one control-flow verification.
///
/// Every maximum is inclusive. [`VerificationLimits::UNTRUSTED`] uses the
/// provisional untrusted-input profile from `BYTECODE_VERIFIER.md`.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VerificationLimits {
    max_bytecode_bytes_per_function: u32,
    max_instructions_per_function: u32,
    max_constants_per_function: u32,
    max_atom_pool_entries: u32,
    max_transfer_evaluations: u64,
    max_stack_depth: u32,
}

impl VerificationLimits {
    /// Provisional limits for untrusted bytecode.
    pub const UNTRUSTED: Self = Self {
        max_bytecode_bytes_per_function: 16 * 1024 * 1024,
        max_instructions_per_function: 4_194_304,
        max_constants_per_function: 262_144,
        max_atom_pool_entries: 1_048_576,
        max_transfer_evaluations: 33_554_432,
        max_stack_depth: MAX_OPERAND_STACK_DEPTH,
    };

    /// Creates an explicit verification profile.
    #[must_use]
    pub const fn new(
        max_bytecode_bytes_per_function: u32,
        max_instructions_per_function: u32,
        max_constants_per_function: u32,
        max_atom_pool_entries: u32,
        max_transfer_evaluations: u64,
        max_stack_depth: u32,
    ) -> Self {
        Self {
            max_bytecode_bytes_per_function,
            max_instructions_per_function,
            max_constants_per_function,
            max_atom_pool_entries,
            max_transfer_evaluations,
            max_stack_depth,
        }
    }

    /// Returns the per-function bytecode-byte maximum.
    #[must_use]
    pub const fn max_bytecode_bytes_per_function(self) -> u32 {
        self.max_bytecode_bytes_per_function
    }

    /// Returns the per-function instruction maximum.
    #[must_use]
    pub const fn max_instructions_per_function(self) -> u32 {
        self.max_instructions_per_function
    }

    /// Returns the per-function constant count maximum.
    #[must_use]
    pub const fn max_constants_per_function(self) -> u32 {
        self.max_constants_per_function
    }

    /// Returns the function-local atom-pool entry maximum.
    #[must_use]
    pub const fn max_atom_pool_entries(self) -> u32 {
        self.max_atom_pool_entries
    }

    /// Returns the transfer-function evaluation maximum.
    #[must_use]
    pub const fn max_transfer_evaluations(self) -> u64 {
        self.max_transfer_evaluations
    }

    /// Returns the configured stack-depth maximum.
    #[must_use]
    pub const fn max_stack_depth(self) -> u32 {
        self.max_stack_depth
    }
}

impl Default for VerificationLimits {
    fn default() -> Self {
        Self::UNTRUSTED
    }
}

/// An opaque path to a function in a future nested bytecode graph.
///
/// This first verifier slice accepts only a root function. The private
/// representation leaves room for typed child path segments later.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FunctionPath(());

impl FunctionPath {
    /// The root function path.
    pub const ROOT: Self = Self(());

    /// Returns whether this is the root function.
    #[must_use]
    pub const fn is_root(self) -> bool {
        true
    }
}

impl fmt::Display for FunctionPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<root>")
    }
}

/// A validated index into one [`VerifiedControlFlow`] instruction array.
///
/// Construction is private so arbitrary integers cannot be presented as
/// verified instruction identities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct InstructionIndex(u32);

impl InstructionIndex {
    /// Returns the numeric instruction position.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for InstructionIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The statically validated successor shape of an instruction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VerifiedSuccessorKind {
    /// One ordinary fallthrough edge.
    Fallthrough,
    /// A taken branch and a not-taken fallthrough edge.
    Branch,
    /// One unconditional jump edge.
    Jump,
    /// No normal successor.
    Terminate,
}

/// Validated successor instruction indices.
///
/// The representation and constructors are private; instances can only be
/// obtained from a [`VerifiedInstruction`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VerifiedSuccessors(VerifiedSuccessorsRepr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum VerifiedSuccessorsRepr {
    Fallthrough(InstructionIndex),
    Branch {
        taken: InstructionIndex,
        not_taken: InstructionIndex,
    },
    Jump(InstructionIndex),
    Terminate,
}

impl VerifiedSuccessors {
    /// Returns the successor shape.
    #[must_use]
    pub const fn kind(self) -> VerifiedSuccessorKind {
        match self.0 {
            VerifiedSuccessorsRepr::Fallthrough(_) => VerifiedSuccessorKind::Fallthrough,
            VerifiedSuccessorsRepr::Branch { .. } => VerifiedSuccessorKind::Branch,
            VerifiedSuccessorsRepr::Jump(_) => VerifiedSuccessorKind::Jump,
            VerifiedSuccessorsRepr::Terminate => VerifiedSuccessorKind::Terminate,
        }
    }

    /// Returns an ordinary or not-taken fallthrough successor.
    #[must_use]
    pub const fn fallthrough(self) -> Option<InstructionIndex> {
        match self.0 {
            VerifiedSuccessorsRepr::Fallthrough(index)
            | VerifiedSuccessorsRepr::Branch {
                not_taken: index, ..
            } => Some(index),
            VerifiedSuccessorsRepr::Jump(_) | VerifiedSuccessorsRepr::Terminate => None,
        }
    }

    /// Returns the taken target of a conditional branch.
    #[must_use]
    pub const fn branch_target(self) -> Option<InstructionIndex> {
        match self.0 {
            VerifiedSuccessorsRepr::Branch { taken, .. } => Some(taken),
            VerifiedSuccessorsRepr::Fallthrough(_)
            | VerifiedSuccessorsRepr::Jump(_)
            | VerifiedSuccessorsRepr::Terminate => None,
        }
    }

    /// Returns an unconditional jump target.
    #[must_use]
    pub const fn jump_target(self) -> Option<InstructionIndex> {
        match self.0 {
            VerifiedSuccessorsRepr::Jump(index) => Some(index),
            VerifiedSuccessorsRepr::Fallthrough(_)
            | VerifiedSuccessorsRepr::Branch { .. }
            | VerifiedSuccessorsRepr::Terminate => None,
        }
    }
}

/// One decoded instruction with validated successors and an analyzed entry
/// stack depth.
///
/// Construction is private. `None` entry depth denotes a structurally valid
/// but unreachable instruction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VerifiedInstruction {
    decoded: DecodedInstruction,
    entry_stack_depth: Option<u32>,
    successors: VerifiedSuccessors,
}

impl VerifiedInstruction {
    /// Returns the checked decoded instruction.
    #[must_use]
    pub const fn decoded(self) -> DecodedInstruction {
        self.decoded
    }

    /// Returns the analyzed entry stack depth, or `None` when unreachable.
    #[must_use]
    pub const fn entry_stack_depth(self) -> Option<u32> {
        self.entry_stack_depth
    }

    /// Returns the validated successor indices.
    #[must_use]
    pub const fn successors(self) -> VerifiedSuccessors {
        self.successors
    }
}

/// Completely predecoded and ordinary-stack-verified control flow.
///
/// This is deliberately not execution authority. It does not validate actual
/// pool contents, atom namespaces, function metadata, nested functions,
/// internal handler slots, or source tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedControlFlow {
    bytecode: Vec<u8>,
    instructions: Vec<VerifiedInstruction>,
    instruction_start_bitmap: Vec<u64>,
    computed_stack_size: u32,
    domains: FunctionIndexDomains,
}

impl VerifiedControlFlow {
    /// Returns the immutable raw bytecode retained by this certificate.
    #[must_use]
    pub fn bytecode(&self) -> &[u8] {
        &self.bytecode
    }

    /// Returns all verified instructions in bytecode order.
    #[must_use]
    pub fn instructions(&self) -> &[VerifiedInstruction] {
        &self.instructions
    }

    /// Returns the recomputed maximum operand-stack depth.
    #[must_use]
    pub const fn computed_stack_size(&self) -> u32 {
        self.computed_stack_size
    }

    /// Returns the structural index domains against which operands were
    /// checked.
    #[must_use]
    pub const fn domains(&self) -> FunctionIndexDomains {
        self.domains
    }

    /// Returns whether `pc` starts an instruction.
    #[must_use]
    pub fn is_instruction_start(&self, pc: BytecodePc) -> bool {
        is_instruction_start(&self.instruction_start_bitmap, self.bytecode.len(), pc)
    }

    /// Resolves an instruction-start PC to its private validated index.
    #[must_use]
    pub fn instruction_index_at(&self, pc: BytecodePc) -> Option<InstructionIndex> {
        instruction_index_at(
            &self.instructions,
            &self.instruction_start_bitmap,
            self.bytecode.len(),
            pc,
        )
    }

    /// Returns one verified instruction.
    #[must_use]
    pub fn instruction(&self, index: InstructionIndex) -> Option<&VerifiedInstruction> {
        let index = usize::try_from(index.get()).ok()?;
        self.instructions.get(index)
    }
}

/// Resource named by a verifier budget or allocation error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VerificationResource {
    /// Encoded bytes in one function.
    BytecodeBytes,
    /// Decoded instructions in one function.
    Instructions,
    /// Constants in one function.
    Constants,
    /// Entries in one function-local atom pool.
    AtomPoolEntries,
    /// Abstract transfer-function evaluations.
    TransferEvaluations,
    /// Operand-stack entries.
    StackDepth,
    /// Instruction-start bitmap words.
    InstructionBoundaryWords,
    /// Worklist entries.
    WorklistEntries,
}

/// A function metadata count subject to a structural maximum.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionCountDomain {
    /// Function arguments.
    Arguments,
    /// Function locals.
    Locals,
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
    /// Function-kind and suspension-point validation.
    FunctionKindAndSuspension,
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

    const fn root(kind: VerificationErrorKind) -> Self {
        Self {
            function_path: FunctionPath::ROOT,
            pc: None,
            opcode: None,
            kind,
        }
    }

    const fn at_instruction(decoded: DecodedInstruction, kind: VerificationErrorKind) -> Self {
        Self {
            function_path: FunctionPath::ROOT,
            pc: Some(decoded.pc()),
            opcode: Some(decoded.instruction().opcode()),
            kind,
        }
    }

    fn from_decode(source: DecodeError) -> Self {
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
        })
    }
}

impl fmt::Display for FunctionCountDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Arguments => "argument",
            Self::Locals => "local",
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
            Self::FunctionKindAndSuspension => "function-kind-and-suspension",
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

/// Completely predecodes and verifies the currently supported ordinary-value
/// control-flow subset.
///
/// Predecoding always consumes the complete instruction stream before opcode
/// capability errors are considered. The returned value is not executable
/// bytecode: whole-function metadata and pool validation remain mandatory.
///
/// # Errors
///
/// Returns a contextual [`VerificationError`] for malformed encoding, limits,
/// allocation failure, invalid operands or targets, unsupported semantics,
/// stack underflow, inconsistent joins, or a stored-stack-size mismatch.
pub fn verify_control_flow(
    body: UnverifiedFunctionBody,
    limits: VerificationLimits,
) -> Result<VerifiedControlFlow, VerificationError> {
    validate_limits_and_counts(&body, limits)?;

    if body.bytecode.is_empty() {
        return Err(VerificationError::root(
            VerificationErrorKind::EmptyBytecode,
        ));
    }

    let mut instruction_start_bitmap = allocate_boundary_bitmap(body.bytecode.len())?;
    let decoded = predecode_complete(
        &body.bytecode,
        &mut instruction_start_bitmap,
        limits.max_instructions_per_function,
    )?;
    let mut instructions = validate_static_semantics(
        &decoded,
        &instruction_start_bitmap,
        body.bytecode.len(),
        body.domains,
    )?;
    let computed_stack_size = analyze_ordinary_stack(&mut instructions, limits)?;

    if computed_stack_size != body.expected_stack_size {
        return Err(VerificationError::root(
            VerificationErrorKind::SerializedStackSizeMismatch {
                serialized: body.expected_stack_size,
                computed: computed_stack_size,
            },
        ));
    }

    Ok(VerifiedControlFlow {
        bytecode: body.bytecode,
        instructions,
        instruction_start_bitmap,
        computed_stack_size,
        domains: body.domains,
    })
}

fn validate_limits_and_counts(
    body: &UnverifiedFunctionBody,
    limits: VerificationLimits,
) -> Result<(), VerificationError> {
    if limits.max_stack_depth > MAX_OPERAND_STACK_DEPTH {
        return Err(VerificationError::root(
            VerificationErrorKind::InvalidStackLimit {
                value: limits.max_stack_depth,
                maximum: MAX_OPERAND_STACK_DEPTH,
            },
        ));
    }

    check_limit(
        VerificationResource::BytecodeBytes,
        usize_to_u64(body.bytecode.len()),
        u64::from(limits.max_bytecode_bytes_per_function),
    )?;
    check_limit(
        VerificationResource::Constants,
        u64::from(body.domains.constant_pool_len),
        u64::from(limits.max_constants_per_function),
    )?;
    check_limit(
        VerificationResource::AtomPoolEntries,
        u64::from(body.domains.atom_pool_len),
        u64::from(limits.max_atom_pool_entries),
    )?;

    check_structural_count(FunctionCountDomain::Arguments, body.domains.argument_count)?;
    check_structural_count(FunctionCountDomain::Locals, body.domains.local_count)?;
    check_structural_count(
        FunctionCountDomain::ClosureVariables,
        body.domains.closure_var_count,
    )?;
    check_structural_count(
        FunctionCountDomain::ExpectedStackSize,
        body.expected_stack_size,
    )?;
    check_limit(
        VerificationResource::StackDepth,
        u64::from(body.expected_stack_size),
        u64::from(limits.max_stack_depth),
    )
}

fn check_structural_count(
    domain: FunctionCountDomain,
    value: u32,
) -> Result<(), VerificationError> {
    if value > MAX_FUNCTION_INDEX_ENTRIES {
        return Err(VerificationError::root(
            VerificationErrorKind::MetadataCountOutOfRange {
                domain,
                value,
                maximum: MAX_FUNCTION_INDEX_ENTRIES,
            },
        ));
    }
    Ok(())
}

fn check_limit(
    resource: VerificationResource,
    observed: u64,
    limit: u64,
) -> Result<(), VerificationError> {
    if observed > limit {
        return Err(VerificationError::root(
            VerificationErrorKind::LimitExceeded {
                resource,
                limit,
                observed,
            },
        ));
    }
    Ok(())
}

fn allocate_boundary_bitmap(bytecode_len: usize) -> Result<Vec<u64>, VerificationError> {
    let word_count = bytecode_len.checked_add(63).ok_or_else(|| {
        VerificationError::root(VerificationErrorKind::AllocationFailed {
            resource: VerificationResource::InstructionBoundaryWords,
            requested: u64::MAX,
        })
    })? / 64;
    let mut bitmap = Vec::new();
    bitmap.try_reserve_exact(word_count).map_err(|_| {
        VerificationError::root(VerificationErrorKind::AllocationFailed {
            resource: VerificationResource::InstructionBoundaryWords,
            requested: usize_to_u64(word_count),
        })
    })?;
    bitmap.resize(word_count, 0);
    Ok(bitmap)
}

fn predecode_complete(
    bytecode: &[u8],
    instruction_start_bitmap: &mut [u64],
    max_instructions: u32,
) -> Result<Vec<DecodedInstruction>, VerificationError> {
    let mut instructions = Vec::new();
    let instruction_stream = InstructionDecoder::new(bytecode);

    for item in instruction_stream {
        let decoded = item.map_err(VerificationError::from_decode)?;
        let observed = usize_to_u64(instructions.len()).saturating_add(1);
        if observed > u64::from(max_instructions) {
            return Err(VerificationError::at_instruction(
                decoded,
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::Instructions,
                    limit: u64::from(max_instructions),
                    observed,
                },
            ));
        }
        mark_instruction_start(instruction_start_bitmap, bytecode.len(), decoded.pc())?;
        if instructions.len() == instructions.capacity() {
            instructions.try_reserve(1).map_err(|_| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::AllocationFailed {
                        resource: VerificationResource::Instructions,
                        requested: 1,
                    },
                )
            })?;
        }
        instructions.push(decoded);
    }

    Ok(instructions)
}

fn validate_static_semantics(
    decoded: &[DecodedInstruction],
    instruction_start_bitmap: &[u64],
    bytecode_len: usize,
    domains: FunctionIndexDomains,
) -> Result<Vec<VerifiedInstruction>, VerificationError> {
    let mut verified = Vec::new();
    verified.try_reserve_exact(decoded.len()).map_err(|_| {
        VerificationError::root(VerificationErrorKind::AllocationFailed {
            resource: VerificationResource::Instructions,
            requested: usize_to_u64(decoded.len()),
        })
    })?;

    for &current in decoded {
        validate_operand_indices(current, domains)?;
        validate_secondary_operands(current, domains)?;

        let target =
            resolve_relative_target(current, decoded, instruction_start_bitmap, bytecode_len)?;
        validate_gosub_continuation(current, decoded, instruction_start_bitmap, bytecode_len)?;

        let semantics = opcode_semantics(current.instruction().opcode());
        let successors = resolve_static_successors(
            current,
            semantics,
            target,
            decoded,
            instruction_start_bitmap,
            bytecode_len,
        )?;

        verified.push(VerifiedInstruction {
            decoded: current,
            entry_stack_depth: None,
            successors,
        });
    }

    for instruction in &verified {
        let decoded = instruction.decoded;
        if let OpcodeSemantics::Unsupported(feature, _) =
            opcode_semantics(decoded.instruction().opcode())
        {
            return Err(VerificationError::at_instruction(
                decoded,
                VerificationErrorKind::UnsupportedOpcodeSemantics { feature },
            ));
        }
    }

    Ok(verified)
}

fn resolve_static_successors(
    current: DecodedInstruction,
    semantics: OpcodeSemantics,
    target: Option<InstructionIndex>,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<VerifiedSuccessors, VerificationError> {
    let Some(shape) = semantics.successor_shape() else {
        return Err(VerificationError::at_instruction(
            current,
            VerificationErrorKind::Decode(DecodeError::InvalidOpcode {
                pc: current.pc(),
                opcode_byte: FinalOpcode::Invalid.encoded_byte(),
                source: crate::FinalOpcodeDecodeError::ReservedInvalid,
            }),
        ));
    };

    match shape {
        SuccessorShape::Fallthrough => Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Fallthrough(
            resolve_fallthrough(current, instructions, bitmap, bytecode_len)?,
        ))),
        SuccessorShape::Branch => {
            let taken = require_encoded_target(current, target)?;
            let not_taken = resolve_fallthrough(current, instructions, bitmap, bytecode_len)?;
            Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Branch {
                taken,
                not_taken,
            }))
        }
        SuccessorShape::Jump => Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Jump(
            require_encoded_target(current, target)?,
        ))),
        SuccessorShape::Terminate => Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Terminate)),
    }
}

fn require_encoded_target(
    decoded: DecodedInstruction,
    target: Option<InstructionIndex>,
) -> Result<InstructionIndex, VerificationError> {
    target.ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::MissingControlFlowOperand {
                expected: decoded.instruction().opcode().metadata().operand_format(),
            },
        )
    })
}

fn mark_instruction_start(
    bitmap: &mut [u64],
    bytecode_len: usize,
    pc: BytecodePc,
) -> Result<(), VerificationError> {
    let offset = usize::try_from(pc.get())
        .map_err(|_| VerificationError::from_decode(DecodeError::PcNotRepresentable { pc }))?;
    let word = bitmap.get_mut(offset / 64).ok_or_else(|| {
        VerificationError::from_decode(DecodeError::PcOutOfBounds { pc, bytecode_len })
    })?;
    *word |= 1_u64 << (offset % 64);
    Ok(())
}

fn is_instruction_start(bitmap: &[u64], bytecode_len: usize, pc: BytecodePc) -> bool {
    let Ok(offset) = usize::try_from(pc.get()) else {
        return false;
    };
    if offset >= bytecode_len {
        return false;
    }
    bitmap
        .get(offset / 64)
        .is_some_and(|word| word & (1_u64 << (offset % 64)) != 0)
}

fn decoded_instruction_index_at(
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
    pc: BytecodePc,
) -> Option<InstructionIndex> {
    if !is_instruction_start(bitmap, bytecode_len, pc) {
        return None;
    }
    let index = instructions
        .binary_search_by_key(&pc, |instruction| instruction.pc())
        .ok()?;
    Some(InstructionIndex(u32::try_from(index).ok()?))
}

fn instruction_index_at(
    instructions: &[VerifiedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
    pc: BytecodePc,
) -> Option<InstructionIndex> {
    if !is_instruction_start(bitmap, bytecode_len, pc) {
        return None;
    }
    let index = instructions
        .binary_search_by_key(&pc, |instruction| instruction.decoded.pc())
        .ok()?;
    Some(InstructionIndex(u32::try_from(index).ok()?))
}

#[allow(clippy::too_many_lines)]
fn validate_operand_indices(
    decoded: DecodedInstruction,
    domains: FunctionIndexDomains,
) -> Result<(), VerificationError> {
    let instruction = decoded.instruction();
    let operands = instruction.operands();

    if let Some(index) = operands.atom_pool_index() {
        validate_index(
            decoded,
            OperandIndexDomain::AtomPool,
            index.get(),
            domains.atom_pool_len,
        )?;
    }

    match operands {
        Operands::Const(index) => validate_index(
            decoded,
            OperandIndexDomain::ConstantPool,
            index,
            domains.constant_pool_len,
        )?,
        Operands::Const8(index) => validate_index(
            decoded,
            OperandIndexDomain::ConstantPool,
            u32::from(index),
            domains.constant_pool_len,
        )?,
        Operands::Loc(index) => validate_index(
            decoded,
            OperandIndexDomain::Local,
            u32::from(index),
            domains.local_count,
        )?,
        Operands::Loc8(index) => validate_index(
            decoded,
            OperandIndexDomain::Local,
            u32::from(index),
            domains.local_count,
        )?,
        Operands::Arg(index) => validate_index(
            decoded,
            OperandIndexDomain::Argument,
            u32::from(index),
            domains.argument_count,
        )?,
        Operands::VarRef(index) => validate_index(
            decoded,
            OperandIndexDomain::ClosureVariable,
            u32::from(index),
            domains.closure_var_count,
        )?,
        Operands::AtomU16 { value, .. } => match instruction.opcode() {
            FinalOpcode::MakeLocRef => validate_index(
                decoded,
                OperandIndexDomain::Local,
                u32::from(value),
                domains.local_count,
            )?,
            FinalOpcode::MakeArgRef => validate_index(
                decoded,
                OperandIndexDomain::Argument,
                u32::from(value),
                domains.argument_count,
            )?,
            FinalOpcode::MakeVarRefRef => validate_index(
                decoded,
                OperandIndexDomain::ClosureVariable,
                u32::from(value),
                domains.closure_var_count,
            )?,
            _ => {}
        },
        Operands::NoneLoc => {
            let index = implied_local_index(instruction.opcode()).ok_or_else(|| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::MissingControlFlowOperand {
                        expected: OperandFormat::NoneLoc,
                    },
                )
            })?;
            validate_index(
                decoded,
                OperandIndexDomain::Local,
                index,
                domains.local_count,
            )?;
        }
        Operands::NoneArg => {
            let index = implied_argument_index(instruction.opcode()).ok_or_else(|| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::MissingControlFlowOperand {
                        expected: OperandFormat::NoneArg,
                    },
                )
            })?;
            validate_index(
                decoded,
                OperandIndexDomain::Argument,
                index,
                domains.argument_count,
            )?;
        }
        Operands::NoneVarRef => {
            let index = implied_closure_variable_index(instruction.opcode()).ok_or_else(|| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::MissingControlFlowOperand {
                        expected: OperandFormat::NoneVarRef,
                    },
                )
            })?;
            validate_index(
                decoded,
                OperandIndexDomain::ClosureVariable,
                index,
                domains.closure_var_count,
            )?;
        }
        Operands::None
        | Operands::NoneInt
        | Operands::U8(_)
        | Operands::I8(_)
        | Operands::Label8(_)
        | Operands::U16(_)
        | Operands::I16(_)
        | Operands::Label16(_)
        | Operands::NPop { .. }
        | Operands::NPopX
        | Operands::NPopU16 { .. }
        | Operands::U32(_)
        | Operands::I32(_)
        | Operands::Label(_)
        | Operands::Atom(_)
        | Operands::AtomU8 { .. }
        | Operands::AtomLabelU8 { .. }
        | Operands::AtomLabelU16 { .. }
        | Operands::LabelU16 { .. } => {}
    }

    Ok(())
}

fn validate_index(
    decoded: DecodedInstruction,
    domain: OperandIndexDomain,
    index: u32,
    len: u32,
) -> Result<(), VerificationError> {
    if index >= len {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::IndexOutOfBounds { domain, index, len },
        ));
    }
    Ok(())
}

fn implied_local_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetLoc0 | FinalOpcode::PutLoc0 | FinalOpcode::SetLoc0 => Some(0),
        FinalOpcode::GetLoc1 | FinalOpcode::PutLoc1 | FinalOpcode::SetLoc1 => Some(1),
        FinalOpcode::GetLoc2 | FinalOpcode::PutLoc2 | FinalOpcode::SetLoc2 => Some(2),
        FinalOpcode::GetLoc3 | FinalOpcode::PutLoc3 | FinalOpcode::SetLoc3 => Some(3),
        _ => None,
    }
}

fn implied_argument_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetArg0 | FinalOpcode::PutArg0 | FinalOpcode::SetArg0 => Some(0),
        FinalOpcode::GetArg1 | FinalOpcode::PutArg1 | FinalOpcode::SetArg1 => Some(1),
        FinalOpcode::GetArg2 | FinalOpcode::PutArg2 | FinalOpcode::SetArg2 => Some(2),
        FinalOpcode::GetArg3 | FinalOpcode::PutArg3 | FinalOpcode::SetArg3 => Some(3),
        _ => None,
    }
}

fn implied_closure_variable_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetVarRef0 | FinalOpcode::PutVarRef0 | FinalOpcode::SetVarRef0 => Some(0),
        FinalOpcode::GetVarRef1 | FinalOpcode::PutVarRef1 | FinalOpcode::SetVarRef1 => Some(1),
        FinalOpcode::GetVarRef2 | FinalOpcode::PutVarRef2 | FinalOpcode::SetVarRef2 => Some(2),
        FinalOpcode::GetVarRef3 | FinalOpcode::PutVarRef3 | FinalOpcode::SetVarRef3 => Some(3),
        _ => None,
    }
}

fn validate_secondary_operands(
    decoded: DecodedInstruction,
    domains: FunctionIndexDomains,
) -> Result<(), VerificationError> {
    let instruction = decoded.instruction();
    let invalid = |field, value| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidSecondaryOperand { field, value },
        )
    };

    match (instruction.opcode(), instruction.operands()) {
        (FinalOpcode::SpecialObject, Operands::U8(value)) if value > 6 => Err(invalid(
            SecondaryOperandField::SpecialObjectKind,
            u32::from(value),
        )),
        (FinalOpcode::Rest, Operands::U16(value)) if u32::from(value) > domains.argument_count => {
            Err(invalid(
                SecondaryOperandField::RestFirstArgument,
                u32::from(value),
            ))
        }
        (FinalOpcode::Apply, Operands::U16(value)) if value > 2 => {
            Err(invalid(SecondaryOperandField::ApplyMagic, u32::from(value)))
        }
        (FinalOpcode::ThrowError, Operands::AtomU8 { value, .. }) if value > 4 => Err(invalid(
            SecondaryOperandField::ThrowErrorKind,
            u32::from(value),
        )),
        (FinalOpcode::DefineMethod, Operands::AtomU8 { value, .. })
        | (FinalOpcode::DefineMethodComputed, Operands::U8(value))
            if value & !0b111 != 0 || value & 0b11 == 0b11 =>
        {
            Err(invalid(
                SecondaryOperandField::DefineMethodFlags,
                u32::from(value),
            ))
        }
        (
            FinalOpcode::DefineClass | FinalOpcode::DefineClassComputed,
            Operands::AtomU8 { value, .. },
        ) if value > 1 => Err(invalid(
            SecondaryOperandField::DefineClassFlags,
            u32::from(value),
        )),
        (
            FinalOpcode::WithGetVar
            | FinalOpcode::WithPutVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef,
            Operands::AtomLabelU8 { value, .. },
        ) if value > 1 => Err(invalid(SecondaryOperandField::IsWith, u32::from(value))),
        (FinalOpcode::IteratorCall, Operands::U8(value)) if value > 2 => Err(invalid(
            SecondaryOperandField::IteratorCallFlags,
            u32::from(value),
        )),
        _ => Ok(()),
    }
}

fn resolve_relative_target(
    decoded: DecodedInstruction,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<Option<InstructionIndex>, VerificationError> {
    let Some((edge, base_delta, displacement)) = relative_target_spec(decoded)? else {
        return Ok(None);
    };
    let pc = i64::from(decoded.pc().get());
    let base = pc.checked_add(base_delta).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::ControlFlowTargetOverflow {
                edge,
                base: pc,
                displacement: base_delta,
            },
        )
    })?;
    let target = base.checked_add(displacement).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::ControlFlowTargetOverflow {
                edge,
                base,
                displacement,
            },
        )
    })?;
    resolve_target(decoded, edge, target, instructions, bitmap, bytecode_len).map(Some)
}

fn relative_target_spec(
    decoded: DecodedInstruction,
) -> Result<Option<(ControlFlowEdge, i64, i64)>, VerificationError> {
    let instruction = decoded.instruction();
    let opcode = instruction.opcode();
    let operands = instruction.operands();

    let missing = |expected| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::MissingControlFlowOperand { expected },
        )
    };

    match opcode {
        FinalOpcode::IfFalse8 | FinalOpcode::IfTrue8 => match operands {
            Operands::Label8(displacement) => {
                Ok(Some((ControlFlowEdge::Branch, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label8)),
        },
        FinalOpcode::IfFalse | FinalOpcode::IfTrue => match operands {
            Operands::Label(displacement) => {
                Ok(Some((ControlFlowEdge::Branch, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::Goto8 => match operands {
            Operands::Label8(displacement) => {
                Ok(Some((ControlFlowEdge::Jump, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label8)),
        },
        FinalOpcode::Goto16 => match operands {
            Operands::Label16(displacement) => {
                Ok(Some((ControlFlowEdge::Jump, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label16)),
        },
        FinalOpcode::Goto => match operands {
            Operands::Label(displacement) => {
                Ok(Some((ControlFlowEdge::Jump, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::Catch => match operands {
            Operands::Label(displacement) => Ok(Some((
                ControlFlowEdge::CatchHandler,
                1,
                i64::from(displacement),
            ))),
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::Gosub => match operands {
            Operands::Label(displacement) => Ok(Some((
                ControlFlowEdge::FinallySubroutine,
                1,
                i64::from(displacement),
            ))),
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::WithGetVar
        | FinalOpcode::WithPutVar
        | FinalOpcode::WithDeleteVar
        | FinalOpcode::WithMakeRef
        | FinalOpcode::WithGetRef => match operands {
            Operands::AtomLabelU8 { label, .. } => {
                Ok(Some((ControlFlowEdge::WithBinding, 5, i64::from(label))))
            }
            _ => Err(missing(OperandFormat::AtomLabelU8)),
        },
        _ => Ok(None),
    }
}

fn validate_gosub_continuation(
    decoded: DecodedInstruction,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<(), VerificationError> {
    if decoded.instruction().opcode() != FinalOpcode::Gosub {
        return Ok(());
    }
    resolve_target(
        decoded,
        ControlFlowEdge::FinallyContinuation,
        i64::from(decoded.next_pc().get()),
        instructions,
        bitmap,
        bytecode_len,
    )?;
    Ok(())
}

fn resolve_fallthrough(
    decoded: DecodedInstruction,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<InstructionIndex, VerificationError> {
    resolve_target(
        decoded,
        ControlFlowEdge::Fallthrough,
        i64::from(decoded.next_pc().get()),
        instructions,
        bitmap,
        bytecode_len,
    )
}

fn resolve_target(
    decoded: DecodedInstruction,
    edge: ControlFlowEdge,
    target: i64,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<InstructionIndex, VerificationError> {
    let bytecode_len_u32 = u32::try_from(bytecode_len).map_err(|_| {
        VerificationError::root(VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::BytecodeBytes,
            limit: u64::from(u32::MAX),
            observed: usize_to_u64(bytecode_len),
        })
    })?;

    if target < 0 || target >= i64::from(bytecode_len_u32) {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::OutsideBytecode,
            },
        ));
    }
    if decoded.instruction().opcode() == FinalOpcode::Catch && target == 0 {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::CatchTargetZero,
            },
        ));
    }

    let target_pc = BytecodePc::new(u32::try_from(target).map_err(|_| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::OutsideBytecode,
            },
        )
    })?);
    decoded_instruction_index_at(instructions, bitmap, bytecode_len, target_pc).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::NotInstructionBoundary,
            },
        )
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpcodeSemantics {
    Invalid,
    Ordinary,
    Conditional,
    Jump,
    Terminate,
    Unsupported(UnsupportedVerifierFeature, SuccessorShape),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SuccessorShape {
    Fallthrough,
    Branch,
    Jump,
    Terminate,
}

impl OpcodeSemantics {
    const fn successor_shape(self) -> Option<SuccessorShape> {
        match self {
            Self::Invalid => None,
            Self::Ordinary => Some(SuccessorShape::Fallthrough),
            Self::Conditional => Some(SuccessorShape::Branch),
            Self::Jump => Some(SuccessorShape::Jump),
            Self::Terminate => Some(SuccessorShape::Terminate),
            Self::Unsupported(_, shape) => Some(shape),
        }
    }
}

#[allow(clippy::too_many_lines)]
const fn opcode_semantics(opcode: FinalOpcode) -> OpcodeSemantics {
    match opcode {
        FinalOpcode::Invalid => OpcodeSemantics::Invalid,

        FinalOpcode::IfFalse
        | FinalOpcode::IfTrue
        | FinalOpcode::IfFalse8
        | FinalOpcode::IfTrue8 => OpcodeSemantics::Conditional,

        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16 => OpcodeSemantics::Jump,

        FinalOpcode::TailCall
        | FinalOpcode::TailCallMethod
        | FinalOpcode::Return
        | FinalOpcode::ReturnUndef
        | FinalOpcode::Throw
        | FinalOpcode::ThrowError => OpcodeSemantics::Terminate,

        FinalOpcode::PushConst
        | FinalOpcode::FClosure
        | FinalOpcode::PushConst8
        | FinalOpcode::FClosure8 => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::ConstantPoolTyping,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::DefineClass | FinalOpcode::DefineClassComputed => {
            OpcodeSemantics::Unsupported(
                UnsupportedVerifierFeature::RawFunctionStack,
                SuccessorShape::Fallthrough,
            )
        }

        FinalOpcode::Eval | FinalOpcode::ApplyEval => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::EvalScopeMetadata,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::CloseLoc
        | FinalOpcode::MakeLocRef
        | FinalOpcode::MakeArgRef
        | FinalOpcode::MakeVarRefRef
        | FinalOpcode::MakeVarRef => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::CapturedBindingMetadata,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::Catch => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::CatchMarkers,
            SuccessorShape::Branch,
        ),

        FinalOpcode::NipCatch => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::CatchMarkers,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::Gosub => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::FinallyReturnAddresses,
            SuccessorShape::Jump,
        ),

        FinalOpcode::Ret => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::FinallyReturnAddresses,
            SuccessorShape::Terminate,
        ),

        FinalOpcode::WithGetVar
        | FinalOpcode::WithPutVar
        | FinalOpcode::WithDeleteVar
        | FinalOpcode::WithMakeRef
        | FinalOpcode::WithGetRef => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::WithEnvironmentBranches,
            SuccessorShape::Branch,
        ),

        FinalOpcode::ForOfStart
        | FinalOpcode::ForAwaitOfStart
        | FinalOpcode::ForOfNext
        | FinalOpcode::ForAwaitOfNext
        | FinalOpcode::IteratorGetValueDone
        | FinalOpcode::IteratorClose
        | FinalOpcode::IteratorNext
        | FinalOpcode::IteratorCall => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::IteratorMarkers,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::ReturnAsync => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::FunctionKindAndSuspension,
            SuccessorShape::Terminate,
        ),

        FinalOpcode::InitialYield
        | FinalOpcode::Yield
        | FinalOpcode::YieldStar
        | FinalOpcode::AsyncYieldStar
        | FinalOpcode::Await => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::FunctionKindAndSuspension,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::CopyDataProperties => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::PackedStackOffsets,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::PushI32
        | FinalOpcode::PushAtomValue
        | FinalOpcode::PrivateSymbol
        | FinalOpcode::Undefined
        | FinalOpcode::Null
        | FinalOpcode::PushThis
        | FinalOpcode::PushFalse
        | FinalOpcode::PushTrue
        | FinalOpcode::Object
        | FinalOpcode::SpecialObject
        | FinalOpcode::Rest
        | FinalOpcode::Drop
        | FinalOpcode::Nip
        | FinalOpcode::Nip1
        | FinalOpcode::Dup
        | FinalOpcode::Dup1
        | FinalOpcode::Dup2
        | FinalOpcode::Dup3
        | FinalOpcode::Insert2
        | FinalOpcode::Insert3
        | FinalOpcode::Insert4
        | FinalOpcode::Perm3
        | FinalOpcode::Perm4
        | FinalOpcode::Perm5
        | FinalOpcode::Swap
        | FinalOpcode::Swap2
        | FinalOpcode::Rot3l
        | FinalOpcode::Rot3r
        | FinalOpcode::Rot4l
        | FinalOpcode::Rot5l
        | FinalOpcode::CallConstructor
        | FinalOpcode::Call
        | FinalOpcode::CallMethod
        | FinalOpcode::ArrayFrom
        | FinalOpcode::Apply
        | FinalOpcode::CheckCtorReturn
        | FinalOpcode::CheckCtor
        | FinalOpcode::InitCtor
        | FinalOpcode::CheckBrand
        | FinalOpcode::AddBrand
        | FinalOpcode::RegExp
        | FinalOpcode::GetSuper
        | FinalOpcode::Import
        | FinalOpcode::GetVarUndef
        | FinalOpcode::GetVar
        | FinalOpcode::PutVar
        | FinalOpcode::PutVarInit
        | FinalOpcode::GetRefValue
        | FinalOpcode::PutRefValue
        | FinalOpcode::GetField
        | FinalOpcode::GetField2
        | FinalOpcode::PutField
        | FinalOpcode::GetPrivateField
        | FinalOpcode::PutPrivateField
        | FinalOpcode::DefinePrivateField
        | FinalOpcode::GetArrayEl
        | FinalOpcode::GetArrayEl2
        | FinalOpcode::GetArrayEl3
        | FinalOpcode::PutArrayEl
        | FinalOpcode::GetSuperValue
        | FinalOpcode::PutSuperValue
        | FinalOpcode::DefineField
        | FinalOpcode::SetName
        | FinalOpcode::SetNameComputed
        | FinalOpcode::SetProto
        | FinalOpcode::SetHomeObject
        | FinalOpcode::DefineArrayEl
        | FinalOpcode::Append
        | FinalOpcode::DefineMethod
        | FinalOpcode::DefineMethodComputed
        | FinalOpcode::GetLoc
        | FinalOpcode::PutLoc
        | FinalOpcode::SetLoc
        | FinalOpcode::GetArg
        | FinalOpcode::PutArg
        | FinalOpcode::SetArg
        | FinalOpcode::GetVarRef
        | FinalOpcode::PutVarRef
        | FinalOpcode::SetVarRef
        | FinalOpcode::SetLocUninitialized
        | FinalOpcode::GetLocCheck
        | FinalOpcode::PutLocCheck
        | FinalOpcode::SetLocCheck
        | FinalOpcode::PutLocCheckInit
        | FinalOpcode::GetLocCheckThis
        | FinalOpcode::GetVarRefCheck
        | FinalOpcode::PutVarRefCheck
        | FinalOpcode::PutVarRefCheckInit
        | FinalOpcode::ToObject
        | FinalOpcode::ToPropKey
        | FinalOpcode::ForInStart
        | FinalOpcode::ForInNext
        | FinalOpcode::IteratorCheckObject
        | FinalOpcode::Neg
        | FinalOpcode::Plus
        | FinalOpcode::Dec
        | FinalOpcode::Inc
        | FinalOpcode::PostDec
        | FinalOpcode::PostInc
        | FinalOpcode::DecLoc
        | FinalOpcode::IncLoc
        | FinalOpcode::AddLoc
        | FinalOpcode::Not
        | FinalOpcode::Lnot
        | FinalOpcode::Typeof
        | FinalOpcode::Delete
        | FinalOpcode::DeleteVar
        | FinalOpcode::Mul
        | FinalOpcode::Div
        | FinalOpcode::Mod
        | FinalOpcode::Add
        | FinalOpcode::Sub
        | FinalOpcode::Pow
        | FinalOpcode::Shl
        | FinalOpcode::Sar
        | FinalOpcode::Shr
        | FinalOpcode::Lt
        | FinalOpcode::Lte
        | FinalOpcode::Gt
        | FinalOpcode::Gte
        | FinalOpcode::InstanceOf
        | FinalOpcode::In
        | FinalOpcode::Eq
        | FinalOpcode::Neq
        | FinalOpcode::StrictEq
        | FinalOpcode::StrictNeq
        | FinalOpcode::And
        | FinalOpcode::Xor
        | FinalOpcode::Or
        | FinalOpcode::IsUndefinedOrNull
        | FinalOpcode::PrivateIn
        | FinalOpcode::PushBigIntI32
        | FinalOpcode::Nop
        | FinalOpcode::PushMinus1
        | FinalOpcode::Push0
        | FinalOpcode::Push1
        | FinalOpcode::Push2
        | FinalOpcode::Push3
        | FinalOpcode::Push4
        | FinalOpcode::Push5
        | FinalOpcode::Push6
        | FinalOpcode::Push7
        | FinalOpcode::PushI8
        | FinalOpcode::PushI16
        | FinalOpcode::PushEmptyString
        | FinalOpcode::GetLoc8
        | FinalOpcode::PutLoc8
        | FinalOpcode::SetLoc8
        | FinalOpcode::GetLoc0
        | FinalOpcode::GetLoc1
        | FinalOpcode::GetLoc2
        | FinalOpcode::GetLoc3
        | FinalOpcode::PutLoc0
        | FinalOpcode::PutLoc1
        | FinalOpcode::PutLoc2
        | FinalOpcode::PutLoc3
        | FinalOpcode::SetLoc0
        | FinalOpcode::SetLoc1
        | FinalOpcode::SetLoc2
        | FinalOpcode::SetLoc3
        | FinalOpcode::GetArg0
        | FinalOpcode::GetArg1
        | FinalOpcode::GetArg2
        | FinalOpcode::GetArg3
        | FinalOpcode::PutArg0
        | FinalOpcode::PutArg1
        | FinalOpcode::PutArg2
        | FinalOpcode::PutArg3
        | FinalOpcode::SetArg0
        | FinalOpcode::SetArg1
        | FinalOpcode::SetArg2
        | FinalOpcode::SetArg3
        | FinalOpcode::GetVarRef0
        | FinalOpcode::GetVarRef1
        | FinalOpcode::GetVarRef2
        | FinalOpcode::GetVarRef3
        | FinalOpcode::PutVarRef0
        | FinalOpcode::PutVarRef1
        | FinalOpcode::PutVarRef2
        | FinalOpcode::PutVarRef3
        | FinalOpcode::SetVarRef0
        | FinalOpcode::SetVarRef1
        | FinalOpcode::SetVarRef2
        | FinalOpcode::SetVarRef3
        | FinalOpcode::GetLength
        | FinalOpcode::Call0
        | FinalOpcode::Call1
        | FinalOpcode::Call2
        | FinalOpcode::Call3
        | FinalOpcode::IsUndefined
        | FinalOpcode::IsNull
        | FinalOpcode::TypeofIsUndefined
        | FinalOpcode::TypeofIsFunction => OpcodeSemantics::Ordinary,
    }
}

#[allow(clippy::too_many_lines)]
fn analyze_ordinary_stack(
    instructions: &mut [VerifiedInstruction],
    limits: VerificationLimits,
) -> Result<u32, VerificationError> {
    let Some(entry) = instructions.first_mut() else {
        return Err(VerificationError::root(
            VerificationErrorKind::EmptyBytecode,
        ));
    };
    entry.entry_stack_depth = Some(0);

    let mut worklist = VecDeque::new();
    reserve_worklist_entry(&mut worklist, entry.decoded)?;
    worklist.push_back(InstructionIndex(0));

    let mut computed_max = 0_u32;
    let mut evaluations = 0_u64;

    while let Some(index) = worklist.pop_front() {
        let position = usize::try_from(index.get()).map_err(|_| {
            VerificationError::root(VerificationErrorKind::LimitExceeded {
                resource: VerificationResource::Instructions,
                limit: u64::from(u32::MAX),
                observed: u64::from(index.get()),
            })
        })?;
        let Some(current) = instructions.get(position).copied() else {
            return Err(VerificationError::root(
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::Instructions,
                    limit: usize_to_u64(instructions.len()),
                    observed: u64::from(index.get()) + 1,
                },
            ));
        };
        let Some(entry_depth) = current.entry_stack_depth else {
            continue;
        };

        evaluations = evaluations.checked_add(1).ok_or_else(|| {
            VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::TransferEvaluations,
                    limit: limits.max_transfer_evaluations,
                    observed: u64::MAX,
                },
            )
        })?;
        if evaluations > limits.max_transfer_evaluations {
            return Err(VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::TransferEvaluations,
                    limit: limits.max_transfer_evaluations,
                    observed: evaluations,
                },
            ));
        }

        let effect = current
            .decoded
            .instruction()
            .stack_effect()
            .map_err(|source| {
                VerificationError::at_instruction(
                    current.decoded,
                    VerificationErrorKind::StackEffect(source),
                )
            })?;
        if entry_depth < effect.pops() {
            return Err(VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::StackUnderflow {
                    required: effect.pops(),
                    available: entry_depth,
                },
            ));
        }
        let output_depth = u64::from(entry_depth - effect.pops()) + u64::from(effect.pushes());
        if output_depth > u64::from(limits.max_stack_depth) {
            return Err(VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::StackLimitExceeded {
                    depth: output_depth,
                    limit: limits.max_stack_depth,
                },
            ));
        }
        let output_depth = u32::try_from(output_depth).map_err(|_| {
            VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::StackLimitExceeded {
                    depth: output_depth,
                    limit: limits.max_stack_depth,
                },
            )
        })?;
        computed_max = computed_max.max(output_depth);

        match current.successors.0 {
            VerifiedSuccessorsRepr::Fallthrough(successor)
            | VerifiedSuccessorsRepr::Jump(successor) => propagate_stack_depth(
                instructions,
                &mut worklist,
                successor,
                output_depth,
                current.decoded,
            )?,
            VerifiedSuccessorsRepr::Branch { taken, not_taken } => {
                propagate_stack_depth(
                    instructions,
                    &mut worklist,
                    taken,
                    output_depth,
                    current.decoded,
                )?;
                propagate_stack_depth(
                    instructions,
                    &mut worklist,
                    not_taken,
                    output_depth,
                    current.decoded,
                )?;
            }
            VerifiedSuccessorsRepr::Terminate => {}
        }
    }

    Ok(computed_max)
}

fn propagate_stack_depth(
    instructions: &mut [VerifiedInstruction],
    worklist: &mut VecDeque<InstructionIndex>,
    target: InstructionIndex,
    incoming_depth: u32,
    source: DecodedInstruction,
) -> Result<(), VerificationError> {
    let position = usize::try_from(target.get()).map_err(|_| {
        VerificationError::at_instruction(
            source,
            VerificationErrorKind::LimitExceeded {
                resource: VerificationResource::Instructions,
                limit: u64::from(u32::MAX),
                observed: u64::from(target.get()),
            },
        )
    })?;
    let Some(target_instruction) = instructions.get_mut(position) else {
        return Err(VerificationError::at_instruction(
            source,
            VerificationErrorKind::LimitExceeded {
                resource: VerificationResource::Instructions,
                limit: usize_to_u64(instructions.len()),
                observed: u64::from(target.get()) + 1,
            },
        ));
    };

    match target_instruction.entry_stack_depth {
        None => {
            reserve_worklist_entry(worklist, source)?;
            target_instruction.entry_stack_depth = Some(incoming_depth);
            worklist.push_back(target);
        }
        Some(established_depth) if established_depth == incoming_depth => {}
        Some(established_depth) => {
            return Err(VerificationError::at_instruction(
                source,
                VerificationErrorKind::InconsistentStackAtJoin {
                    target: target_instruction.decoded.pc(),
                    established_depth,
                    incoming_depth,
                    incoming_from: source.pc(),
                },
            ));
        }
    }
    Ok(())
}

fn reserve_worklist_entry(
    worklist: &mut VecDeque<InstructionIndex>,
    decoded: DecodedInstruction,
) -> Result<(), VerificationError> {
    if worklist.len() == worklist.capacity() {
        worklist.try_reserve(1).map_err(|_| {
            VerificationError::at_instruction(
                decoded,
                VerificationErrorKind::AllocationFailed {
                    resource: VerificationResource::WorklistEntries,
                    requested: 1,
                },
            )
        })?;
    }
    Ok(())
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{OpcodeSemantics, opcode_semantics};
    use crate::{ALL_FINAL_OPCODES, FinalOpcode};

    #[test]
    fn final_opcode_capability_partition_is_exhaustive_and_counted_from_the_table() {
        let mut invalid = 0;
        let mut supported = 0;
        let mut unsupported = 0;

        for &opcode in ALL_FINAL_OPCODES {
            match opcode_semantics(opcode) {
                OpcodeSemantics::Invalid => invalid += 1,
                OpcodeSemantics::Unsupported(_, _) => unsupported += 1,
                OpcodeSemantics::Ordinary
                | OpcodeSemantics::Conditional
                | OpcodeSemantics::Jump
                | OpcodeSemantics::Terminate => supported += 1,
            }
        }

        assert_eq!(invalid, 1);
        assert_eq!(supported, 206);
        assert_eq!(unsupported, 37);
        assert_eq!(invalid + supported + unsupported, ALL_FINAL_OPCODES.len());
        assert_eq!(
            opcode_semantics(FinalOpcode::Invalid),
            OpcodeSemantics::Invalid
        );
    }
}
