/// Maximum argument, local, closure-reference, or stack count accepted by the
/// pinned `QuickJS` release.
pub const MAX_FUNCTION_INDEX_ENTRIES: u32 = 65_534;

/// Structural maximum operand-stack depth accepted by the pinned `QuickJS`
/// release.
pub const MAX_OPERAND_STACK_DEPTH: u32 = 65_534;

/// Resource limits for one control-flow verification.
///
/// Every maximum is inclusive. [`VerificationLimits::UNTRUSTED`] uses the
/// provisional untrusted-input profile from `BYTECODE_VERIFIER.md`. The
/// current compiler also applies the byte, instruction, and transfer-work
/// values independently to final assembly; the transfer-work value bounds
/// branch-relaxation instruction visits before verification begins.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VerificationLimits {
    pub(super) max_bytecode_bytes_per_function: u32,
    pub(super) max_instructions_per_function: u32,
    pub(super) max_constants_per_function: u32,
    pub(super) max_atom_pool_entries: u32,
    pub(super) max_transfer_evaluations: u64,
    pub(super) max_stack_depth: u32,
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
    ///
    /// The compiler also uses this value as its independent pre-verification
    /// branch-relaxation evaluation maximum.
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
    /// Compiler capture-layout entries.
    CompilerCaptures,
}
