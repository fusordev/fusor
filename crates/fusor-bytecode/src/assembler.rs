//! Typed symbolic labels and final branch relaxation.
//!
//! [`BytecodeBuilder`](crate::BytecodeBuilder) intentionally accepts only
//! final relative displacements. This module owns the compiler phase before
//! it: labels are assembler-scoped, every label must be bound exactly once,
//! and supported branches are relaxed to the shortest final `QuickJS` form
//! before bytes or instruction PCs are produced.

use std::{error::Error, fmt, sync::Arc};

use crate::{
    AtomPoolIndex, BytecodeBuilder, BytecodePc, EncodeError, FinalOpcode, Instruction,
    InstructionError, OperandFormat, Operands,
};

#[derive(Debug)]
struct AssemblerIdentity;

/// A symbolic instruction-boundary label issued by one [`BytecodeAssembler`].
///
/// The owner identity is private and compared by `Arc` identity, so a label
/// from another assembler cannot silently select a same-numbered label.
#[derive(Clone)]
pub struct AssemblerLabel {
    identity: Arc<AssemblerIdentity>,
    index: u32,
}

impl fmt::Debug for AssemblerLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AssemblerLabel")
            .field(&self.index)
            .finish()
    }
}

/// A symbolic branch family supported by final branch relaxation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BranchKind {
    /// Pop a condition and branch when it converts to false.
    IfFalse,
    /// Pop a condition and branch when it converts to true.
    IfTrue,
    /// Unconditionally branch.
    Goto,
    /// Install an exception handler at the symbolic target.
    Catch,
    /// Enter a finally subroutine at the symbolic target.
    Gosub,
}

impl BranchKind {
    const fn long_opcode(self) -> FinalOpcode {
        match self {
            Self::IfFalse => FinalOpcode::IfFalse,
            Self::IfTrue => FinalOpcode::IfTrue,
            Self::Goto => FinalOpcode::Goto,
            Self::Catch => FinalOpcode::Catch,
            Self::Gosub => FinalOpcode::Gosub,
        }
    }

    const fn supports_width(self, instruction_size: u8) -> bool {
        match self {
            Self::IfFalse | Self::IfTrue => matches!(instruction_size, 2 | 5),
            Self::Goto => matches!(instruction_size, 2 | 3 | 5),
            Self::Catch | Self::Gosub => instruction_size == 5,
        }
    }

    const fn minimum_size(self) -> u8 {
        match self {
            Self::IfFalse | Self::IfTrue | Self::Goto => 2,
            Self::Catch | Self::Gosub => 5,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum AssemblyItem {
    Instruction(Instruction),
    Branch {
        kind: BranchKind,
        label_index: u32,
    },
    WithBranch {
        opcode: FinalOpcode,
        atom: AtomPoolIndex,
        value: u8,
        label_index: u32,
    },
}

#[derive(Clone, Copy)]
enum BranchTargetState {
    Unknown,
    Resolving,
    Terminal(u32),
    Cycle,
}

impl AssemblyItem {
    fn minimum_size(self) -> u8 {
        match self {
            Self::Instruction(instruction) => instruction.encoded_size(),
            Self::Branch { kind, .. } => kind.minimum_size(),
            Self::WithBranch { .. } => 10,
        }
    }
}

/// Inclusive resource limits for one symbolic assembly.
///
/// One relaxation evaluation is charged for every planned instruction in
/// each fixed-point pass. Branchless plans perform no relaxation evaluations.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssemblerLimits {
    max_bytecode_bytes: u32,
    max_instructions: u32,
    max_relaxation_evaluations: u64,
}

impl AssemblerLimits {
    /// Creates an explicit symbolic-assembly resource profile.
    #[must_use]
    pub const fn new(
        max_bytecode_bytes: u32,
        max_instructions: u32,
        max_relaxation_evaluations: u64,
    ) -> Self {
        Self {
            max_bytecode_bytes,
            max_instructions,
            max_relaxation_evaluations,
        }
    }

    /// Returns the final encoded-byte maximum.
    #[must_use]
    pub const fn max_bytecode_bytes(self) -> u32 {
        self.max_bytecode_bytes
    }

    /// Returns the planned-instruction maximum.
    #[must_use]
    pub const fn max_instructions(self) -> u32 {
        self.max_instructions
    }

    /// Returns the branch-relaxation evaluation maximum.
    #[must_use]
    pub const fn max_relaxation_evaluations(self) -> u64 {
        self.max_relaxation_evaluations
    }
}

impl Default for AssemblerLimits {
    fn default() -> Self {
        Self::new(u32::MAX, u32::MAX, u64::MAX)
    }
}

/// A symbolic-assembly resource with an inclusive configured limit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AssemblerResource {
    /// Planned final instructions.
    Instructions,
    /// Instruction visits across branch-relaxation passes.
    RelaxationEvaluations,
}

impl fmt::Display for AssemblerResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Instructions => "assembler instructions",
            Self::RelaxationEvaluations => "branch-relaxation evaluations",
        })
    }
}

