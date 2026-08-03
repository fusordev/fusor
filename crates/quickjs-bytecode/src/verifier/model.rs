use std::{fmt, sync::Arc};

use crate::{BytecodePc, DecodedInstruction, function::UnverifiedFunctionHeader};

use super::{instruction_index_at, predecode::is_instruction_start};

/// Function-local index-domain lengths needed for body verification.
///
/// These counts do not prove that any corresponding pool entry or metadata
/// record is valid. A later whole-function verifier must own and validate the
/// actual pools before execution.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FunctionIndexDomains {
    pub(super) atom_pool_len: u32,
    pub(super) constant_pool_len: u32,
    pub(super) argument_count: u32,
    pub(super) local_count: u32,
    pub(super) closure_var_count: u32,
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

/// One function-owned binding backed by a variable-reference cell.
///
/// The binding's position in [`CompilerCaptureLayout`] is its dense
/// variable-reference index. Local lifetime is explicit because only a
/// block-scoped local may be targeted by `close_loc`; arguments and
/// function-lifetime locals are closed by frame teardown.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerCapturedBinding {
    /// A captured function argument.
    Argument(u32),
    /// A captured local whose lifetime is the complete function frame.
    FunctionLocal(u32),
    /// A captured local whose lexical scope may end before frame teardown.
    ScopedLocal(u32),
}

impl CompilerCapturedBinding {
    pub(super) const fn identity(self) -> CompilerCapturedBindingIdentity {
        match self {
            Self::Argument(index) => CompilerCapturedBindingIdentity::Argument(index),
            Self::FunctionLocal(index) | Self::ScopedLocal(index) => {
                CompilerCapturedBindingIdentity::Local(index)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum CompilerCapturedBindingIdentity {
    Argument(u32),
    Local(u32),
}

/// Immutable compiler-owned capture metadata for one function frame.
///
/// Entries are ordered by dense variable-reference index: entry zero
/// describes variable-reference cell zero, and so on. Verification checks
/// the count, frame-domain bounds, and binding uniqueness before any
/// `close_loc` instruction can be authorized.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CompilerCaptureLayout {
    pub(super) bindings: Arc<[CompilerCapturedBinding]>,
    pub(super) mapped_arguments: Option<Arc<[u32]>>,
}

impl CompilerCaptureLayout {
    /// Creates a capture layout with variable-reference indices defined by
    /// `bindings` order.
    #[must_use]
    pub const fn new(bindings: Arc<[CompilerCapturedBinding]>) -> Self {
        Self {
            bindings,
            mapped_arguments: None,
        }
    }

    /// Attaches the ascending formal positions mapped by a sloppy arguments
    /// exotic object. `Some(empty)` distinguishes a zero-parameter mapped
    /// object from a function without arguments-object authority.
    #[must_use]
    pub fn with_mapped_arguments(mut self, indices: Arc<[u32]>) -> Self {
        self.mapped_arguments = Some(indices);
        self
    }

    /// Returns the dense variable-reference binding table.
    #[must_use]
    pub fn bindings(&self) -> &[CompilerCapturedBinding] {
        &self.bindings
    }

    /// Resolves one dense variable-reference index.
    #[must_use]
    pub fn binding_for_variable_reference(&self, index: u32) -> Option<CompilerCapturedBinding> {
        let index = usize::try_from(index).ok()?;
        self.bindings.get(index).copied()
    }

    /// Returns the compiler-authorized mapped formal positions.
    #[must_use]
    pub fn mapped_arguments(&self) -> Option<&[u32]> {
        self.mapped_arguments.as_deref()
    }

    /// Clones the shared mapped-formal-position certificate without copying
    /// its entries.
    #[must_use]
    pub fn mapped_arguments_arc(&self) -> Option<Arc<[u32]>> {
        self.mapped_arguments.clone()
    }
}

/// Compiler-known runtime kind of one constant-pool entry.
///
/// This metadata is deliberately narrower than an actual constant value. It
/// lets staged control-flow verification distinguish a value loaded by
/// `push_const` from a nested function template instantiated by `fclosure`
/// without granting execution authority to the certificate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerConstantKind {
    /// An ordinary JavaScript value.
    Value,
    /// A compiler-declared nested bytecode-function template.
    Function,
}

/// Immutable compiler-owned type layout for one function's constant pool.
///
/// Entries are ordered by constant-pool index. Actual values and nested
/// function bodies remain the responsibility of the later whole-function
/// verifier.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CompilerConstantLayout {
    pub(super) kinds: Arc<[CompilerConstantKind]>,
}

impl CompilerConstantLayout {
    /// Creates a constant layout indexed by `kinds` order.
    #[must_use]
    pub const fn new(kinds: Arc<[CompilerConstantKind]>) -> Self {
        Self { kinds }
    }

    /// Returns the complete constant-kind table.
    #[must_use]
    pub fn kinds(&self) -> &[CompilerConstantKind] {
        &self.kinds
    }

    /// Resolves one constant-pool index.
    #[must_use]
    pub fn kind(&self, index: u32) -> Option<CompilerConstantKind> {
        let index = usize::try_from(index).ok()?;
        self.kinds.get(index).copied()
    }
}

/// Owned bytecode and structural counts that have not been verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedFunctionBody {
    pub(super) bytecode: Vec<u8>,
    pub(super) expected_stack_size: u32,
    pub(super) domains: FunctionIndexDomains,
    pub(super) function_header: UnverifiedFunctionHeader,
}