/// Final bytecode plus the relocated PC of every emitted instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssembledBytecode {
    bytecode: Vec<u8>,
    instruction_pcs: Vec<BytecodePc>,
}

impl AssembledBytecode {
    /// Returns the final encoded instruction stream.
    #[must_use]
    pub fn bytecode(&self) -> &[u8] {
        &self.bytecode
    }

    /// Returns one final PC per source assembler instruction.
    #[must_use]
    pub fn instruction_pcs(&self) -> &[BytecodePc] {
        &self.instruction_pcs
    }

    /// Consumes the assembly and returns its final bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytecode
    }

    /// Consumes the assembly and returns both owned components.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Vec<BytecodePc>) {
        (self.bytecode, self.instruction_pcs)
    }
}

/// A symbolic-label or final-encoding failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssemblerError {
    /// A configured symbolic-assembly resource limit was exceeded.
    LimitExceeded {
        /// Limited resource.
        resource: AssemblerResource,
        /// First planned instruction denied by the inclusive limit.
        instruction_index: u32,
        /// Inclusive configured limit.
        limit: u64,
        /// First value beyond the configured limit.
        observed: u64,
    },
    /// The assembler cannot represent another label identity.
    TooManyLabels,
    /// The assembler cannot represent another instruction identity.
    TooManyInstructions,
    /// Reserving owned assembler storage failed.
    AllocationFailed {
        /// Stable storage category.
        resource: &'static str,
        /// Number of additional entries requested.
        requested: u64,
    },
    /// A label was issued by another assembler.
    ForeignLabel,
    /// An internally referenced label identity does not exist.
    UnknownLabel {
        /// Plan-local label number.
        label_index: u32,
    },
    /// A label was bound more than once.
    DuplicateLabel {
        /// Plan-local label number.
        label_index: u32,
    },
    /// A label was allocated but never bound.
    UnboundLabel {
        /// Plan-local label number.
        label_index: u32,
    },
    /// A referenced label points after the final instruction.
    TargetAtEnd {
        /// Plan-local label number.
        label_index: u32,
    },
    /// Raw relative operands cannot bypass symbolic label validation.
    SymbolicBranchRequired {
        /// Rejected final opcode.
        opcode: FinalOpcode,
    },
    /// An ordinary final instruction was structurally invalid.
    InvalidInstruction {
        /// Zero-based assembler instruction number.
        instruction_index: u32,
        /// Exact instruction validation failure.
        source: InstructionError,
    },
    /// The relaxed stream cannot fit the final PC domain.
    EncodedLengthOutOfRange {
        /// Required final byte count.
        encoded_bytes: u64,
    },
    /// A branch cannot encode the distance to its label.
    BranchDisplacementOutOfRange {
        /// Zero-based assembler instruction number.
        instruction_index: u32,
        /// Plan-local label number.
        label_index: u32,
        /// Signed displacement relative to the first operand byte.
        displacement: i64,
    },
    /// Final instruction encoding failed.
    Encoding {
        /// Zero-based assembler instruction number.
        instruction_index: u32,
        /// Exact final encoder failure.
        source: EncodeError,
    },
    /// The relaxed layout and final encoder disagreed about a PC.
    LayoutMismatch {
        /// Zero-based assembler instruction number.
        instruction_index: u32,
        /// PC derived by branch relaxation.
        expected: BytecodePc,
        /// PC returned by the final encoder.
        actual: BytecodePc,
    },
}

impl AssemblerError {
    /// Returns the stable assembler instruction associated with this failure.
    #[must_use]
    pub const fn instruction_index(&self) -> Option<u32> {
        match self {
            Self::LimitExceeded {
                instruction_index, ..
            }
            | Self::InvalidInstruction {
                instruction_index, ..
            }
            | Self::BranchDisplacementOutOfRange {
                instruction_index, ..
            }
            | Self::Encoding {
                instruction_index, ..
            }
            | Self::LayoutMismatch {
                instruction_index, ..
            } => Some(*instruction_index),
            Self::TooManyLabels
            | Self::TooManyInstructions
            | Self::AllocationFailed { .. }
            | Self::ForeignLabel
            | Self::UnknownLabel { .. }
            | Self::DuplicateLabel { .. }
            | Self::UnboundLabel { .. }
            | Self::TargetAtEnd { .. }
            | Self::SymbolicBranchRequired { .. }
            | Self::EncodedLengthOutOfRange { .. } => None,
        }
    }

    /// Returns the stable plan-local label associated with this failure.
    #[must_use]
    pub const fn label_index(&self) -> Option<u32> {
        match self {
            Self::UnknownLabel { label_index }
            | Self::DuplicateLabel { label_index }
            | Self::UnboundLabel { label_index }
            | Self::TargetAtEnd { label_index }
            | Self::BranchDisplacementOutOfRange { label_index, .. } => Some(*label_index),
            Self::LimitExceeded { .. }
            | Self::TooManyLabels
            | Self::TooManyInstructions
            | Self::AllocationFailed { .. }
            | Self::ForeignLabel
            | Self::SymbolicBranchRequired { .. }
            | Self::InvalidInstruction { .. }
            | Self::EncodedLengthOutOfRange { .. }
            | Self::Encoding { .. }
            | Self::LayoutMismatch { .. } => None,
        }
    }
}

impl fmt::Display for AssemblerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded {
                resource,
                instruction_index,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} exceeded at assembler instruction {instruction_index} by observed value {observed}"
            ),
            Self::TooManyLabels => formatter.write_str("too many symbolic labels"),
            Self::TooManyInstructions => formatter.write_str("too many assembler instructions"),
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                formatter,
                "failed to reserve {requested} additional {resource} entries"
            ),
            Self::ForeignLabel => formatter.write_str("label belongs to another assembler"),
            Self::UnknownLabel { label_index } => {
                write!(formatter, "unknown assembler label {label_index}")
            }
            Self::DuplicateLabel { label_index } => {
                write!(formatter, "assembler label {label_index} was bound twice")
            }
            Self::UnboundLabel { label_index } => {
                write!(formatter, "assembler label {label_index} was never bound")
            }
            Self::TargetAtEnd { label_index } => write!(
                formatter,
                "assembler label {label_index} targets the end of the instruction stream"
            ),
            Self::SymbolicBranchRequired { opcode } => {
                write!(
                    formatter,
                    "opcode {opcode} requires a symbolic assembler label"
                )
            }
            Self::InvalidInstruction {
                instruction_index,
                source,
            } => write!(
                formatter,
                "invalid assembler instruction {instruction_index}: {source}"
            ),
            Self::EncodedLengthOutOfRange { encoded_bytes } => write!(
                formatter,
                "assembled bytecode length {encoded_bytes} exceeds the final PC domain"
            ),
            Self::BranchDisplacementOutOfRange {
                instruction_index,
                label_index,
                displacement,
            } => write!(
                formatter,
                "branch instruction {instruction_index} cannot reach label {label_index} with displacement {displacement}"
            ),
            Self::Encoding {
                instruction_index,
                source,
            } => write!(
                formatter,
                "failed to encode assembler instruction {instruction_index}: {source}"
            ),
            Self::LayoutMismatch {
                instruction_index,
                expected,
                actual,
            } => write!(
                formatter,
                "assembler instruction {instruction_index} expected PC {expected}, encoded at PC {actual}"
            ),
        }
    }
}

impl Error for AssemblerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidInstruction { source, .. } => Some(source),
            Self::Encoding { source, .. } => Some(source),
            Self::LimitExceeded { .. }
            | Self::TooManyLabels
            | Self::TooManyInstructions
            | Self::AllocationFailed { .. }
            | Self::ForeignLabel
            | Self::UnknownLabel { .. }
            | Self::DuplicateLabel { .. }
            | Self::UnboundLabel { .. }
            | Self::TargetAtEnd { .. }
            | Self::SymbolicBranchRequired { .. }
            | Self::EncodedLengthOutOfRange { .. }
            | Self::BranchDisplacementOutOfRange { .. }
            | Self::LayoutMismatch { .. } => None,
        }
    }
}

/// A typed symbolic-label plan that resolves to final `QuickJS` branches.
#[derive(Debug)]
pub struct BytecodeAssembler {
    identity: Arc<AssemblerIdentity>,
    items: Vec<AssemblyItem>,
    label_bindings: Vec<Option<usize>>,
    limits: AssemblerLimits,
}