impl UnverifiedFunctionBody {
    /// Creates an unverified body.
    #[must_use]
    pub const fn new(
        bytecode: Vec<u8>,
        expected_stack_size: u32,
        domains: FunctionIndexDomains,
        function_header: UnverifiedFunctionHeader,
    ) -> Self {
        Self {
            bytecode,
            expected_stack_size,
            domains,
            function_header,
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

    /// Returns the raw function execution metadata.
    #[must_use]
    pub const fn function_header(&self) -> UnverifiedFunctionHeader {
        self.function_header
    }
}

/// Compiler-generated bytecode whose exact maximum stack size is not yet
/// known.
///
/// Unlike [`UnverifiedFunctionBody`], this input has no serialized stack-size
/// field to compare. [`super::verify_compiler_control_flow`] independently
/// derives the reachable maximum and retains it in [`VerifiedControlFlow`].
/// This does not weaken serialized-bytecode verification and does not grant
/// execution authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedCompilerFunctionBody {
    pub(super) bytecode: Vec<u8>,
    pub(super) domains: FunctionIndexDomains,
    pub(super) function_header: UnverifiedFunctionHeader,
    pub(super) capture_layout: Option<CompilerCaptureLayout>,
    pub(super) constant_layout: Option<CompilerConstantLayout>,
}

impl UnverifiedCompilerFunctionBody {
    /// Creates compiler-generated input requiring complete CFG verification.
    #[must_use]
    pub const fn new(
        bytecode: Vec<u8>,
        domains: FunctionIndexDomains,
        function_header: UnverifiedFunctionHeader,
    ) -> Self {
        Self {
            bytecode,
            domains,
            function_header,
            capture_layout: None,
            constant_layout: None,
        }
    }

    /// Attaches the compiler-owned capture layout.
    #[must_use]
    pub fn with_capture_layout(mut self, capture_layout: CompilerCaptureLayout) -> Self {
        self.capture_layout = Some(capture_layout);
        self
    }

    /// Attaches the compiler-owned constant-pool type layout.
    #[must_use]
    pub fn with_constant_layout(mut self, constant_layout: CompilerConstantLayout) -> Self {
        self.constant_layout = Some(constant_layout);
        self
    }

    /// Returns the raw final-bytecode bytes.
    #[must_use]
    pub fn bytecode(&self) -> &[u8] {
        &self.bytecode
    }

    /// Returns the declared index-domain lengths.
    #[must_use]
    pub const fn domains(&self) -> FunctionIndexDomains {
        self.domains
    }

    /// Returns the raw function execution metadata.
    #[must_use]
    pub const fn function_header(&self) -> UnverifiedFunctionHeader {
        self.function_header
    }

    /// Returns the compiler-owned capture layout.
    #[must_use]
    pub const fn capture_layout(&self) -> Option<&CompilerCaptureLayout> {
        self.capture_layout.as_ref()
    }

    /// Returns the compiler-owned constant-pool type layout.
    #[must_use]
    pub const fn constant_layout(&self) -> Option<&CompilerConstantLayout> {
        self.constant_layout.as_ref()
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
pub struct InstructionIndex(pub(super) u32);

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
    /// A target edge and a continuation fallthrough edge.
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
pub struct VerifiedSuccessors(pub(super) VerifiedSuccessorsRepr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum VerifiedSuccessorsRepr {
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

    /// Returns the taken branch, handler, or subroutine target.
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
    pub(super) decoded: DecodedInstruction,
    pub(super) entry_stack_depth: Option<u32>,
    pub(super) successors: VerifiedSuccessors,
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
/// pool contents, atom namespaces, nested functions, debug payloads, internal
/// handler slots, or source tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedControlFlow {
    pub(super) bytecode: Vec<u8>,
    pub(super) instructions: Vec<VerifiedInstruction>,
    pub(super) instruction_start_bitmap: Vec<u64>,
    pub(super) computed_stack_size: u32,
    pub(super) transfer_evaluations: u64,
    pub(super) domains: FunctionIndexDomains,
    pub(super) function_header: crate::function::VerifiedFunctionHeader,
    pub(super) compiler_capture_layout: Option<CompilerCaptureLayout>,
    pub(super) compiler_constant_layout: Option<CompilerConstantLayout>,
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

    /// Returns the number of reachable abstract transfer evaluations.
    ///
    /// Whole-function graph verification charges this retained count against
    /// its aggregate work budget without rerunning body analysis.
    #[must_use]
    pub const fn transfer_evaluations(&self) -> u64 {
        self.transfer_evaluations
    }

    /// Returns the structural index domains against which operands were
    /// checked.
    #[must_use]
    pub const fn domains(&self) -> FunctionIndexDomains {
        self.domains
    }

    /// Returns the validated function execution metadata.
    #[must_use]
    pub const fn function_header(&self) -> &crate::function::VerifiedFunctionHeader {
        &self.function_header
    }

    /// Returns the validated compiler capture layout.
    ///
    /// Serialized control-flow certificates return `None` because this
    /// verifier does not yet accept serialized capture metadata.
    #[must_use]
    pub const fn compiler_capture_layout(&self) -> Option<&CompilerCaptureLayout> {
        self.compiler_capture_layout.as_ref()
    }

    /// Returns the validated compiler constant-pool type layout.
    ///
    /// Serialized control-flow certificates return `None` because actual
    /// serialized constants require later whole-function verification.
    #[must_use]
    pub const fn compiler_constant_layout(&self) -> Option<&CompilerConstantLayout> {
        self.compiler_constant_layout.as_ref()
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