impl BytecodeAssembler {
    /// Creates an empty assembler with the full integer-domain limits.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(AssemblerLimits::default())
    }

    /// Creates an empty assembler with a final encoded-byte limit and full
    /// integer-domain instruction and relaxation limits.
    #[must_use]
    pub fn with_byte_limit(byte_limit: u32) -> Self {
        Self::with_limits(AssemblerLimits::new(byte_limit, u32::MAX, u64::MAX))
    }

    /// Creates an empty assembler with explicit inclusive resource limits.
    #[must_use]
    pub fn with_limits(limits: AssemblerLimits) -> Self {
        Self {
            identity: Arc::new(AssemblerIdentity),
            items: Vec::new(),
            label_bindings: Vec::new(),
            limits,
        }
    }

    /// Allocates a new assembler-owned symbolic label.
    ///
    /// # Errors
    ///
    /// Returns a capacity or allocation failure without modifying the label
    /// table.
    pub fn new_label(&mut self) -> Result<AssemblerLabel, AssemblerError> {
        let index =
            u32::try_from(self.label_bindings.len()).map_err(|_| AssemblerError::TooManyLabels)?;
        self.label_bindings
            .try_reserve(1)
            .map_err(|_| AssemblerError::AllocationFailed {
                resource: "label",
                requested: 1,
            })?;
        self.label_bindings.push(None);
        Ok(AssemblerLabel {
            identity: Arc::clone(&self.identity),
            index,
        })
    }

    /// Binds a symbolic label to the next emitted instruction.
    ///
    /// Multiple distinct labels may name the same instruction.
    ///
    /// # Errors
    ///
    /// Rejects foreign, unknown, or already-bound labels.
    pub fn bind(&mut self, label: &AssemblerLabel) -> Result<(), AssemblerError> {
        let instruction_position = self.items.len();
        let binding = self.resolve_label_mut(label)?;
        if binding.is_some() {
            return Err(AssemblerError::DuplicateLabel {
                label_index: label.index,
            });
        }
        *binding = Some(instruction_position);
        Ok(())
    }

    /// Appends one non-symbolic final instruction to the plan.
    ///
    /// # Errors
    ///
    /// Rejects invalid opcode/operand pairs, raw label operands, instruction
    /// capacity exhaustion, and allocation failure.
    pub fn push(&mut self, opcode: FinalOpcode, operands: Operands) -> Result<u32, AssemblerError> {
        let instruction_index = self.next_instruction_index()?;
        let instruction = Instruction::new(opcode, operands).map_err(|source| {
            AssemblerError::InvalidInstruction {
                instruction_index,
                source,
            }
        })?;
        if contains_symbolic_label(instruction.opcode().metadata().operand_format()) {
            return Err(AssemblerError::SymbolicBranchRequired { opcode });
        }
        self.push_item(AssemblyItem::Instruction(instruction))?;
        Ok(instruction_index)
    }

    /// Appends a symbolic conditional, unconditional, or exception-handler branch.
    ///
    /// # Errors
    ///
    /// Rejects foreign or unknown labels, instruction capacity exhaustion,
    /// and allocation failure.
    pub fn branch(
        &mut self,
        kind: BranchKind,
        target: &AssemblerLabel,
    ) -> Result<u32, AssemblerError> {
        self.resolve_label(target)?;
        let instruction_index = self.next_instruction_index()?;
        self.push_item(AssemblyItem::Branch {
            kind,
            label_index: target.index,
        })?;
        Ok(instruction_index)
    }

    /// Appends a symbolic `with_*` object-environment branch.
    ///
    /// These final opcodes retain `QuickJS`'s fixed-width `atom_label_u8`
    /// encoding, whose displacement base is the label field at `pc + 5`
    /// rather than the ordinary branch base at `pc + 1`.
    ///
    /// # Errors
    ///
    /// Rejects non-`with_*` opcodes, foreign or unknown labels, instruction
    /// capacity exhaustion, and allocation failure.
    pub fn with_branch(
        &mut self,
        opcode: FinalOpcode,
        atom: AtomPoolIndex,
        value: u8,
        target: &AssemblerLabel,
    ) -> Result<u32, AssemblerError> {
        self.resolve_label(target)?;
        let instruction_index = self.next_instruction_index()?;
        if !matches!(
            opcode,
            FinalOpcode::WithGetVar
                | FinalOpcode::WithPutVar
                | FinalOpcode::WithDeleteVar
                | FinalOpcode::WithMakeRef
                | FinalOpcode::WithGetRef
        ) {
            return Err(AssemblerError::InvalidInstruction {
                instruction_index,
                source: InstructionError::OperandFormatMismatch {
                    opcode,
                    expected: opcode.metadata().operand_format(),
                    actual: OperandFormat::AtomLabelU8,
                },
            });
        }
        Instruction::new(
            opcode,
            Operands::AtomLabelU8 {
                atom,
                label: 0,
                value,
            },
        )
        .map_err(|source| AssemblerError::InvalidInstruction {
            instruction_index,
            source,
        })?;
        self.push_item(AssemblyItem::WithBranch {
            opcode,
            atom,
            value,
            label_index: target.index,
        })?;
        Ok(instruction_index)
    }

    /// Resolves labels, relaxes branches, and emits final bytecode.
    ///
    /// Branch displacement is always `target_pc - (opcode_pc + 1)`, matching
    /// the pinned `QuickJS` final encoding. Relaxation starts from every
    /// branch's shortest form and widens monotonically to the least valid
    /// fixed point. Forward candidates account for bytes added by their own
    /// wider operand.
    ///
    /// # Errors
    ///
    /// Rejects missing/invalid targets, configured instruction or relaxation
    /// limits, unrepresentable layout or displacement, configured byte
    /// limits, final encoding failures, and allocation failure.
    pub fn finish(mut self) -> Result<AssembledBytecode, AssemblerError> {
        self.validate_bound_labels()?;
        self.thread_redundant_goto_targets()?;
        let (widths, positions) = self.relaxed_layout()?;
        let encoded_bytes = *positions
            .last()
            .ok_or(AssemblerError::EncodedLengthOutOfRange { encoded_bytes: 0 })?;
        if encoded_bytes > u64::from(u32::MAX) {
            return Err(AssemblerError::EncodedLengthOutOfRange { encoded_bytes });
        }

        let mut builder = BytecodeBuilder::with_byte_limit(self.limits.max_bytecode_bytes());
        let mut instruction_pcs = Vec::new();
        instruction_pcs
            .try_reserve_exact(self.items.len())
            .map_err(|_| AssemblerError::AllocationFailed {
                resource: "instruction PC",
                requested: usize_to_u64(self.items.len()),
            })?;

        for (position, item) in self.items.iter().copied().enumerate() {
            let instruction_index =
                u32::try_from(position).map_err(|_| AssemblerError::TooManyInstructions)?;
            let expected = BytecodePc::new(
                u32::try_from(positions[position])
                    .map_err(|_| AssemblerError::EncodedLengthOutOfRange { encoded_bytes })?,
            );
            let instruction = match item {
                AssemblyItem::Instruction(instruction) => instruction,
                AssemblyItem::Branch { kind, label_index } => {
                    let target_position = self.target_position(label_index)?;
                    let displacement =
                        signed_displacement(positions[position], positions[target_position])?;
                    branch_instruction(
                        kind,
                        widths[position],
                        displacement,
                        instruction_index,
                        label_index,
                    )?
                }
                AssemblyItem::WithBranch {
                    opcode,
                    atom,
                    value,
                    label_index,
                } => {
                    let target_position = self.target_position(label_index)?;
                    let displacement = with_branch_displacement(
                        positions[position],
                        positions[target_position],
                        instruction_index,
                        label_index,
                    )?;
                    Instruction::new(
                        opcode,
                        Operands::AtomLabelU8 {
                            atom,
                            label: displacement,
                            value,
                        },
                    )
                    .map_err(|source| AssemblerError::InvalidInstruction {
                        instruction_index,
                        source,
                    })?
                }
            };
            let actual = builder.push_instruction(instruction).map_err(|source| {
                AssemblerError::Encoding {
                    instruction_index,
                    source,
                }
            })?;
            if actual != expected {
                return Err(AssemblerError::LayoutMismatch {
                    instruction_index,
                    expected,
                    actual,
                });
            }
            instruction_pcs.push(actual);
        }

        Ok(AssembledBytecode {
            bytecode: builder.into_bytes(),
            instruction_pcs,
        })
    }

    fn next_instruction_index(&self) -> Result<u32, AssemblerError> {
        let instruction_index =
            u32::try_from(self.items.len()).map_err(|_| AssemblerError::TooManyInstructions)?;
        let observed = u64::from(instruction_index) + 1;
        let limit = self.limits.max_instructions();
        if observed > u64::from(limit) {
            return Err(AssemblerError::LimitExceeded {
                resource: AssemblerResource::Instructions,
                instruction_index,
                limit: u64::from(limit),
                observed,
            });
        }
        Ok(instruction_index)
    }

    fn push_item(&mut self, item: AssemblyItem) -> Result<(), AssemblerError> {
        self.items
            .try_reserve(1)
            .map_err(|_| AssemblerError::AllocationFailed {
                resource: "instruction",
                requested: 1,
            })?;
        self.items.push(item);
        Ok(())
    }

    fn resolve_label(&self, label: &AssemblerLabel) -> Result<Option<usize>, AssemblerError> {
        if !Arc::ptr_eq(&self.identity, &label.identity) {
            return Err(AssemblerError::ForeignLabel);
        }
        self.label_bindings
            .get(label.index as usize)
            .copied()
            .ok_or(AssemblerError::UnknownLabel {
                label_index: label.index,
            })
    }

    fn resolve_label_mut(
        &mut self,
        label: &AssemblerLabel,
    ) -> Result<&mut Option<usize>, AssemblerError> {
        if !Arc::ptr_eq(&self.identity, &label.identity) {
            return Err(AssemblerError::ForeignLabel);
        }
        self.label_bindings
            .get_mut(label.index as usize)
            .ok_or(AssemblerError::UnknownLabel {
                label_index: label.index,
            })
    }

    fn validate_bound_labels(&self) -> Result<(), AssemblerError> {
        for (index, binding) in self.label_bindings.iter().enumerate() {
            if binding.is_none() {
                return Err(AssemblerError::UnboundLabel {
                    label_index: u32::try_from(index).map_err(|_| AssemblerError::TooManyLabels)?,
                });
            }
        }
        for item in &self.items {
            if let AssemblyItem::Branch { label_index, .. }
            | AssemblyItem::WithBranch { label_index, .. } = item
            {
                let target = self.target_position(*label_index)?;
                if target == self.items.len() {
                    return Err(AssemblerError::TargetAtEnd {
                        label_index: *label_index,
                    });
                }
            }
        }
        Ok(())
    }

    /// Canonicalizes ordinary branch destinations before branch relaxation.
    ///
    /// A label whose first instruction is an unconditional `goto` has no
    /// observable effect of its own. Conditional and unconditional branches
    /// may therefore target the final destination directly. This preserves
    /// every planned instruction (and consequently compiler source mappings
    /// and statement-stack anchors) while avoiding redundant VM transfers in
    /// branch-only basic blocks. Exception-handler and finally-subroutine
    /// entries deliberately retain their exact targets.
    fn thread_redundant_goto_targets(&mut self) -> Result<(), AssemblerError> {
        let label_count = self.label_bindings.len();
        let requested = usize_to_u64(label_count);
        let mut states = Vec::new();
        states
            .try_reserve_exact(label_count)
            .map_err(|_| AssemblerError::AllocationFailed {
                resource: "branch target state",
                requested,
            })?;
        states.resize(label_count, BranchTargetState::Unknown);

        let mut path = Vec::new();
        path.try_reserve_exact(label_count)
            .map_err(|_| AssemblerError::AllocationFailed {
                resource: "branch target path",
                requested,
            })?;

        for start in 0..label_count {
            if !matches!(states[start], BranchTargetState::Unknown) {
                continue;
            }
            let mut current = start;
            loop {
                match states[current] {
                    BranchTargetState::Unknown => {
                        states[current] = BranchTargetState::Resolving;
                        path.push(current);
                        let Some(next) = self.label_goto_target(current) else {
                            let terminal = u32::try_from(current)
                                .map_err(|_| AssemblerError::TooManyLabels)?;
                            for label in path.drain(..) {
                                states[label] = BranchTargetState::Terminal(terminal);
                            }
                            break;
                        };
                        current = next as usize;
                    }
                    BranchTargetState::Resolving | BranchTargetState::Cycle => {
                        for label in path.drain(..) {
                            states[label] = BranchTargetState::Cycle;
                        }
                        break;
                    }
                    BranchTargetState::Terminal(terminal) => {
                        for label in path.drain(..) {
                            states[label] = BranchTargetState::Terminal(terminal);
                        }
                        break;
                    }
                }
            }
        }

        for instruction_index in 0..self.items.len() {
            let AssemblyItem::Branch { kind, label_index } = self.items[instruction_index] else {
                continue;
            };
            if !matches!(
                kind,
                BranchKind::IfFalse | BranchKind::IfTrue | BranchKind::Goto
            ) {
                continue;
            }

            let BranchTargetState::Terminal(target) = states[label_index as usize] else {
                continue;
            };
            if target != label_index {
                self.items[instruction_index] = AssemblyItem::Branch {
                    kind,
                    label_index: target,
                };
            }
        }
        Ok(())
    }

    fn label_goto_target(&self, label_index: usize) -> Option<u32> {
        let position = self.label_bindings.get(label_index).copied().flatten()?;
        let AssemblyItem::Branch {
            kind: BranchKind::Goto,
            label_index: target,
        } = self.items.get(position)?
        else {
            return None;
        };
        Some(*target)
    }

    fn target_position(&self, label_index: u32) -> Result<usize, AssemblerError> {
        self.label_bindings
            .get(label_index as usize)
            .copied()
            .flatten()
            .ok_or(AssemblerError::UnknownLabel { label_index })
    }

    fn relaxed_layout(&self) -> Result<(Vec<u8>, Vec<u64>), AssemblerError> {
        let mut widths = Vec::new();
        widths.try_reserve_exact(self.items.len()).map_err(|_| {
            AssemblerError::AllocationFailed {
                resource: "instruction width",
                requested: usize_to_u64(self.items.len()),
            }
        })?;
        widths.extend(self.items.iter().map(|item| item.minimum_size()));

        let mut positions = Vec::new();
        positions
            .try_reserve_exact(widths.len().saturating_add(1))
            .map_err(|_| AssemblerError::AllocationFailed {
                resource: "instruction position",
                requested: usize_to_u64(widths.len()).saturating_add(1),
            })?;
        if !self
            .items
            .iter()
            .any(|item| matches!(item, AssemblyItem::Branch { .. }))
        {
            populate_instruction_positions(&widths, &mut positions)?;
            return Ok((widths, positions));
        }

        let mut relaxation_evaluations = 0_u64;
        loop {
            relaxation_evaluations = self.charge_relaxation_pass(relaxation_evaluations)?;
            populate_instruction_positions(&widths, &mut positions)?;
            let mut changed = false;
            for (position, item) in self.items.iter().copied().enumerate() {
                let AssemblyItem::Branch { kind, label_index } = item else {
                    continue;
                };
                let target_position = self.target_position(label_index)?;
                let desired = required_branch_size(
                    kind,
                    widths[position],
                    position,
                    target_position,
                    &positions,
                    u32::try_from(position).map_err(|_| AssemblerError::TooManyInstructions)?,
                    label_index,
                )?;
                if desired > widths[position] {
                    widths[position] = desired;
                    changed = true;
                }
            }
            if !changed {
                return Ok((widths, positions));
            }
        }
    }

    fn charge_relaxation_pass(&self, completed: u64) -> Result<u64, AssemblerError> {
        let requested = usize_to_u64(self.items.len());
        let limit = self.limits.max_relaxation_evaluations();
        let remaining = limit.saturating_sub(completed);
        if requested > remaining {
            let denied_index =
                u32::try_from(remaining).map_err(|_| AssemblerError::TooManyInstructions)?;
            return Err(AssemblerError::LimitExceeded {
                resource: AssemblerResource::RelaxationEvaluations,
                instruction_index: denied_index,
                limit,
                observed: limit.saturating_add(1),
            });
        }
        Ok(completed + requested)
    }
}

impl Default for BytecodeAssembler {
    fn default() -> Self {
        Self::new()
    }
}

fn contains_symbolic_label(format: OperandFormat) -> bool {
    matches!(
        format,
        OperandFormat::Label8
            | OperandFormat::Label16
            | OperandFormat::Label
            | OperandFormat::AtomLabelU8
            | OperandFormat::AtomLabelU16
            | OperandFormat::LabelU16
    )
}

fn populate_instruction_positions(
    widths: &[u8],
    positions: &mut Vec<u64>,
) -> Result<(), AssemblerError> {
    positions.clear();
    positions.push(0);
    for width in widths {
        let next = positions
            .last()
            .copied()
            .and_then(|position: u64| position.checked_add(u64::from(*width)))
            .ok_or(AssemblerError::EncodedLengthOutOfRange {
                encoded_bytes: u64::MAX,
            })?;
        positions.push(next);
    }
    Ok(())
}

fn required_branch_size(
    kind: BranchKind,
    current_size: u8,
    source_position: usize,
    target_position: usize,
    positions: &[u64],
    instruction_index: u32,
    label_index: u32,
) -> Result<u8, AssemblerError> {
    for candidate in [2_u8, 3, 5] {
        if candidate < current_size || !kind.supports_width(candidate) {
            continue;
        }
        let added = u64::from(candidate - current_size);
        let candidate_target = if target_position > source_position {
            positions[target_position].checked_add(added).ok_or(
                AssemblerError::EncodedLengthOutOfRange {
                    encoded_bytes: positions[target_position],
                },
            )?
        } else {
            positions[target_position]
        };
        let displacement = signed_displacement(positions[source_position], candidate_target)?;
        let fits = match candidate {
            2 => i8::try_from(displacement).is_ok(),
            3 => i16::try_from(displacement).is_ok(),
            5 => i32::try_from(displacement).is_ok(),
            _ => false,
        };
        if fits {
            return Ok(candidate);
        }
    }

    let widest = kind.long_opcode().metadata().instruction_size();
    let added = u64::from(widest - current_size);
    let widest_target = if target_position > source_position {
        positions[target_position].checked_add(added).ok_or(
            AssemblerError::EncodedLengthOutOfRange {
                encoded_bytes: positions[target_position],
            },
        )?
    } else {
        positions[target_position]
    };
    let displacement = signed_displacement(positions[source_position], widest_target)?;
    Err(AssemblerError::BranchDisplacementOutOfRange {
        instruction_index,
        label_index,
        displacement,
    })
}

fn signed_displacement(source_pc: u64, target_pc: u64) -> Result<i64, AssemblerError> {
    let base = source_pc
        .checked_add(1)
        .ok_or(AssemblerError::EncodedLengthOutOfRange {
            encoded_bytes: source_pc,
        })?;
    let source = i128::from(base);
    let target = i128::from(target_pc);
    i64::try_from(target - source).map_err(|_| AssemblerError::EncodedLengthOutOfRange {
        encoded_bytes: source_pc.max(target_pc),
    })
}

fn with_branch_displacement(
    source_pc: u64,
    target_pc: u64,
    instruction_index: u32,
    label_index: u32,
) -> Result<i32, AssemblerError> {
    let base = source_pc
        .checked_add(5)
        .ok_or(AssemblerError::EncodedLengthOutOfRange {
            encoded_bytes: source_pc,
        })?;
    let displacement = i128::from(target_pc) - i128::from(base);
    i32::try_from(displacement).map_err(|_| AssemblerError::BranchDisplacementOutOfRange {
        instruction_index,
        label_index,
        displacement: i64::try_from(displacement).unwrap_or_else(|_| {
            if displacement.is_negative() {
                i64::MIN
            } else {
                i64::MAX
            }
        }),
    })
}

fn branch_instruction(
    kind: BranchKind,
    instruction_size: u8,
    displacement: i64,
    instruction_index: u32,
    label_index: u32,
) -> Result<Instruction, AssemblerError> {
    let (opcode, operands) =
        match (kind, instruction_size) {
            (BranchKind::IfFalse, 2) => (
                FinalOpcode::IfFalse8,
                Operands::Label8(i8::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::IfFalse, 5) => (
                FinalOpcode::IfFalse,
                Operands::Label(i32::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::IfTrue, 2) => (
                FinalOpcode::IfTrue8,
                Operands::Label8(i8::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::IfTrue, 5) => (
                FinalOpcode::IfTrue,
                Operands::Label(i32::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::Goto, 2) => (
                FinalOpcode::Goto8,
                Operands::Label8(i8::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::Goto, 3) => (
                FinalOpcode::Goto16,
                Operands::Label16(i16::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::Goto, 5) => (
                FinalOpcode::Goto,
                Operands::Label(i32::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::Catch, 5) => (
                FinalOpcode::Catch,
                Operands::Label(i32::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            (BranchKind::Gosub, 5) => (
                FinalOpcode::Gosub,
                Operands::Label(i32::try_from(displacement).map_err(|_| {
                    displacement_error(instruction_index, label_index, displacement)
                })?),
            ),
            _ => {
                return Err(AssemblerError::BranchDisplacementOutOfRange {
                    instruction_index,
                    label_index,
                    displacement,
                });
            }
        };
    Instruction::new(opcode, operands).map_err(|source| AssemblerError::InvalidInstruction {
        instruction_index,
        source,
    })
}

const fn displacement_error(
    instruction_index: u32,
    label_index: u32,
    displacement: i64,
) -> AssemblerError {
    AssemblerError::BranchDisplacementOutOfRange {
        instruction_index,
        label_index,
        displacement,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{AssemblerError, BranchKind, required_branch_size};

    #[test]
    fn virtual_layout_rejects_both_i32_displacement_overflows_without_large_allocations() {
        let target = u64::try_from(i64::from(i32::MAX) + 10).expect("positive target");
        let error = required_branch_size(BranchKind::Goto, 5, 0, 1, &[0, target], 7, 11)
            .expect_err("target exceeds the final displacement domain");
        assert_eq!(
            error,
            AssemblerError::BranchDisplacementOutOfRange {
                instruction_index: 7,
                label_index: 11,
                displacement: i64::from(i32::MAX) + 9,
            }
        );

        let source = u64::try_from(i64::from(i32::MAX) + 1).expect("positive source");
        let error = required_branch_size(BranchKind::Goto, 5, 1, 0, &[0, source], 13, 17)
            .expect_err("backward target exceeds the final displacement domain");
        assert_eq!(
            error,
            AssemblerError::BranchDisplacementOutOfRange {
                instruction_index: 13,
                label_index: 17,
                displacement: i64::from(i32::MIN) - 1,
            }
        );
    }
}
