//! Final compiler-bytecode metadata verification and execution authority.
//!
//! The staged body and function-graph certificates deliberately omit runtime
//! binding and source metadata. This module closes that boundary for the
//! current ordinary-function compiler profile. Verification is pure, bounded,
//! iterative, and does not materialize runtime values or atoms.

use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt,
    sync::Arc,
};

use crate::{
    AtomPoolIndex, BytecodePc, CompilerClosureSource, FinalOpcode, FunctionKind,
    FunctionTemplateId, Operands, VerifiedCompilerFunction, VerifiedCompilerFunctionGraph,
    VerifiedControlFlow, VerifiedInstruction,
    verifier::{CompilerCapturedBinding, InstructionIndex},
};

const DEFAULT_MAX_VARIABLE_DEFINITIONS: u64 = 1_048_576;
const DEFAULT_MAX_CLOSURE_DEFINITIONS: u64 = 1_048_576;
const DEFAULT_MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_SOURCE_MAPPINGS: u64 = 8_388_608;
const DEFAULT_MAX_FRAME_STATE_ENTRIES: u64 = 33_554_432;
const DEFAULT_MAX_POLICY_TRANSFERS: u64 = 33_554_432;

/// Source declaration category retained by executable compiler metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerBindingKind {
    /// A simple formal parameter.
    Parameter,
    /// A function-scoped `var`.
    Var,
    /// A lexical mutable binding.
    Let,
    /// A lexical immutable binding.
    Const,
    /// A function declaration.
    Function,
    /// A named function-expression self binding.
    FunctionName,
    /// A catch-clause parameter.
    Catch,
    /// An unresolved name looked up in the constructor realm's global
    /// environment each time a global opcode executes.
    GlobalReference,
}

/// When a compiler binding receives its first language-visible value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerInitializationPolicy {
    /// Copied from the corresponding call argument.
    Argument,
    /// Initialized to `undefined` when the function frame is created.
    UndefinedAtInstantiation,
    /// Initialized by executing its declaration.
    AtDeclaration,
    /// Initialized with a closure during function instantiation.
    FunctionAtInstantiation,
    /// Initialized with a closure when its lexical scope is entered.
    FunctionAtScopeEntry,
    /// Initialized to the newly created named function object.
    FunctionName,
    /// Initialized when a catch clause is entered.
    Catch,
    /// Resolved against the constructor realm rather than initialized in a
    /// function frame.
    ConstructorRealmLookup,
}

/// Assignment behavior after a compiler binding is initialized.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerWritePolicy {
    /// Ordinary writes are permitted.
    Mutable,
    /// Every source write is rejected.
    Immutable,
    /// Sloppy writes are ignored while strict writes are rejected.
    ImmutableInStrictCode,
}

/// Complete compiler declaration policy needed by frame and closure checks.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompilerBindingPolicy {
    kind: CompilerBindingKind,
    initialization: CompilerInitializationPolicy,
    writes: CompilerWritePolicy,
    temporal_dead_zone: bool,
}

impl CompilerBindingPolicy {
    /// Creates an unverified compiler declaration policy.
    #[must_use]
    pub const fn new(
        kind: CompilerBindingKind,
        initialization: CompilerInitializationPolicy,
        writes: CompilerWritePolicy,
        temporal_dead_zone: bool,
    ) -> Self {
        Self {
            kind,
            initialization,
            writes,
            temporal_dead_zone,
        }
    }

    /// Returns the declaration category.
    #[must_use]
    pub const fn kind(self) -> CompilerBindingKind {
        self.kind
    }

    /// Returns the initialization point.
    #[must_use]
    pub const fn initialization(self) -> CompilerInitializationPolicy {
        self.initialization
    }

    /// Returns the post-initialization write policy.
    #[must_use]
    pub const fn writes(self) -> CompilerWritePolicy {
        self.writes
    }

    /// Returns whether reads must preserve temporal-dead-zone behavior.
    #[must_use]
    pub const fn has_temporal_dead_zone(self) -> bool {
        self.temporal_dead_zone
    }

    const fn has_scope(self) -> bool {
        matches!(
            self.kind,
            CompilerBindingKind::Let | CompilerBindingKind::Const | CompilerBindingKind::Catch
        ) || matches!(
            self.initialization,
            CompilerInitializationPolicy::FunctionAtScopeEntry
        )
    }

    const fn is_valid(self) -> bool {
        match self.kind {
            CompilerBindingKind::Parameter => {
                matches!(self.initialization, CompilerInitializationPolicy::Argument)
                    && matches!(self.writes, CompilerWritePolicy::Mutable)
                    && !self.temporal_dead_zone
            }
            CompilerBindingKind::Var => {
                matches!(
                    self.initialization,
                    CompilerInitializationPolicy::UndefinedAtInstantiation
                ) && matches!(self.writes, CompilerWritePolicy::Mutable)
                    && !self.temporal_dead_zone
            }
            CompilerBindingKind::Let => {
                matches!(
                    self.initialization,
                    CompilerInitializationPolicy::AtDeclaration
                ) && matches!(self.writes, CompilerWritePolicy::Mutable)
                    && self.temporal_dead_zone
            }
            CompilerBindingKind::Const => {
                matches!(
                    self.initialization,
                    CompilerInitializationPolicy::AtDeclaration
                ) && matches!(self.writes, CompilerWritePolicy::Immutable)
                    && self.temporal_dead_zone
            }
            CompilerBindingKind::Function => {
                matches!(
                    self.initialization,
                    CompilerInitializationPolicy::FunctionAtInstantiation
                        | CompilerInitializationPolicy::FunctionAtScopeEntry
                ) && matches!(self.writes, CompilerWritePolicy::Mutable)
                    && !self.temporal_dead_zone
            }
            CompilerBindingKind::FunctionName => {
                matches!(
                    self.initialization,
                    CompilerInitializationPolicy::FunctionName
                ) && matches!(
                    self.writes,
                    CompilerWritePolicy::Immutable | CompilerWritePolicy::ImmutableInStrictCode
                ) && !self.temporal_dead_zone
            }
            CompilerBindingKind::Catch => false,
            CompilerBindingKind::GlobalReference => {
                matches!(
                    self.initialization,
                    CompilerInitializationPolicy::ConstructorRealmLookup
                ) && matches!(self.writes, CompilerWritePolicy::Mutable)
                    && !self.temporal_dead_zone
            }
        }
    }

    const fn is_valid_for_function(self, strict: bool) -> bool {
        if !self.is_valid() || matches!(self.kind, CompilerBindingKind::GlobalReference) {
            return false;
        }
        match self.kind {
            CompilerBindingKind::FunctionName => matches!(
                (strict, self.writes),
                (true, CompilerWritePolicy::Immutable)
                    | (false, CompilerWritePolicy::ImmutableInStrictCode)
            ),
            _ => true,
        }
    }
}

/// Link to the next local in the same or enclosing lexical scope.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScopeLink {
    /// End of an ordinary scope chain (`QuickJS` sentinel `-1`).
    End,
    /// End of a parameter-expression scope (`QuickJS` sentinel `-2`).
    ArgumentScopeEnd,
    /// Another local-variable slot.
    Local(u32),
}

/// One ordered argument or local compiler-metadata record.
///
/// Public construction is unverified; the same record is exposed through
/// [`VerifiedFunctionMetadata`] only after the complete graph check succeeds.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VariableDefinition {
    name: Option<AtomPoolIndex>,
    scope_next: ScopeLink,
    policy: CompilerBindingPolicy,
    has_scope: bool,
    variable_reference: Option<u32>,
    function_initializer: Option<u32>,
}

impl VariableDefinition {
    /// Creates an unverified variable definition.
    #[must_use]
    pub const fn new(
        name: Option<AtomPoolIndex>,
        scope_next: ScopeLink,
        policy: CompilerBindingPolicy,
        has_scope: bool,
        variable_reference: Option<u32>,
    ) -> Self {
        Self {
            name,
            scope_next,
            policy,
            has_scope,
            variable_reference,
            function_initializer: None,
        }
    }

    /// Attaches the function-template constant that initializes this binding.
    #[must_use]
    pub const fn with_function_initializer(mut self, constant: u32) -> Self {
        self.function_initializer = Some(constant);
        self
    }

    /// Returns the optional function-local name atom.
    #[must_use]
    pub const fn name(&self) -> Option<AtomPoolIndex> {
        self.name
    }

    /// Returns the lexical scope-chain link.
    #[must_use]
    pub const fn scope_next(&self) -> ScopeLink {
        self.scope_next
    }

    /// Returns the declaration policy.
    #[must_use]
    pub const fn policy(&self) -> CompilerBindingPolicy {
        self.policy
    }

    /// Returns whether the binding belongs to a non-function lexical scope.
    #[must_use]
    pub const fn has_scope(&self) -> bool {
        self.has_scope
    }

    /// Returns the dense own variable-reference index when captured.
    #[must_use]
    pub const fn variable_reference(&self) -> Option<u32> {
        self.variable_reference
    }

    /// Returns the function-template constant used for declaration initialization.
    #[must_use]
    pub const fn function_initializer(&self) -> Option<u32> {
        self.function_initializer
    }
}

/// Storage origin retained for one closure-domain slot.
///
/// Realm-global bindings use the same final closure-slot operand domain as
/// captured cells, matching the pinned `QuickJS` opcode contract, but remain
/// explicitly typed so installation and execution never infer their origin
/// from a declaration policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerClosureBinding {
    /// A cell imported from an enclosing function activation.
    Captured(CompilerBindingPolicy),
    /// An unresolved lookup or configurable indirect-eval `var` owned by the
    /// constructor realm. Evaluation-local lexical declarations never use
    /// this origin.
    RealmGlobal(CompilerBindingPolicy),
}

impl CompilerClosureBinding {
    /// Returns the declaration or lookup policy carried by the slot.
    #[must_use]
    pub const fn policy(self) -> CompilerBindingPolicy {
        match self {
            Self::Captured(policy) | Self::RealmGlobal(policy) => policy,
        }
    }

    /// Returns whether this slot belongs to the constructor realm.
    #[must_use]
    pub const fn is_realm_global(self) -> bool {
        matches!(self, Self::RealmGlobal(_))
    }
}

/// One ordered imported closure compiler-metadata record.
///
/// Public construction is unverified; the same record is exposed through
/// [`VerifiedFunctionMetadata`] only after parent-edge checks succeed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ClosureVariableDefinition {
    name: Option<AtomPoolIndex>,
    binding: CompilerClosureBinding,
    source: CompilerClosureSource,
    function_initializer: Option<u32>,
}

impl ClosureVariableDefinition {
    /// Creates an unverified closure descriptor.
    #[must_use]
    pub const fn new(
        name: Option<AtomPoolIndex>,
        policy: CompilerBindingPolicy,
        source: CompilerClosureSource,
    ) -> Self {
        Self {
            name,
            binding: CompilerClosureBinding::Captured(policy),
            source,
            function_initializer: None,
        }
    }

    /// Creates an unverified constructor-realm unresolved-name or
    /// indirect-eval `var` descriptor.
    #[must_use]
    pub const fn realm_global(
        name: Option<AtomPoolIndex>,
        policy: CompilerBindingPolicy,
        source: CompilerClosureSource,
    ) -> Self {
        Self {
            name,
            binding: CompilerClosureBinding::RealmGlobal(policy),
            source,
            function_initializer: None,
        }
    }

    /// Attaches the function-template constant that initializes a
    /// constructor-realm global function declaration.
    #[must_use]
    pub const fn with_function_initializer(mut self, constant: u32) -> Self {
        self.function_initializer = Some(constant);
        self
    }

    /// Returns the optional function-local name atom.
    #[must_use]
    pub const fn name(&self) -> Option<AtomPoolIndex> {
        self.name
    }

    /// Returns the declaration policy inherited from the original binding.
    #[must_use]
    pub const fn policy(&self) -> CompilerBindingPolicy {
        self.binding.policy()
    }

    /// Returns the verified storage origin and its declaration policy.
    #[must_use]
    pub const fn binding(&self) -> CompilerClosureBinding {
        self.binding
    }

    /// Returns the immediate-parent closure source.
    #[must_use]
    pub const fn source(&self) -> CompilerClosureSource {
        self.source
    }

    /// Returns the function-template constant used for a constructor-realm
    /// global function declaration.
    #[must_use]
    pub const fn function_initializer(&self) -> Option<u32> {
        self.function_initializer
    }
}

/// Half-open UTF-8 byte span in one retained source artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceByteSpan {
    start: u32,
    end: u32,
}

impl SourceByteSpan {
    /// Creates an unverified half-open source span.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Returns the inclusive start byte.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// Returns the exclusive end byte.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
}

/// One final instruction PC mapped to an exact source span.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PcSourceSpan {
    pc: BytecodePc,
    span: SourceByteSpan,
}

impl PcSourceSpan {
    /// Creates an unverified PC-to-source mapping.
    #[must_use]
    pub const fn new(pc: BytecodePc, span: SourceByteSpan) -> Self {
        Self { pc, span }
    }

    /// Returns the final instruction PC.
    #[must_use]
    pub const fn pc(self) -> BytecodePc {
        self.pc
    }

    /// Returns the mapped source span.
    #[must_use]
    pub const fn span(self) -> SourceByteSpan {
        self.span
    }
}

/// Owned source identity, text, function range, and final instruction map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerSource {
    display_name: Arc<str>,
    text: Arc<str>,
    function_span: SourceByteSpan,
    name_span: Option<SourceByteSpan>,
    mappings: Arc<[PcSourceSpan]>,
}

impl CompilerSource {
    /// Creates an unverified compiler source record.
    #[must_use]
    pub const fn new(
        display_name: Arc<str>,
        text: Arc<str>,
        function_span: SourceByteSpan,
        name_span: Option<SourceByteSpan>,
        mappings: Arc<[PcSourceSpan]>,
    ) -> Self {
        Self {
            display_name,
            text,
            function_span,
            name_span,
            mappings,
        }
    }
}

/// Compiler-owned execution role assigned to one function-graph record.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CompilerExecutableKind {
    /// An ordinary callable JavaScript function.
    #[default]
    OrdinaryFunction,
    /// The constructor-realm global Script produced for dynamic `Function`.
    DynamicFunctionScript,
}

/// One complete function metadata record awaiting final verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedFunctionMetadata {
    executable_kind: CompilerExecutableKind,
    function_name: Option<AtomPoolIndex>,
    variables: Arc<[VariableDefinition]>,
    closures: Arc<[ClosureVariableDefinition]>,
    source: CompilerSource,
}

impl UnverifiedFunctionMetadata {
    /// Creates one unverified metadata record.
    #[must_use]
    pub const fn new(
        function_name: Option<AtomPoolIndex>,
        variables: Arc<[VariableDefinition]>,
        closures: Arc<[ClosureVariableDefinition]>,
        source: CompilerSource,
    ) -> Self {
        Self {
            executable_kind: CompilerExecutableKind::OrdinaryFunction,
            function_name,
            variables,
            closures,
            source,
        }
    }

    /// Selects the compiler-owned execution role for this record.
    #[must_use]
    pub const fn with_executable_kind(mut self, executable_kind: CompilerExecutableKind) -> Self {
        self.executable_kind = executable_kind;
        self
    }

    /// Returns the requested compiler-owned execution role.
    #[must_use]
    pub const fn executable_kind(&self) -> CompilerExecutableKind {
        self.executable_kind
    }
}

/// A staged compiler graph paired with complete parallel metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedCompilerBytecodeGraph {
    graph: Arc<VerifiedCompilerFunctionGraph>,
    metadata: Arc<[UnverifiedFunctionMetadata]>,
}

impl UnverifiedCompilerBytecodeGraph {
    /// Creates final-verifier input.
    #[must_use]
    pub const fn new(
        graph: Arc<VerifiedCompilerFunctionGraph>,
        metadata: Arc<[UnverifiedFunctionMetadata]>,
    ) -> Self {
        Self { graph, metadata }
    }
}

/// Verified source identity and mappings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCompilerSource {
    display_name: Arc<str>,
    text: Arc<str>,
    function_span: SourceByteSpan,
    name_span: Option<SourceByteSpan>,
    mappings: Arc<[PcSourceSpan]>,
}

impl VerifiedCompilerSource {
    /// Returns the retained display name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Clones the immutable retained display-name owner.
    #[must_use]
    pub fn display_name_arc(&self) -> Arc<str> {
        Arc::clone(&self.display_name)
    }

    /// Returns the complete retained source unit.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Clones the immutable retained source-text owner.
    #[must_use]
    pub fn text_arc(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Returns the verified function byte span.
    #[must_use]
    pub const fn function_span(&self) -> SourceByteSpan {
        self.function_span
    }

    /// Returns the verified optional function-name byte span.
    #[must_use]
    pub const fn name_span(&self) -> Option<SourceByteSpan> {
        self.name_span
    }

    /// Returns the exact source slice for this function.
    #[must_use]
    pub fn function_source(&self) -> &str {
        let start = self.function_span.start as usize;
        let end = self.function_span.end as usize;
        &self.text[start..end]
    }

    /// Returns one mapping for every final instruction.
    #[must_use]
    pub fn mappings(&self) -> &[PcSourceSpan] {
        &self.mappings
    }
}

/// Complete immutable runtime metadata for one verified function template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedFunctionMetadata {
    executable_kind: CompilerExecutableKind,
    function_name: Option<AtomPoolIndex>,
    variables: Arc<[VariableDefinition]>,
    closures: Arc<[ClosureVariableDefinition]>,
    source: VerifiedCompilerSource,
}

impl VerifiedFunctionMetadata {
    /// Returns the verified compiler-owned execution role.
    #[must_use]
    pub const fn executable_kind(&self) -> CompilerExecutableKind {
        self.executable_kind
    }

    /// Returns the optional function-name atom.
    #[must_use]
    pub const fn function_name(&self) -> Option<AtomPoolIndex> {
        self.function_name
    }

    /// Returns arguments followed by locals in frame-slot order.
    #[must_use]
    pub fn variables(&self) -> &[VariableDefinition] {
        &self.variables
    }

    /// Returns imported closure descriptors in child-slot order.
    #[must_use]
    pub fn closures(&self) -> &[ClosureVariableDefinition] {
        &self.closures
    }

    /// Returns the verified retained source artifact.
    #[must_use]
    pub const fn source(&self) -> &VerifiedCompilerSource {
        &self.source
    }
}

/// Runtime implementation family referenced by one verified compiler graph.
///
/// These conservative families select runtime modules; they are not a
/// whole-program value-type proof or a substitute for realm and environment
/// materialization.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExecutionRequirement {
    /// Primitive values, branches, and ordinary returns.
    CoreValues,
    /// ECMAScript Number constants or compact literals.
    Numbers,
    /// ECMAScript String constants or atoms.
    Strings,
    /// Compact `BigInt` values or operators.
    BigInts,
    /// Nested function templates and closure environments.
    Closures,
    /// Ordinary objects, static property access, and own data properties.
    OrdinaryObjects,
    /// Ordinary JavaScript calls, including receiver-aware method calls.
    Calls,
    /// Explicit JavaScript abrupt completions.
    AbruptCompletions,
    /// Lexical initialization, TDZ, or captured scoped locals.
    LexicalBindings,
    /// Constructor-realm unresolved lookup or indirect-eval `var` bindings.
    RealmGlobalBindings,
    /// `in` or `instanceof` object semantics.
    ObjectOperators,
    /// Full dynamic coercion and mixed-type operator semantics.
    DynamicOperators,
}

/// Borrowed complete view of one function in [`VerifiedBytecode`].
#[derive(Clone, Copy, Debug)]
pub struct VerifiedBytecodeFunction<'graph> {
    function: &'graph VerifiedCompilerFunction,
    metadata: &'graph VerifiedFunctionMetadata,
}

impl<'graph> VerifiedBytecodeFunction<'graph> {
    /// Returns the staged body, atom, constant, and child-function record.
    #[must_use]
    pub const fn function(self) -> &'graph VerifiedCompilerFunction {
        self.function
    }

    /// Returns the final verified runtime metadata.
    #[must_use]
    pub const fn metadata(self) -> &'graph VerifiedFunctionMetadata {
        self.metadata
    }
}

/// Immutable execution authority for the current compiler-bytecode profile.
///
/// This type has no public constructor. A VM must accept this as its
/// code-and-metadata boundary, never raw bytes, decoded instructions, or either
/// staged certificate alone. The current runtime installs a fail-closed opcode
/// subset into a same-runtime realm and derives exact child closure
/// environments from the verified metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBytecode {
    graph: Arc<VerifiedCompilerFunctionGraph>,
    metadata: Arc<Vec<VerifiedFunctionMetadata>>,
    requirements: Arc<[ExecutionRequirement]>,
    usage: BytecodeGraphUsage,
}

impl VerifiedBytecode {
    /// Returns the graph-local root identity.
    #[must_use]
    pub fn root_id(&self) -> FunctionTemplateId {
        self.graph.root_id()
    }

    /// Returns the complete verified root-function view.
    #[must_use]
    pub fn root(&self) -> VerifiedBytecodeFunction<'_> {
        let index = self.graph.root_id().get() as usize;
        VerifiedBytecodeFunction {
            function: self.graph.root(),
            metadata: &self.metadata[index],
        }
    }

    /// Returns the staged graph retained by this authority.
    #[must_use]
    pub const fn compiler_graph(&self) -> &Arc<VerifiedCompilerFunctionGraph> {
        &self.graph
    }

    /// Resolves one complete verified function view.
    #[must_use]
    pub fn function(&self, id: FunctionTemplateId) -> Option<VerifiedBytecodeFunction<'_>> {
        let index = usize::try_from(id.get()).ok()?;
        Some(VerifiedBytecodeFunction {
            function: self.graph.function(id)?,
            metadata: self.metadata.get(index)?,
        })
    }

    /// Iterates complete function views in dense template order.
    #[must_use]
    pub fn functions(&self) -> impl ExactSizeIterator<Item = VerifiedBytecodeFunction<'_>> {
        self.graph
            .functions()
            .iter()
            .zip(self.metadata.iter())
            .map(|(function, metadata)| VerifiedBytecodeFunction { function, metadata })
    }

    /// Returns final metadata in dense function-template order.
    #[must_use]
    pub fn metadata(&self) -> &[VerifiedFunctionMetadata] {
        &self.metadata
    }

    /// Returns the sorted runtime feature requirements.
    #[must_use]
    pub fn requirements(&self) -> &[ExecutionRequirement] {
        &self.requirements
    }

    /// Returns aggregate final-verifier resource usage.
    #[must_use]
    pub const fn usage(&self) -> BytecodeGraphUsage {
        self.usage
    }
}

/// Explicit resource limits for final compiler-bytecode verification.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BytecodeGraphVerificationLimits {
    max_variable_definitions: u64,
    max_closure_definitions: u64,
    max_source_bytes: u64,
    max_source_mappings: u64,
    max_frame_state_entries: u64,
    max_policy_transfers: u64,
}

impl BytecodeGraphVerificationLimits {
    /// Provisional limits for untrusted complete compiler graphs.
    pub const UNTRUSTED: Self = Self::new(
        DEFAULT_MAX_VARIABLE_DEFINITIONS,
        DEFAULT_MAX_CLOSURE_DEFINITIONS,
        DEFAULT_MAX_SOURCE_BYTES,
        DEFAULT_MAX_SOURCE_MAPPINGS,
        DEFAULT_MAX_FRAME_STATE_ENTRIES,
        DEFAULT_MAX_POLICY_TRANSFERS,
    );

    /// Creates an explicit final-verification profile.
    #[must_use]
    pub const fn new(
        max_variable_definitions: u64,
        max_closure_definitions: u64,
        max_source_bytes: u64,
        max_source_mappings: u64,
        max_frame_state_entries: u64,
        max_policy_transfers: u64,
    ) -> Self {
        Self {
            max_variable_definitions,
            max_closure_definitions,
            max_source_bytes,
            max_source_mappings,
            max_frame_state_entries,
            max_policy_transfers,
        }
    }

    /// Returns a copy with another variable-definition maximum.
    #[must_use]
    pub const fn with_max_variable_definitions(mut self, maximum: u64) -> Self {
        self.max_variable_definitions = maximum;
        self
    }

    /// Returns a copy with another closure-definition maximum.
    #[must_use]
    pub const fn with_max_closure_definitions(mut self, maximum: u64) -> Self {
        self.max_closure_definitions = maximum;
        self
    }

    /// Returns a copy with another retained source-byte maximum.
    #[must_use]
    pub const fn with_max_source_bytes(mut self, maximum: u64) -> Self {
        self.max_source_bytes = maximum;
        self
    }

    /// Returns a copy with another source-mapping maximum.
    #[must_use]
    pub const fn with_max_source_mappings(mut self, maximum: u64) -> Self {
        self.max_source_mappings = maximum;
        self
    }

    /// Returns a copy with another frame-state entry maximum.
    #[must_use]
    pub const fn with_max_frame_state_entries(mut self, maximum: u64) -> Self {
        self.max_frame_state_entries = maximum;
        self
    }

    /// Returns a copy with another binding-policy transfer maximum.
    #[must_use]
    pub const fn with_max_policy_transfers(mut self, maximum: u64) -> Self {
        self.max_policy_transfers = maximum;
        self
    }

    /// Returns the aggregate argument-and-local definition maximum.
    #[must_use]
    pub const fn max_variable_definitions(self) -> u64 {
        self.max_variable_definitions
    }

    /// Returns the aggregate imported-closure definition maximum.
    #[must_use]
    pub const fn max_closure_definitions(self) -> u64 {
        self.max_closure_definitions
    }

    /// Returns the unique retained source-byte maximum.
    #[must_use]
    pub const fn max_source_bytes(self) -> u64 {
        self.max_source_bytes
    }

    /// Returns the aggregate final PC-to-source mapping maximum.
    #[must_use]
    pub const fn max_source_mappings(self) -> u64 {
        self.max_source_mappings
    }

    /// Returns the conservative abstract frame-state cell maximum.
    #[must_use]
    pub const fn max_frame_state_entries(self) -> u64 {
        self.max_frame_state_entries
    }

    /// Returns the aggregate binding-policy state-cell visit maximum.
    #[must_use]
    pub const fn max_policy_transfers(self) -> u64 {
        self.max_policy_transfers
    }
}

impl Default for BytecodeGraphVerificationLimits {
    fn default() -> Self {
        Self::UNTRUSTED
    }
}

/// Aggregate resource usage retained by [`VerifiedBytecode`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct BytecodeGraphUsage {
    variable_definitions: u64,
    closure_definitions: u64,
    source_bytes: u64,
    source_mappings: u64,
    frame_state_entries: u64,
    policy_transfers: u64,
}

impl BytecodeGraphUsage {
    /// Returns total argument and local definitions.
    #[must_use]
    pub const fn variable_definitions(self) -> u64 {
        self.variable_definitions
    }

    /// Returns total imported closure definitions.
    #[must_use]
    pub const fn closure_definitions(self) -> u64 {
        self.closure_definitions
    }

    /// Returns unique retained source and display-name bytes.
    #[must_use]
    pub const fn source_bytes(self) -> u64 {
        self.source_bytes
    }

    /// Returns total final PC-to-source mappings.
    #[must_use]
    pub const fn source_mappings(self) -> u64 {
        self.source_mappings
    }

    /// Returns allocated abstract frame-state entries.
    #[must_use]
    pub const fn frame_state_entries(self) -> u64 {
        self.frame_state_entries
    }

    /// Returns evaluated binding-policy transfers.
    #[must_use]
    pub const fn policy_transfers(self) -> u64 {
        self.policy_transfers
    }
}

/// Resource governed by final bytecode graph limits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BytecodeGraphResource {
    /// Argument and local definitions.
    VariableDefinitions,
    /// Imported closure definitions.
    ClosureDefinitions,
    /// Unique retained source and display-name bytes.
    SourceBytes,
    /// Final PC-to-source mappings.
    SourceMappings,
    /// Abstract frame-state entries.
    FrameStateEntries,
    /// Binding-policy transfer evaluations.
    PolicyTransfers,
    /// Frozen verified metadata records.
    VerifiedMetadata,
}

impl fmt::Display for BytecodeGraphResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::VariableDefinitions => "variable definitions",
            Self::ClosureDefinitions => "closure definitions",
            Self::SourceBytes => "source bytes",
            Self::SourceMappings => "source mappings",
            Self::FrameStateEntries => "frame-state entries",
            Self::PolicyTransfers => "binding-policy transfers",
            Self::VerifiedMetadata => "verified metadata records",
        })
    }
}

/// Metadata atom location named by a verification failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MetadataAtomField {
    /// Function display name.
    FunctionName,
    /// Argument or local name.
    VariableName(u32),
    /// Imported closure name.
    ClosureName(u32),
}

/// Frame slot named by a binding-policy failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BindingSlot {
    /// Formal argument slot.
    Argument(u32),
    /// Function local slot.
    Local(u32),
    /// Imported closure slot.
    Closure(u32),
}

/// Exact reason an opcode conflicts with retained binding policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BindingPolicyViolationReason {
    /// A supplied declaration-policy combination is not defined.
    InvalidDeclarationPolicy,
    /// Argument metadata is not the simple mutable-parameter profile.
    InvalidArgumentDefinition,
    /// The scope flag disagrees with the declaration policy.
    ScopeFlagMismatch,
    /// A checked operation targets a non-TDZ binding.
    UnexpectedCheckedAccess,
    /// An unchecked operation targets a TDZ binding.
    UncheckedTemporalDeadZoneAccess,
    /// A write targets an immutable binding.
    ImmutableWrite,
    /// Lexical initialization was attempted outside the uninitialized state.
    InvalidLexicalInitialization,
    /// A reachable lexical access can occur before scope initialization.
    MissingLexicalScopeInitialization,
}

/// Structured final compiler-bytecode verification failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BytecodeVerificationError {
    function: Option<FunctionTemplateId>,
    kind: BytecodeVerificationErrorKind,
}

impl BytecodeVerificationError {
    fn graph(kind: BytecodeVerificationErrorKind) -> Self {
        Self {
            function: None,
            kind,
        }
    }

    fn function(function: FunctionTemplateId, kind: BytecodeVerificationErrorKind) -> Self {
        Self {
            function: Some(function),
            kind,
        }
    }

    /// Returns the affected function, when local.
    #[must_use]
    pub const fn function_id(&self) -> Option<FunctionTemplateId> {
        self.function
    }

    /// Returns the exact structured failure.
    #[must_use]
    pub const fn kind(&self) -> &BytecodeVerificationErrorKind {
        &self.kind
    }
}

impl fmt::Display for BytecodeVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(function) = self.function {
            write!(formatter, "function {function}: ")?;
        }
        self.kind.fmt(formatter)
    }
}

impl Error for BytecodeVerificationError {}

/// Exact reason final compiler-bytecode verification failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BytecodeVerificationErrorKind {
    /// Metadata count differs from the staged function count.
    FunctionMetadataCountMismatch {
        /// Staged function records.
        functions: u64,
        /// Supplied metadata records.
        metadata: u64,
    },
    /// An aggregate final-verifier budget was exceeded.
    LimitExceeded {
        /// Exhausted resource.
        resource: BytecodeGraphResource,
        /// Inclusive configured maximum.
        limit: u64,
        /// Observed value.
        observed: u64,
    },
    /// Temporary or frozen verifier storage could not be reserved.
    AllocationFailed {
        /// Allocation purpose.
        resource: BytecodeGraphResource,
        /// Requested entries.
        requested: u64,
    },
    /// A dynamic-Function Script record is not the graph root.
    DynamicFunctionScriptNotRoot,
    /// A dynamic-Function Script record declares a call-argument domain.
    DynamicFunctionScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A dynamic-Function Script record carries function-name metadata or a
    /// named-function self binding.
    DynamicFunctionScriptHasFunctionName,
    /// A constructor-realm global source appears outside a dynamic-Function
    /// Script authority root.
    ConstructorRealmGlobalSourceRequiresDynamicFunctionScript {
        /// Closure-domain slot containing the source.
        closure: u32,
    },
    /// A global opcode targets a captured slot, or a captured-cell opcode
    /// targets a realm-global slot.
    ClosureBindingOpcodeMismatch {
        /// Affected closure-domain slot.
        closure: u32,
        /// Final bytecode position.
        pc: BytecodePc,
        /// Rejected opcode.
        opcode: FinalOpcode,
    },
    /// `delete_var` names no unresolved realm-global reference declared by
    /// this function's verified closure metadata.
    RealmGlobalDeleteBindingMissing {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Function-local atom named by `delete_var`.
        atom: AtomPoolIndex,
    },
    /// Function flags or mode are outside the selected compiler profile.
    UnsupportedFunctionHeader,
    /// Source-defined arguments do not equal simple parameter positions.
    DefinedArgumentCountMismatch {
        /// Header-defined argument count.
        defined: u32,
        /// Frame argument count.
        arguments: u32,
    },
    /// Argument-plus-local definition count differs from frame domains.
    VariableDefinitionCountMismatch {
        /// Required definition count.
        declared: u64,
        /// Supplied definitions.
        entries: u64,
    },
    /// Closure definition count differs from the staged closure domain.
    ClosureDefinitionCountMismatch {
        /// Required closure count.
        declared: u32,
        /// Supplied descriptors.
        entries: u64,
    },
    /// A required variable or closure name is absent.
    MissingMetadataAtom {
        /// Missing atom field.
        field: MetadataAtomField,
    },
    /// A metadata atom index is outside its function-local pool.
    MetadataAtomOutOfBounds {
        /// Invalid atom field.
        field: MetadataAtomField,
        /// Rejected index.
        index: u32,
        /// Function-local atom count.
        len: u32,
    },
    /// A variable definition has an invalid policy or opcode relationship.
    BindingPolicyViolation {
        /// Affected frame slot.
        slot: BindingSlot,
        /// Final bytecode position when opcode-related.
        pc: Option<BytecodePc>,
        /// Exact mismatch.
        reason: BindingPolicyViolationReason,
    },
    /// Parameter-expression scope metadata is deferred.
    ArgumentScopeMetadataUnsupported {
        /// Definition containing the sentinel.
        definition: u32,
    },
    /// A local scope link is out of range.
    ScopeLinkOutOfBounds {
        /// Definition containing the link.
        definition: u32,
        /// Rejected local index.
        target: u32,
        /// Local count.
        locals: u32,
    },
    /// A scope link crosses between lexical and function-scoped definitions.
    ScopeLinkKindMismatch {
        /// Definition containing the link.
        definition: u32,
        /// Target with an incompatible scope category.
        target: u32,
    },
    /// A local scope-link chain contains a cycle.
    ScopeLinkCycle {
        /// One local in the cycle.
        local: u32,
    },
    /// A variable-reference index is out of range.
    VariableReferenceOutOfBounds {
        /// Definition containing the reference.
        definition: u32,
        /// Rejected reference.
        reference: u32,
        /// Declared reference count.
        len: u32,
    },
    /// Two vardefs claim the same own variable-reference index.
    DuplicateVariableReference {
        /// Repeated reference index.
        reference: u32,
    },
    /// Captured vardefs do not form exactly the dense declared domain.
    VariableReferenceDomainMismatch {
        /// Declared references.
        declared: u32,
        /// Captured vardefs.
        captured: u32,
    },
    /// A vardef disagrees with the staged capture layout.
    CaptureLayoutMismatch {
        /// Dense variable-reference index.
        reference: u32,
    },
    /// Function-binding metadata does not name one valid function constant.
    FunctionInitializerMetadataMismatch {
        /// Argument-or-local definition index.
        definition: u32,
        /// Supplied constant index, when present.
        constant: Option<u32>,
    },
    /// Function-binding bytecode does not contain one isolated initializer pair.
    FunctionInitializerOpcodeMismatch {
        /// Argument-or-local definition index.
        definition: u32,
        /// Required function constant.
        constant: u32,
        /// Matching `FClosure` plus `Put` pairs.
        matches: u32,
    },
    /// Two vardefs claim the same function initializer constant.
    FunctionInitializerConstantReused {
        /// Reused function constant.
        constant: u32,
        /// First vardef using the constant.
        first: u32,
        /// Duplicate vardef using the constant.
        duplicate: u32,
    },
    /// A function-instantiation initializer is outside the isolated entry prefix.
    FunctionInitializerPlacementMismatch {
        /// Argument-or-local definition index.
        definition: u32,
        /// Actual closure PC.
        pc: BytecodePc,
    },
    /// A constructor-realm function declaration does not name one valid
    /// function constant.
    RealmGlobalFunctionInitializerMetadataMismatch {
        /// Root closure-domain slot.
        closure: u32,
        /// Supplied constant index, when present.
        constant: Option<u32>,
    },
    /// Constructor-realm function bytecode does not contain one isolated
    /// initializer pair.
    RealmGlobalFunctionInitializerOpcodeMismatch {
        /// Root closure-domain slot.
        closure: u32,
        /// Required function constant.
        constant: u32,
        /// Matching `FClosure` plus `PutVar` pairs.
        matches: u32,
    },
    /// A constructor-realm function initializer is outside the isolated entry
    /// prefix.
    RealmGlobalFunctionInitializerPlacementMismatch {
        /// Root closure-domain slot.
        closure: u32,
        /// Actual closure PC.
        pc: BytecodePc,
    },
    /// A function template is not owned by exactly one parent constant edge.
    FunctionTemplateOwnershipMismatch {
        /// Function template with invalid ownership.
        child: FunctionTemplateId,
        /// Observed incoming function-constant edges.
        incoming: u64,
    },
    /// Closure source or declaration metadata differs from its parent.
    ClosureMetadataMismatch {
        /// Child function.
        child: FunctionTemplateId,
        /// Child closure slot.
        closure: u32,
    },
    /// Source display name is empty.
    EmptySourceDisplayName,
    /// A source span is reversed, out of range, or not on UTF-8 boundaries.
    InvalidSourceSpan {
        /// Rejected span.
        span: SourceByteSpan,
    },
    /// Function-name atom and source span presence differ.
    FunctionNameSourceMismatch,
    /// Function-name span is outside the function source span.
    FunctionNameOutsideFunction,
    /// Source mapping count differs from the instruction count.
    SourceMappingCountMismatch {
        /// Verified instructions.
        instructions: u64,
        /// Supplied mappings.
        mappings: u64,
    },
    /// A mapping PC differs from the corresponding verified instruction.
    SourcePcMismatch {
        /// Mapping position.
        mapping: u32,
        /// Supplied PC.
        declared: BytecodePc,
        /// Verified PC.
        actual: BytecodePc,
    },
    /// An instruction source span falls outside its function source.
    InstructionSourceOutsideFunction {
        /// Mapping position.
        mapping: u32,
    },
    /// The staged body contains an opcode outside the compiler profile.
    UnsupportedCompilerOpcode {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Rejected opcode.
        opcode: FinalOpcode,
    },
}

impl fmt::Display for BytecodeVerificationErrorKind {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FunctionMetadataCountMismatch {
                functions,
                metadata,
            } => write!(
                formatter,
                "metadata count {metadata} does not equal function count {functions}"
            ),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} was exceeded by observed value {observed}"
            ),
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                formatter,
                "could not allocate {requested} entries for {resource}"
            ),
            Self::DynamicFunctionScriptNotRoot => {
                formatter.write_str("dynamic-Function Script executable is not the graph root")
            }
            Self::DynamicFunctionScriptHasArguments { defined, arguments } => write!(
                formatter,
                "dynamic-Function Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::DynamicFunctionScriptHasFunctionName => formatter.write_str(
                "dynamic-Function Script carries function-name metadata or a self binding",
            ),
            Self::ConstructorRealmGlobalSourceRequiresDynamicFunctionScript { closure } => write!(
                formatter,
                "closure slot {closure} originates a constructor-realm global outside a dynamic-Function Script root"
            ),
            Self::ClosureBindingOpcodeMismatch {
                closure,
                pc,
                opcode,
            } => write!(
                formatter,
                "opcode {opcode} at PC {pc} is incompatible with closure slot {closure}'s storage origin"
            ),
            Self::RealmGlobalDeleteBindingMissing { pc, atom } => write!(
                formatter,
                "delete_var at PC {pc} names atom {} without a verified unresolved realm-global binding",
                atom.get()
            ),
            Self::UnsupportedFunctionHeader => {
                formatter.write_str("function header is outside its compiler executable profile")
            }
            Self::DefinedArgumentCountMismatch { defined, arguments } => write!(
                formatter,
                "defined argument count {defined} does not equal simple argument count {arguments}"
            ),
            Self::VariableDefinitionCountMismatch { declared, entries } => write!(
                formatter,
                "variable definition count {entries} does not equal frame count {declared}"
            ),
            Self::ClosureDefinitionCountMismatch { declared, entries } => write!(
                formatter,
                "closure definition count {entries} does not equal closure domain {declared}"
            ),
            Self::MissingMetadataAtom { field } => {
                write!(formatter, "required metadata atom {field:?} is absent")
            }
            Self::MetadataAtomOutOfBounds { field, index, len } => write!(
                formatter,
                "metadata atom {field:?} index {index} is outside atom count {len}"
            ),
            Self::BindingPolicyViolation { slot, pc, reason } => {
                write!(formatter, "binding policy {reason:?} for {slot:?}")?;
                if let Some(pc) = pc {
                    write!(formatter, " at PC {pc}")?;
                }
                Ok(())
            }
            Self::ArgumentScopeMetadataUnsupported { definition } => write!(
                formatter,
                "definition {definition} uses deferred parameter-expression scope metadata"
            ),
            Self::ScopeLinkOutOfBounds {
                definition,
                target,
                locals,
            } => write!(
                formatter,
                "definition {definition} links to local {target} outside local count {locals}"
            ),
            Self::ScopeLinkKindMismatch { definition, target } => write!(
                formatter,
                "definition {definition} links to local {target} with a different scope category"
            ),
            Self::ScopeLinkCycle { local } => {
                write!(
                    formatter,
                    "scope-link chain contains a cycle at local {local}"
                )
            }
            Self::VariableReferenceOutOfBounds {
                definition,
                reference,
                len,
            } => write!(
                formatter,
                "definition {definition} variable reference {reference} is outside count {len}"
            ),
            Self::DuplicateVariableReference { reference } => {
                write!(
                    formatter,
                    "variable reference {reference} is assigned twice"
                )
            }
            Self::VariableReferenceDomainMismatch { declared, captured } => write!(
                formatter,
                "captured vardef count {captured} does not equal variable-reference count {declared}"
            ),
            Self::CaptureLayoutMismatch { reference } => write!(
                formatter,
                "vardef for variable reference {reference} disagrees with capture layout"
            ),
            Self::FunctionInitializerMetadataMismatch {
                definition,
                constant,
            } => write!(
                formatter,
                "function definition {definition} has invalid initializer constant {constant:?}"
            ),
            Self::FunctionInitializerOpcodeMismatch {
                definition,
                constant,
                matches,
            } => write!(
                formatter,
                "function definition {definition} constant {constant} has {matches} initializer pairs"
            ),
            Self::FunctionInitializerConstantReused {
                constant,
                first,
                duplicate,
            } => write!(
                formatter,
                "function constant {constant} initializes definitions {first} and {duplicate}"
            ),
            Self::FunctionInitializerPlacementMismatch { definition, pc } => write!(
                formatter,
                "function definition {definition} initializer at PC {pc} is outside the isolated entry prefix"
            ),
            Self::RealmGlobalFunctionInitializerMetadataMismatch { closure, constant } => write!(
                formatter,
                "realm-global function closure {closure} has invalid initializer constant {constant:?}"
            ),
            Self::RealmGlobalFunctionInitializerOpcodeMismatch {
                closure,
                constant,
                matches,
            } => write!(
                formatter,
                "realm-global function closure {closure} constant {constant} has {matches} initializer pairs"
            ),
            Self::RealmGlobalFunctionInitializerPlacementMismatch { closure, pc } => write!(
                formatter,
                "realm-global function closure {closure} initializer at PC {pc} is outside the isolated entry prefix"
            ),
            Self::FunctionTemplateOwnershipMismatch { child, incoming } => write!(
                formatter,
                "function template {child} has {incoming} incoming ownership edges"
            ),
            Self::ClosureMetadataMismatch { child, closure } => write!(
                formatter,
                "child function {child} closure {closure} disagrees with its parent"
            ),
            Self::EmptySourceDisplayName => formatter.write_str("source display name is empty"),
            Self::InvalidSourceSpan { span } => {
                write!(formatter, "invalid source span {span:?}")
            }
            Self::FunctionNameSourceMismatch => {
                formatter.write_str("function name atom and source span presence differ")
            }
            Self::FunctionNameOutsideFunction => {
                formatter.write_str("function name span is outside function span")
            }
            Self::SourceMappingCountMismatch {
                instructions,
                mappings,
            } => write!(
                formatter,
                "source mapping count {mappings} does not equal instruction count {instructions}"
            ),
            Self::SourcePcMismatch {
                mapping,
                declared,
                actual,
            } => write!(
                formatter,
                "source mapping {mapping} uses PC {declared}, expected {actual}"
            ),
            Self::InstructionSourceOutsideFunction { mapping } => write!(
                formatter,
                "source mapping {mapping} is outside its function span"
            ),
            Self::UnsupportedCompilerOpcode { pc, opcode } => {
                write!(
                    formatter,
                    "opcode {opcode:?} at PC {pc} is outside compiler profile"
                )
            }
        }
    }
}

/// Verifies complete compiler metadata and freezes the VM's immutable
/// code-and-metadata input.
///
/// # Errors
///
/// Returns a structured error without exposing partial authority.
pub fn verify_compiler_bytecode_graph(
    input: UnverifiedCompilerBytecodeGraph,
    limits: BytecodeGraphVerificationLimits,
) -> Result<VerifiedBytecode, BytecodeVerificationError> {
    let UnverifiedCompilerBytecodeGraph { graph, metadata } = input;
    let function_count = graph.functions().len();
    if metadata.len() != function_count {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::FunctionMetadataCountMismatch {
                functions: usize_to_u64(function_count),
                metadata: usize_to_u64(metadata.len()),
            },
        ));
    }

    let mut usage = preflight_usage(&graph, &metadata, limits)?;
    verify_function_tree_ownership(&graph)?;
    let mut verified = Vec::new();
    verified.try_reserve_exact(function_count).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::VerifiedMetadata,
            requested: usize_to_u64(function_count),
        })
    })?;
    let mut requirements = Vec::new();
    requirements.try_reserve_exact(12).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::VerifiedMetadata,
            requested: 12,
        })
    })?;
    requirements.push(ExecutionRequirement::CoreValues);
    let root_index = usize::try_from(graph.root_id().get()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::VerifiedMetadata,
            limit: u64::from(u32::MAX),
            observed: u64::from(graph.root_id().get()),
        })
    })?;
    let authority_kind = metadata
        .get(root_index)
        .ok_or_else(|| {
            BytecodeVerificationError::graph(
                BytecodeVerificationErrorKind::FunctionMetadataCountMismatch {
                    functions: usize_to_u64(function_count),
                    metadata: usize_to_u64(metadata.len()),
                },
            )
        })?
        .executable_kind;

    for (index, (function, metadata)) in graph.functions().iter().zip(metadata.iter()).enumerate() {
        let id = function_id(index)?;
        let record = verify_function_metadata(
            id,
            &graph,
            function,
            metadata,
            authority_kind,
            limits,
            &mut usage,
        )?;
        collect_requirements(function, &record, &mut requirements);
        verified.push(record);
    }
    verify_closure_metadata(&graph, &verified)?;

    requirements.sort_unstable();
    Ok(VerifiedBytecode {
        graph,
        metadata: Arc::new(verified),
        requirements: requirements.into(),
        usage,
    })
}

fn preflight_usage(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[UnverifiedFunctionMetadata],
    limits: BytecodeGraphVerificationLimits,
) -> Result<BytecodeGraphUsage, BytecodeVerificationError> {
    let mut usage = BytecodeGraphUsage::default();
    charge(
        &mut usage.policy_transfers,
        graph.usage().closure_edge_evaluations(),
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    let mut source_texts = HashSet::new();
    let mut display_names = HashSet::new();
    source_texts.try_reserve(metadata.len()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::SourceBytes,
            requested: usize_to_u64(metadata.len()),
        })
    })?;
    display_names.try_reserve(metadata.len()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::SourceBytes,
            requested: usize_to_u64(metadata.len()),
        })
    })?;
    for (function, metadata) in graph.functions().iter().zip(metadata) {
        charge(
            &mut usage.variable_definitions,
            usize_to_u64(metadata.variables.len()),
            limits.max_variable_definitions,
            BytecodeGraphResource::VariableDefinitions,
        )?;
        charge(
            &mut usage.closure_definitions,
            usize_to_u64(metadata.closures.len()),
            limits.max_closure_definitions,
            BytecodeGraphResource::ClosureDefinitions,
        )?;
        charge(
            &mut usage.source_mappings,
            usize_to_u64(metadata.source.mappings.len()),
            limits.max_source_mappings,
            BytecodeGraphResource::SourceMappings,
        )?;
        let frame_tracked = metadata
            .variables
            .iter()
            .filter(|definition| requires_binding_state(definition))
            .count();
        let state_entries = usize_to_u64(function.control_flow().instructions().len())
            .checked_mul(usize_to_u64(frame_tracked))
            .ok_or_else(|| {
                BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    limit: limits.max_frame_state_entries,
                    observed: u64::MAX,
                })
            })?;
        charge(
            &mut usage.frame_state_entries,
            state_entries,
            limits.max_frame_state_entries,
            BytecodeGraphResource::FrameStateEntries,
        )?;
        if source_texts.insert(Arc::as_ptr(&metadata.source.text)) {
            charge(
                &mut usage.source_bytes,
                usize_to_u64(metadata.source.text.len()),
                limits.max_source_bytes,
                BytecodeGraphResource::SourceBytes,
            )?;
        }
        if display_names.insert(Arc::as_ptr(&metadata.source.display_name)) {
            charge(
                &mut usage.source_bytes,
                usize_to_u64(metadata.source.display_name.len()),
                limits.max_source_bytes,
                BytecodeGraphResource::SourceBytes,
            )?;
        }
    }
    Ok(usage)
}

fn verify_function_tree_ownership(
    graph: &VerifiedCompilerFunctionGraph,
) -> Result<(), BytecodeVerificationError> {
    let functions = graph.functions();
    let mut incoming = try_filled_vec(
        graph.root_id(),
        functions.len(),
        0_u64,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for parent in functions {
        for constant in parent.constants() {
            let crate::CompilerConstant::Function(child) = constant else {
                continue;
            };
            let Some(count) = usize::try_from(child.get())
                .ok()
                .and_then(|index| incoming.get_mut(index))
            else {
                return Err(BytecodeVerificationError::function(
                    *child,
                    BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
                        child: *child,
                        incoming: 0,
                    },
                ));
            };
            *count = count.saturating_add(1);
        }
    }
    for (index, &count) in incoming.iter().enumerate() {
        let child = function_id(index)?;
        let expected = u64::from(child != graph.root_id());
        if count != expected {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
                    child,
                    incoming: count,
                },
            ));
        }
    }
    Ok(())
}

fn charge(
    total: &mut u64,
    amount: u64,
    limit: u64,
    resource: BytecodeGraphResource,
) -> Result<(), BytecodeVerificationError> {
    *total = total.checked_add(amount).ok_or_else(|| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource,
            limit,
            observed: u64::MAX,
        })
    })?;
    if *total > limit {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::LimitExceeded {
                resource,
                limit,
                observed: *total,
            },
        ));
    }
    Ok(())
}

fn try_filled_vec<T: Clone>(
    id: FunctionTemplateId,
    length: usize,
    value: T,
    resource: BytecodeGraphResource,
) -> Result<Vec<T>, BytecodeVerificationError> {
    let mut output = Vec::new();
    output.try_reserve_exact(length).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource,
                requested: usize_to_u64(length),
            },
        )
    })?;
    output.resize(length, value);
    Ok(output)
}

fn try_copy_slice<T: Copy>(
    id: FunctionTemplateId,
    input: &[T],
    resource: BytecodeGraphResource,
) -> Result<Vec<T>, BytecodeVerificationError> {
    let mut output = Vec::new();
    output.try_reserve_exact(input.len()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource,
                requested: usize_to_u64(input.len()),
            },
        )
    })?;
    output.extend_from_slice(input);
    Ok(output)
}

fn verify_function_metadata(
    id: FunctionTemplateId,
    graph: &VerifiedCompilerFunctionGraph,
    function: &VerifiedCompilerFunction,
    metadata: &UnverifiedFunctionMetadata,
    authority_kind: CompilerExecutableKind,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<VerifiedFunctionMetadata, BytecodeVerificationError> {
    let flow = function.control_flow();
    verify_executable_kind(id, graph.root_id(), metadata)?;
    verify_header(id, metadata.executable_kind, flow)?;
    let domains = flow.domains();
    let declared_variables = u64::from(domains.argument_count()) + u64::from(domains.local_count());
    if usize_to_u64(metadata.variables.len()) != declared_variables {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableDefinitionCountMismatch {
                declared: declared_variables,
                entries: usize_to_u64(metadata.variables.len()),
            },
        ));
    }
    if usize_to_u64(metadata.closures.len()) != u64::from(domains.closure_var_count()) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ClosureDefinitionCountMismatch {
                declared: domains.closure_var_count(),
                entries: usize_to_u64(metadata.closures.len()),
            },
        ));
    }
    if metadata.source.mappings.len() != flow.instructions().len() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::SourceMappingCountMismatch {
                instructions: usize_to_u64(flow.instructions().len()),
                mappings: usize_to_u64(metadata.source.mappings.len()),
            },
        ));
    }

    verify_optional_atom(
        id,
        metadata.function_name,
        MetadataAtomField::FunctionName,
        function,
    )?;
    verify_variables(id, function, &metadata.variables)?;
    verify_closures(
        id,
        graph.root_id(),
        authority_kind,
        function,
        &metadata.closures,
    )?;
    let realm_global_initializer_prefix = verify_realm_global_function_initializers(
        id,
        graph.root_id(),
        function,
        &metadata.closures,
    )?;
    let initializer_sites = verify_function_initializers(
        id,
        function,
        &metadata.variables,
        realm_global_initializer_prefix,
    )?;
    verify_source(id, flow, metadata)?;
    verify_supported_opcodes(id, flow, metadata.executable_kind, authority_kind)?;
    verify_binding_opcodes(id, flow, &metadata.variables, &metadata.closures)?;
    let binding_transfers = verify_binding_states(
        id,
        graph,
        function,
        &metadata.variables,
        &initializer_sites,
        realm_global_initializer_prefix,
        usage.policy_transfers,
        limits.max_policy_transfers,
    )?;
    charge(
        &mut usage.policy_transfers,
        binding_transfers,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    Ok(VerifiedFunctionMetadata {
        executable_kind: metadata.executable_kind,
        function_name: metadata.function_name,
        variables: Arc::clone(&metadata.variables),
        closures: Arc::clone(&metadata.closures),
        source: VerifiedCompilerSource {
            display_name: Arc::clone(&metadata.source.display_name),
            text: Arc::clone(&metadata.source.text),
            function_span: metadata.source.function_span,
            name_span: metadata.source.name_span,
            mappings: Arc::clone(&metadata.source.mappings),
        },
    })
}

fn verify_executable_kind(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    match metadata.executable_kind {
        CompilerExecutableKind::OrdinaryFunction => Ok(()),
        CompilerExecutableKind::DynamicFunctionScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptNotRoot,
                ));
            }
            let has_function_name_binding =
                metadata.variables.iter().any(|definition| {
                    definition.policy.kind() == CompilerBindingKind::FunctionName
                }) || metadata.closures.iter().any(|definition| {
                    definition.policy().kind() == CompilerBindingKind::FunctionName
                });
            if metadata.function_name.is_some() || has_function_name_binding {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptHasFunctionName,
                ));
            }
            Ok(())
        }
    }
}

fn verify_header(
    id: FunctionTemplateId,
    executable_kind: CompilerExecutableKind,
    flow: &VerifiedControlFlow,
) -> Result<(), BytecodeVerificationError> {
    let header = *flow.function_header();
    let arguments = flow.domains().argument_count();
    match executable_kind {
        CompilerExecutableKind::OrdinaryFunction => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0643
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != arguments {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || header.mode().bits() != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_variables(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let domains = function.control_flow().domains();
    let arguments = usize::try_from(domains.argument_count()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableDefinitionCountMismatch {
                declared: u64::from(domains.argument_count()),
                entries: usize_to_u64(variables.len()),
            },
        )
    })?;
    let locals = domains.local_count();
    let strict = function.control_flow().function_header().mode().is_strict();
    let variable_references = function
        .control_flow()
        .function_header()
        .variable_reference_count();
    let mut seen_references = try_filled_vec(
        id,
        variable_references as usize,
        false,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut initializer_definitions = Vec::new();
    initializer_definitions
        .try_reserve_exact(variables.len())
        .map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::VariableDefinitions,
                    requested: usize_to_u64(variables.len()),
                },
            )
        })?;
    for (index, definition) in variables.iter().enumerate() {
        let definition_index = usize_to_u32(index);
        let slot = if index < arguments {
            BindingSlot::Argument(definition_index)
        } else {
            BindingSlot::Local(usize_to_u32(index - arguments))
        };
        verify_required_atom(
            id,
            definition.name,
            MetadataAtomField::VariableName(definition_index),
            function,
        )?;
        if !definition.policy.is_valid_for_function(strict) {
            return Err(policy_error(
                id,
                slot,
                None,
                BindingPolicyViolationReason::InvalidDeclarationPolicy,
            ));
        }
        let requires_function_initializer = matches!(
            definition.policy.initialization,
            CompilerInitializationPolicy::FunctionAtInstantiation
                | CompilerInitializationPolicy::FunctionAtScopeEntry
        );
        match definition.function_initializer {
            None if requires_function_initializer => {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: definition_index,
                        constant: None,
                    },
                ));
            }
            Some(constant)
                if !requires_function_initializer
                    && definition.policy.kind != CompilerBindingKind::Parameter =>
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: definition_index,
                        constant: Some(constant),
                    },
                ));
            }
            Some(constant)
                if !matches!(
                    function.constants().get(constant as usize),
                    Some(crate::CompilerConstant::Function(_))
                ) =>
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: definition_index,
                        constant: Some(constant),
                    },
                ));
            }
            _ => {}
        }
        if let Some(constant) = definition.function_initializer {
            initializer_definitions.push((constant, definition_index));
        }
        if index < arguments {
            if definition.policy.kind != CompilerBindingKind::Parameter
                || definition.has_scope
                || definition.scope_next != ScopeLink::End
            {
                return Err(policy_error(
                    id,
                    slot,
                    None,
                    BindingPolicyViolationReason::InvalidArgumentDefinition,
                ));
            }
        } else {
            if definition.has_scope != definition.policy.has_scope() {
                return Err(policy_error(
                    id,
                    slot,
                    None,
                    BindingPolicyViolationReason::ScopeFlagMismatch,
                ));
            }
            match definition.scope_next {
                ScopeLink::End => {}
                ScopeLink::ArgumentScopeEnd => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ArgumentScopeMetadataUnsupported {
                            definition: definition_index,
                        },
                    ));
                }
                ScopeLink::Local(target) if target < locals => {
                    let target_definition = &variables[arguments + target as usize];
                    if definition.has_scope != target_definition.has_scope {
                        return Err(BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::ScopeLinkKindMismatch {
                                definition: definition_index,
                                target,
                            },
                        ));
                    }
                }
                ScopeLink::Local(target) => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ScopeLinkOutOfBounds {
                            definition: definition_index,
                            target,
                            locals,
                        },
                    ));
                }
            }
        }
        if let Some(reference) = definition.variable_reference {
            if reference >= variable_references {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::VariableReferenceOutOfBounds {
                        definition: definition_index,
                        reference,
                        len: variable_references,
                    },
                ));
            }
            let seen = &mut seen_references[reference as usize];
            if std::mem::replace(seen, true) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DuplicateVariableReference { reference },
                ));
            }
        }
    }
    initializer_definitions.sort_unstable_by_key(|&(constant, _)| constant);
    for pair in initializer_definitions.windows(2) {
        let [(constant, first), (duplicate_constant, duplicate)] = pair else {
            continue;
        };
        if constant == duplicate_constant {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionInitializerConstantReused {
                    constant: *constant,
                    first: *first,
                    duplicate: *duplicate,
                },
            ));
        }
    }
    verify_scope_links(id, &variables[arguments..])?;
    let captured = usize_to_u32(seen_references.iter().filter(|&&seen| seen).count());
    if captured != variable_references || seen_references.iter().any(|seen| !seen) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableReferenceDomainMismatch {
                declared: variable_references,
                captured,
            },
        ));
    }
    verify_capture_layout(id, function, variables, arguments)
}

#[derive(Clone, Copy, Debug)]
struct FunctionInitializerSite {
    closure_index: usize,
    closure_pc: BytecodePc,
}

struct VerifiedFunctionInitializers {
    put_definitions: Vec<Option<usize>>,
    entry_prefix_end: usize,
}

#[allow(clippy::too_many_lines)]
fn verify_function_initializers(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
    entry_prefix: usize,
) -> Result<VerifiedFunctionInitializers, BytecodeVerificationError> {
    let flow = function.control_flow();
    let instructions = flow.instructions();
    let mut predecessor_counts = try_filled_vec(
        id,
        instructions.len(),
        0_u32,
        BytecodeGraphResource::SourceMappings,
    )?;
    for verified in instructions {
        let successors = verified.successors();
        for successor in [
            successors.fallthrough(),
            successors.branch_target(),
            successors.jump_target(),
        ]
        .into_iter()
        .flatten()
        {
            let count = &mut predecessor_counts[successor.get() as usize];
            *count = count.saturating_add(1);
        }
    }

    let mut sites = try_filled_vec(
        id,
        variables.len(),
        None,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut matches = try_filled_vec(
        id,
        variables.len(),
        0_u32,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut closure_definitions = try_filled_vec(
        id,
        instructions.len(),
        None,
        BytecodeGraphResource::SourceMappings,
    )?;
    let mut put_definitions = try_filled_vec(
        id,
        instructions.len(),
        None,
        BytecodeGraphResource::SourceMappings,
    )?;
    let argument_count = flow.domains().argument_count() as usize;

    for index in 0..instructions.len().saturating_sub(1) {
        let closure = instructions[index].decoded().instruction();
        let Some(constant) = closure_constant(closure.opcode(), closure.operands()) else {
            continue;
        };
        let put = instructions[index + 1].decoded().instruction();
        let Some(definition_index) =
            initializer_put_definition(put.opcode(), put.operands(), argument_count)
        else {
            continue;
        };
        let Some(definition) = variables.get(definition_index) else {
            continue;
        };
        if definition.function_initializer != Some(constant)
            || instructions[index]
                .successors()
                .fallthrough()
                .map(InstructionIndex::get)
                != Some(usize_to_u32(index + 1))
            || predecessor_counts[index + 1] != 1
        {
            continue;
        }
        let count = &mut matches[definition_index];
        *count = count.saturating_add(1);
        if *count == 1 {
            let site = FunctionInitializerSite {
                closure_index: index,
                closure_pc: instructions[index].decoded().pc(),
            };
            sites[definition_index] = Some(site);
            closure_definitions[index] = Some(definition_index);
            put_definitions[index + 1] = Some(definition_index);
        }
    }

    verify_scope_function_initializer_groups(
        id,
        variables,
        instructions,
        &predecessor_counts,
        &closure_definitions,
        &put_definitions,
        argument_count,
    )?;

    let first_instantiation_definition =
        variables
            .iter()
            .enumerate()
            .find_map(|(index, definition)| {
                (definition.function_initializer.is_some()
                    && definition.policy.initialization
                        != CompilerInitializationPolicy::FunctionAtScopeEntry)
                    .then_some(index)
            });
    let mut prefix_index = entry_prefix;
    if first_instantiation_definition.is_some() || entry_prefix != 0 {
        while let Some(verified) = instructions.get(prefix_index) {
            let instruction = verified.decoded().instruction();
            if instruction.opcode() != FinalOpcode::SetLocUninitialized {
                break;
            }
            let expected_predecessors = u32::from(prefix_index != 0);
            if predecessor_counts[prefix_index] != expected_predecessors
                || verified
                    .successors()
                    .fallthrough()
                    .map(InstructionIndex::get)
                    != Some(usize_to_u32(prefix_index + 1))
            {
                if let Some(first_definition) = first_instantiation_definition {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
                            definition: usize_to_u32(first_definition),
                            pc: verified.decoded().pc(),
                        },
                    ));
                }
                let local =
                    local_operand(instruction.opcode(), instruction.operands()).unwrap_or(0);
                return Err(policy_error(
                    id,
                    BindingSlot::Local(local),
                    Some(verified.decoded().pc()),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            prefix_index += 1;
        }
    }
    for (definition_index, definition) in variables.iter().enumerate() {
        let Some(constant) = definition.function_initializer else {
            continue;
        };
        if matches[definition_index] != 1 {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionInitializerOpcodeMismatch {
                    definition: usize_to_u32(definition_index),
                    constant,
                    matches: matches[definition_index],
                },
            ));
        }
        let Some(site) = sites[definition_index] else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionInitializerOpcodeMismatch {
                    definition: usize_to_u32(definition_index),
                    constant,
                    matches: matches[definition_index],
                },
            ));
        };
        if definition.policy.initialization != CompilerInitializationPolicy::FunctionAtScopeEntry {
            let expected_predecessors = u32::from(prefix_index != 0);
            if site.closure_index != prefix_index
                || predecessor_counts[site.closure_index] != expected_predecessors
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
                        definition: usize_to_u32(definition_index),
                        pc: site.closure_pc,
                    },
                ));
            }
            prefix_index = prefix_index.checked_add(2).ok_or_else(|| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
                        definition: usize_to_u32(definition_index),
                        pc: site.closure_pc,
                    },
                )
            })?;
        }
    }

    Ok(VerifiedFunctionInitializers {
        put_definitions,
        entry_prefix_end: prefix_index,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn verify_scope_function_initializer_groups(
    id: FunctionTemplateId,
    variables: &[VariableDefinition],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    closure_definitions: &[Option<usize>],
    put_definitions: &[Option<usize>],
    argument_count: usize,
) -> Result<(), BytecodeVerificationError> {
    let mut activation_epoch = try_filled_vec(
        id,
        variables.len(),
        0_u32,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut verified_sites = try_filled_vec(
        id,
        variables.len(),
        false,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut epoch = 0_u32;
    let mut index = 0_usize;

    while index < instructions.len() {
        if instructions[index].decoded().instruction().opcode() != FinalOpcode::SetLocUninitialized
        {
            index += 1;
            continue;
        }
        let activation_start = index;
        while index < instructions.len()
            && instructions[index].decoded().instruction().opcode()
                == FinalOpcode::SetLocUninitialized
        {
            index += 1;
        }
        let activation_end = index;
        let pair_start = index;
        while index + 1 < instructions.len() {
            let Some(definition) = closure_definitions[index] else {
                break;
            };
            if variables[definition].policy.initialization
                != CompilerInitializationPolicy::FunctionAtScopeEntry
                || put_definitions[index + 1] != Some(definition)
            {
                break;
            }
            index += 2;
        }
        let pair_end = index;
        if pair_start == pair_end {
            continue;
        }

        epoch = epoch.checked_add(1).ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::VariableDefinitions,
                    limit: u64::MAX,
                    observed: u64::MAX,
                },
            )
        })?;
        for activation in &instructions[activation_start..activation_end] {
            let instruction = activation.decoded().instruction();
            let Some(local) = local_operand(instruction.opcode(), instruction.operands()) else {
                continue;
            };
            let definition = argument_count + local as usize;
            if variables[definition].policy.initialization
                == CompilerInitializationPolicy::FunctionAtScopeEntry
            {
                activation_epoch[definition] = epoch;
            }
        }
        for pair in (pair_start..pair_end).step_by(2) {
            let Some(definition) = closure_definitions[pair] else {
                continue;
            };
            if activation_epoch[definition] != epoch {
                return Err(function_initializer_placement_error(
                    id,
                    definition,
                    instructions[pair].decoded().pc(),
                ));
            }
            verified_sites[definition] = true;
        }
        let Some(first_definition) = closure_definitions[pair_start] else {
            continue;
        };
        for edge in activation_start..pair_end {
            if edge != activation_start && predecessor_counts[edge] != 1 {
                return Err(function_initializer_placement_error(
                    id,
                    first_definition,
                    instructions[pair_start].decoded().pc(),
                ));
            }
            if edge + 1 < pair_end
                && instructions[edge]
                    .successors()
                    .fallthrough()
                    .map(InstructionIndex::get)
                    != Some(usize_to_u32(edge + 1))
            {
                return Err(function_initializer_placement_error(
                    id,
                    first_definition,
                    instructions[pair_start].decoded().pc(),
                ));
            }
        }
    }

    for (definition, variable) in variables.iter().enumerate() {
        if variable.policy.initialization == CompilerInitializationPolicy::FunctionAtScopeEntry
            && variable.function_initializer.is_some()
            && !verified_sites[definition]
        {
            let pc = closure_definitions
                .iter()
                .position(|candidate| *candidate == Some(definition))
                .and_then(|index| instructions.get(index))
                .map_or(BytecodePc::new(0), |instruction| instruction.decoded().pc());
            return Err(function_initializer_placement_error(id, definition, pc));
        }
    }
    Ok(())
}

fn function_initializer_placement_error(
    id: FunctionTemplateId,
    definition: usize,
    pc: BytecodePc,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
            definition: usize_to_u32(definition),
            pc,
        },
    )
}

const fn closure_constant(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match (opcode, operands) {
        (FinalOpcode::FClosure, Operands::Const(index)) => Some(index),
        (FinalOpcode::FClosure8, Operands::Const8(index)) => Some(index as u32),
        _ => None,
    }
}

const fn initializer_put_definition(
    opcode: FinalOpcode,
    operands: Operands,
    argument_count: usize,
) -> Option<usize> {
    if matches!(
        opcode,
        FinalOpcode::PutArg
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
    ) {
        return match argument_operand(opcode, operands) {
            Some(index) => Some(index as usize),
            None => None,
        };
    }
    if matches!(
        opcode,
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    ) {
        return match local_operand(opcode, operands) {
            Some(index) => argument_count.checked_add(index as usize),
            None => None,
        };
    }
    None
}

fn verify_scope_links(
    id: FunctionTemplateId,
    locals: &[VariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let mut states = try_filled_vec(
        id,
        locals.len(),
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut path = Vec::new();
    path.try_reserve_exact(locals.len()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource: BytecodeGraphResource::VariableDefinitions,
                requested: usize_to_u64(locals.len()),
            },
        )
    })?;
    for start in 0..locals.len() {
        if states[start] == 2 {
            continue;
        }
        path.clear();
        let mut current = start;
        loop {
            match states[current] {
                2 => break,
                1 => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ScopeLinkCycle {
                            local: usize_to_u32(current),
                        },
                    ));
                }
                _ => {
                    states[current] = 1;
                    path.push(current);
                }
            }
            match locals[current].scope_next {
                ScopeLink::End => break,
                ScopeLink::ArgumentScopeEnd => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ArgumentScopeMetadataUnsupported {
                            definition: usize_to_u32(current),
                        },
                    ));
                }
                ScopeLink::Local(next) => {
                    current = next as usize;
                }
            }
        }
        for local in path.drain(..) {
            states[local] = 2;
        }
    }
    Ok(())
}

fn verify_capture_layout(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
    arguments: usize,
) -> Result<(), BytecodeVerificationError> {
    let Some(layout) = function.control_flow().compiler_capture_layout() else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableReferenceDomainMismatch {
                declared: function
                    .control_flow()
                    .function_header()
                    .variable_reference_count(),
                captured: 0,
            },
        ));
    };
    for (index, definition) in variables.iter().enumerate() {
        let Some(reference) = definition.variable_reference else {
            continue;
        };
        let expected = if index < arguments {
            CompilerCapturedBinding::Argument(usize_to_u32(index))
        } else if definition.has_scope {
            CompilerCapturedBinding::ScopedLocal(usize_to_u32(index - arguments))
        } else {
            CompilerCapturedBinding::FunctionLocal(usize_to_u32(index - arguments))
        };
        if layout.binding_for_variable_reference(reference) != Some(expected) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::CaptureLayoutMismatch { reference },
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RealmGlobalFunctionInitializerSite {
    closure_index: usize,
    closure_pc: BytecodePc,
}

#[allow(
    clippy::too_many_lines,
    reason = "global function initializer matching and entry placement form one authority check"
)]
fn verify_realm_global_function_initializers(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    closures: &[ClosureVariableDefinition],
) -> Result<usize, BytecodeVerificationError> {
    if !closures
        .iter()
        .any(|definition| definition.function_initializer.is_some())
    {
        return Ok(0);
    }
    if id != root {
        let closure = closures
            .iter()
            .position(|definition| definition.function_initializer.is_some())
            .map_or(0, usize_to_u32);
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                closure,
                constant: closures
                    .get(closure as usize)
                    .and_then(ClosureVariableDefinition::function_initializer),
            },
        ));
    }

    let instructions = function.control_flow().instructions();
    let mut predecessor_counts = try_filled_vec(
        id,
        instructions.len(),
        0_u32,
        BytecodeGraphResource::SourceMappings,
    )?;
    for instruction in instructions {
        let successors = instruction.successors();
        for successor in [
            successors.fallthrough(),
            successors.branch_target(),
            successors.jump_target(),
        ]
        .into_iter()
        .flatten()
        {
            predecessor_counts[successor.get() as usize] =
                predecessor_counts[successor.get() as usize].saturating_add(1);
        }
    }

    let mut sites = try_filled_vec(
        id,
        closures.len(),
        None,
        BytecodeGraphResource::ClosureDefinitions,
    )?;
    let mut matches = try_filled_vec(
        id,
        closures.len(),
        0_u32,
        BytecodeGraphResource::ClosureDefinitions,
    )?;
    for index in 0..instructions.len().saturating_sub(1) {
        let closure_instruction = instructions[index].decoded().instruction();
        let Some(constant) =
            closure_constant(closure_instruction.opcode(), closure_instruction.operands())
        else {
            continue;
        };
        let put_instruction = instructions[index + 1].decoded().instruction();
        let (FinalOpcode::PutVar, Operands::VarRef(closure)) =
            (put_instruction.opcode(), put_instruction.operands())
        else {
            continue;
        };
        let Some(definition) = closures.get(closure as usize) else {
            continue;
        };
        if definition.function_initializer != Some(constant)
            || instructions[index]
                .successors()
                .fallthrough()
                .map(InstructionIndex::get)
                != Some(usize_to_u32(index + 1))
            || predecessor_counts[index + 1] != 1
        {
            continue;
        }
        matches[closure as usize] = matches[closure as usize].saturating_add(1);
        if matches[closure as usize] == 1 {
            sites[closure as usize] = Some(RealmGlobalFunctionInitializerSite {
                closure_index: index,
                closure_pc: instructions[index].decoded().pc(),
            });
        }
    }

    let mut prefix_index = 0_usize;
    for (closure, definition) in closures.iter().enumerate() {
        let Some(constant) = definition.function_initializer else {
            continue;
        };
        if matches[closure] != 1 {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
                    closure: usize_to_u32(closure),
                    constant,
                    matches: matches[closure],
                },
            ));
        }
        let Some(site) = sites[closure] else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
                    closure: usize_to_u32(closure),
                    constant,
                    matches: matches[closure],
                },
            ));
        };
        let expected_predecessors = u32::from(prefix_index != 0);
        if site.closure_index != prefix_index
            || predecessor_counts[site.closure_index] != expected_predecessors
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerPlacementMismatch {
                    closure: usize_to_u32(closure),
                    pc: site.closure_pc,
                },
            ));
        }
        prefix_index = prefix_index.checked_add(2).ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerPlacementMismatch {
                    closure: usize_to_u32(closure),
                    pc: site.closure_pc,
                },
            )
        })?;
    }
    Ok(prefix_index)
}

fn verify_closures(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    authority_kind: CompilerExecutableKind,
    function: &VerifiedCompilerFunction,
    closures: &[ClosureVariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    for (index, (closure, staged_source)) in
        closures.iter().zip(function.closure_sources()).enumerate()
    {
        let slot = BindingSlot::Closure(usize_to_u32(index));
        verify_required_atom(
            id,
            closure.name,
            MetadataAtomField::ClosureName(usize_to_u32(index)),
            function,
        )?;
        let policy = closure.policy();
        let binding_valid = match closure.binding {
            CompilerClosureBinding::Captured(_) => {
                policy.is_valid()
                    && policy.kind() != CompilerBindingKind::GlobalReference
                    && closure.function_initializer.is_none()
                    && matches!(
                        staged_source,
                        CompilerClosureSource::ParentVariableReference(_)
                            | CompilerClosureSource::ParentClosure(_)
                    )
            }
            CompilerClosureBinding::RealmGlobal(_) => {
                realm_global_policy_supported(policy)
                    && match *staged_source {
                        CompilerClosureSource::ConstructorRealmGlobal(atom) => {
                            if id != root
                                || authority_kind != CompilerExecutableKind::DynamicFunctionScript
                            {
                                return Err(BytecodeVerificationError::function(
                                    id,
                                    BytecodeVerificationErrorKind::ConstructorRealmGlobalSourceRequiresDynamicFunctionScript {
                                        closure: usize_to_u32(index),
                                    },
                                ));
                            }
                            closure.name == Some(atom)
                        }
                        CompilerClosureSource::ParentClosure(_) => id != root,
                        CompilerClosureSource::ParentVariableReference(_) => false,
                    }
            }
        };
        if !binding_valid {
            return Err(policy_error(
                id,
                slot,
                None,
                BindingPolicyViolationReason::InvalidDeclarationPolicy,
            ));
        }
        let realm_global_function = matches!(
            closure.binding,
            CompilerClosureBinding::RealmGlobal(policy)
                if policy.kind() == CompilerBindingKind::Function
        );
        let originates_in_constructor_realm = matches!(
            staged_source,
            CompilerClosureSource::ConstructorRealmGlobal(_)
        );
        let initializer_valid = match (
            realm_global_function,
            originates_in_constructor_realm,
            closure.function_initializer,
        ) {
            (true, true, Some(constant)) => matches!(
                function.constants().get(constant as usize),
                Some(crate::CompilerConstant::Function(_))
            ),
            (true, false, None) | (false, _, None) => true,
            (true, true, None) | (true, false, Some(_)) | (false, _, Some(_)) => false,
        };
        if !initializer_valid {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                    closure: usize_to_u32(index),
                    constant: closure.function_initializer,
                },
            ));
        }
        if closure.source != *staged_source {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                    child: id,
                    closure: usize_to_u32(index),
                },
            ));
        }
    }
    Ok(())
}

const fn realm_global_policy_supported(policy: CompilerBindingPolicy) -> bool {
    match policy.kind() {
        CompilerBindingKind::GlobalReference => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::ConstructorRealmLookup
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && !policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Var => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::UndefinedAtInstantiation
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && !policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Function => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::FunctionAtInstantiation
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && !policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Parameter
        | CompilerBindingKind::FunctionName
        | CompilerBindingKind::Catch
        | CompilerBindingKind::Let
        | CompilerBindingKind::Const => false,
    }
}

fn verify_optional_atom(
    id: FunctionTemplateId,
    atom: Option<AtomPoolIndex>,
    field: MetadataAtomField,
    function: &VerifiedCompilerFunction,
) -> Result<(), BytecodeVerificationError> {
    if let Some(atom) = atom {
        verify_atom_bounds(id, atom, field, function)?;
    }
    Ok(())
}

fn verify_required_atom(
    id: FunctionTemplateId,
    atom: Option<AtomPoolIndex>,
    field: MetadataAtomField,
    function: &VerifiedCompilerFunction,
) -> Result<(), BytecodeVerificationError> {
    let atom = atom.ok_or_else(|| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::MissingMetadataAtom { field },
        )
    })?;
    verify_atom_bounds(id, atom, field, function)
}

fn verify_atom_bounds(
    id: FunctionTemplateId,
    atom: AtomPoolIndex,
    field: MetadataAtomField,
    function: &VerifiedCompilerFunction,
) -> Result<(), BytecodeVerificationError> {
    let len = usize_to_u32(function.atoms().len());
    if atom.get() >= len {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::MetadataAtomOutOfBounds {
                field,
                index: atom.get(),
                len,
            },
        ));
    }
    Ok(())
}

fn verify_source(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    let source = &metadata.source;
    if source.display_name.is_empty() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::EmptySourceDisplayName,
        ));
    }
    validate_source_span(id, &source.text, source.function_span)?;
    if metadata.function_name.is_some() != source.name_span.is_some() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::FunctionNameSourceMismatch,
        ));
    }
    if let Some(name_span) = source.name_span {
        validate_source_span(id, &source.text, name_span)?;
        if !contains(source.function_span, name_span) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionNameOutsideFunction,
            ));
        }
    }
    if source.mappings.len() != flow.instructions().len() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::SourceMappingCountMismatch {
                instructions: usize_to_u64(flow.instructions().len()),
                mappings: usize_to_u64(source.mappings.len()),
            },
        ));
    }
    for (index, (mapping, instruction)) in
        source.mappings.iter().zip(flow.instructions()).enumerate()
    {
        let actual = instruction.decoded().pc();
        if mapping.pc != actual {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::SourcePcMismatch {
                    mapping: usize_to_u32(index),
                    declared: mapping.pc,
                    actual,
                },
            ));
        }
        validate_source_span(id, &source.text, mapping.span)?;
        if !contains(source.function_span, mapping.span) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::InstructionSourceOutsideFunction {
                    mapping: usize_to_u32(index),
                },
            ));
        }
    }
    Ok(())
}

fn validate_source_span(
    id: FunctionTemplateId,
    text: &str,
    span: SourceByteSpan,
) -> Result<(), BytecodeVerificationError> {
    let start = span.start as usize;
    let end = span.end as usize;
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::InvalidSourceSpan { span },
        ));
    }
    Ok(())
}

const fn contains(outer: SourceByteSpan, inner: SourceByteSpan) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

#[allow(
    clippy::too_many_lines,
    reason = "parent-child variable, global initializer, and forwarded closure metadata are verified together"
)]
fn verify_closure_metadata(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
) -> Result<(), BytecodeVerificationError> {
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let parent_metadata = &metadata[parent_index];
        for (definition_index, definition) in parent_metadata.variables.iter().enumerate() {
            let Some(constant_index) = definition.function_initializer else {
                continue;
            };
            let Some(crate::CompilerConstant::Function(child_id)) =
                parent.constants().get(constant_index as usize)
            else {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: usize_to_u32(definition_index),
                        constant: Some(constant_index),
                    },
                ));
            };
            let child_index = usize::try_from(child_id.get()).ok();
            let child = child_index.and_then(|index| graph.functions().get(index));
            let child_metadata = child_index.and_then(|index| metadata.get(index));
            let names_match = child
                .zip(child_metadata)
                .is_some_and(|(child, child_metadata)| {
                    atom_contents(definition.name, parent.atoms())
                        == atom_contents(child_metadata.function_name, child.atoms())
                });
            if !names_match {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: usize_to_u32(definition_index),
                        constant: Some(constant_index),
                    },
                ));
            }
        }
        for (closure_index, definition) in parent_metadata.closures.iter().enumerate() {
            let Some(constant_index) = definition.function_initializer else {
                continue;
            };
            let Some(crate::CompilerConstant::Function(child_id)) =
                parent.constants().get(constant_index as usize)
            else {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                        closure: usize_to_u32(closure_index),
                        constant: Some(constant_index),
                    },
                ));
            };
            let child_index = usize::try_from(child_id.get()).ok();
            let child = child_index.and_then(|index| graph.functions().get(index));
            let child_metadata = child_index.and_then(|index| metadata.get(index));
            let names_match = child
                .zip(child_metadata)
                .is_some_and(|(child, child_metadata)| {
                    atom_contents(definition.name, parent.atoms())
                        == atom_contents(child_metadata.function_name, child.atoms())
                });
            if !names_match {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                        closure: usize_to_u32(closure_index),
                        constant: Some(constant_index),
                    },
                ));
            }
        }
        for constant in parent.constants() {
            let crate::CompilerConstant::Function(child_id) = constant else {
                continue;
            };
            let child_index = usize::try_from(child_id.get()).map_err(|_| {
                BytecodeVerificationError::function(
                    *child_id,
                    BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                        child: *child_id,
                        closure: 0,
                    },
                )
            })?;
            let child = graph.function(*child_id).ok_or_else(|| {
                BytecodeVerificationError::function(
                    *child_id,
                    BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                        child: *child_id,
                        closure: 0,
                    },
                )
            })?;
            let child_metadata = &metadata[child_index];
            for (closure_index, (closure, source)) in child_metadata
                .closures
                .iter()
                .zip(child.closure_sources())
                .enumerate()
            {
                let expected = match *source {
                    CompilerClosureSource::ParentVariableReference(reference) => {
                        parent_definition_for_reference(parent, parent_metadata, reference)
                    }
                    CompilerClosureSource::ParentClosure(index) => usize::try_from(index)
                        .ok()
                        .and_then(|index| parent_metadata.closures.get(index))
                        .map(|definition| (definition.name, definition.binding, parent.atoms())),
                    CompilerClosureSource::ConstructorRealmGlobal(_) => None,
                };
                let matches =
                    expected.is_some_and(|(expected_name, expected_binding, expected_atoms)| {
                        expected_binding == closure.binding
                            && atom_contents(expected_name, expected_atoms)
                                == atom_contents(closure.name, child.atoms())
                    });
                if !matches {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                }
            }
        }
    }
    Ok(())
}

fn parent_definition_for_reference<'metadata>(
    parent: &'metadata VerifiedCompilerFunction,
    metadata: &'metadata VerifiedFunctionMetadata,
    reference: u32,
) -> Option<(
    Option<AtomPoolIndex>,
    CompilerClosureBinding,
    &'metadata [crate::CompilerAtom],
)> {
    let binding = parent
        .control_flow()
        .compiler_capture_layout()?
        .binding_for_variable_reference(reference)?;
    let arguments = parent.control_flow().domains().argument_count() as usize;
    let index = match binding {
        CompilerCapturedBinding::Argument(index) => usize::try_from(index).ok()?,
        CompilerCapturedBinding::FunctionLocal(index)
        | CompilerCapturedBinding::ScopedLocal(index) => {
            arguments.checked_add(usize::try_from(index).ok()?)?
        }
    };
    let definition = metadata.variables.get(index)?;
    (definition.variable_reference == Some(reference)).then_some((
        definition.name,
        CompilerClosureBinding::Captured(definition.policy),
        parent.atoms(),
    ))
}

fn atom_contents(
    atom: Option<AtomPoolIndex>,
    atoms: &[crate::CompilerAtom],
) -> Option<&crate::CompilerString> {
    let index = usize::try_from(atom?.get()).ok()?;
    atoms.get(index).map(crate::CompilerAtom::string)
}

fn verify_supported_opcodes(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    _executable_kind: CompilerExecutableKind,
    authority_kind: CompilerExecutableKind,
) -> Result<(), BytecodeVerificationError> {
    for instruction in flow.instructions() {
        let decoded = instruction.decoded();
        let opcode = decoded.instruction().opcode();
        if !supported_compiler_opcode(opcode)
            || (opcode == FinalOpcode::PushThis
                && !flow.function_header().mode().is_strict()
                && authority_kind != CompilerExecutableKind::DynamicFunctionScript)
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                    pc: decoded.pc(),
                    opcode,
                },
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
const fn supported_compiler_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PushI32
            | FinalOpcode::PushConst
            | FinalOpcode::FClosure
            | FinalOpcode::PushAtomValue
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
            | FinalOpcode::Drop
            | FinalOpcode::Dup
            | FinalOpcode::Insert2
            | FinalOpcode::CallConstructor
            | FinalOpcode::Call
            | FinalOpcode::CallMethod
            | FinalOpcode::Return
            | FinalOpcode::ReturnUndef
            | FinalOpcode::Throw
            | FinalOpcode::GetVarUndef
            | FinalOpcode::GetVar
            | FinalOpcode::PutVar
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
            | FinalOpcode::GetVarRefCheck
            | FinalOpcode::PutVarRefCheck
            | FinalOpcode::CloseLoc
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::PutField
            | FinalOpcode::DefineField
            | FinalOpcode::IfFalse
            | FinalOpcode::IfTrue
            | FinalOpcode::Goto
            | FinalOpcode::Neg
            | FinalOpcode::Plus
            | FinalOpcode::Dec
            | FinalOpcode::Inc
            | FinalOpcode::PostDec
            | FinalOpcode::PostInc
            | FinalOpcode::Not
            | FinalOpcode::Lnot
            | FinalOpcode::Typeof
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
            | FinalOpcode::PushConst8
            | FinalOpcode::FClosure8
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
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::IfFalse8
            | FinalOpcode::IfTrue8
            | FinalOpcode::Goto8
            | FinalOpcode::Goto16
    )
}

fn verify_binding_opcodes(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
    closures: &[ClosureVariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let argument_count = flow.domains().argument_count() as usize;
    let mut scope_activations = try_filled_vec(
        id,
        variables.len() - argument_count,
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    for verified in flow.instructions() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if opcode == FinalOpcode::DeleteVar {
            let Operands::Atom(atom) = instruction.operands() else {
                continue;
            };
            let has_binding = closures.iter().any(|definition| {
                definition.name == Some(atom)
                    && matches!(
                        definition.binding,
                        CompilerClosureBinding::RealmGlobal(policy)
                            if policy.kind() == CompilerBindingKind::GlobalReference
                    )
            });
            if !has_binding {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::RealmGlobalDeleteBindingMissing {
                        pc: decoded.pc(),
                        atom,
                    },
                ));
            }
        } else if let Some(local) = local_operand(opcode, instruction.operands()) {
            let definition = &variables[argument_count + local as usize];
            verify_local_opcode(id, decoded.pc(), local, opcode, definition)?;
            if matches!(opcode, FinalOpcode::SetLocUninitialized) {
                let count = &mut scope_activations[local as usize];
                *count = count.saturating_add(1);
                if *count > 1 {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(decoded.pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
            }
        } else if let Some(argument) = argument_operand(opcode, instruction.operands()) {
            let definition = &variables[argument as usize];
            if is_argument_write(opcode) && definition.policy.writes != CompilerWritePolicy::Mutable
            {
                return Err(policy_error(
                    id,
                    BindingSlot::Argument(argument),
                    Some(decoded.pc()),
                    BindingPolicyViolationReason::ImmutableWrite,
                ));
            }
        } else if let Some(closure) = closure_operand(opcode, instruction.operands()) {
            let definition = &closures[closure as usize];
            verify_closure_opcode(id, decoded.pc(), closure, opcode, definition)?;
        }
    }
    for (local, (definition, activations)) in variables[argument_count..]
        .iter()
        .zip(scope_activations)
        .enumerate()
    {
        let requires_scope_activation = definition.policy.temporal_dead_zone
            || definition.policy.initialization
                == CompilerInitializationPolicy::FunctionAtScopeEntry;
        if requires_scope_activation && activations != 1 {
            return Err(policy_error(
                id,
                BindingSlot::Local(usize_to_u32(local)),
                None,
                BindingPolicyViolationReason::MissingLexicalScopeInitialization,
            ));
        }
    }
    Ok(())
}

fn verify_local_opcode(
    id: FunctionTemplateId,
    pc: BytecodePc,
    local: u32,
    opcode: FinalOpcode,
    definition: &VariableDefinition,
) -> Result<(), BytecodeVerificationError> {
    let slot = BindingSlot::Local(local);
    let tdz = definition.policy.temporal_dead_zone;
    if matches!(opcode, FinalOpcode::SetLocUninitialized) {
        if !tdz
            && definition.policy.initialization
                != CompilerInitializationPolicy::FunctionAtScopeEntry
        {
            return Err(policy_error(
                id,
                slot,
                Some(pc),
                BindingPolicyViolationReason::UnexpectedCheckedAccess,
            ));
        }
        return Ok(());
    }
    if is_checked_local(opcode) && !tdz {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            BindingPolicyViolationReason::UnexpectedCheckedAccess,
        ));
    }
    if is_local_write(opcode)
        && !matches!(opcode, FinalOpcode::SetLocUninitialized)
        && definition.policy.writes != CompilerWritePolicy::Mutable
        && !(tdz && is_unchecked_local_put(opcode))
    {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            BindingPolicyViolationReason::ImmutableWrite,
        ));
    }
    Ok(())
}

fn verify_closure_opcode(
    id: FunctionTemplateId,
    pc: BytecodePc,
    closure: u32,
    opcode: FinalOpcode,
    definition: &ClosureVariableDefinition,
) -> Result<(), BytecodeVerificationError> {
    match definition.binding {
        CompilerClosureBinding::Captured(_) if is_realm_global_opcode(opcode) => {
            return Err(closure_opcode_mismatch(id, pc, closure, opcode));
        }
        CompilerClosureBinding::RealmGlobal(policy) => {
            if !is_realm_global_opcode(opcode) {
                return Err(closure_opcode_mismatch(id, pc, closure, opcode));
            }
            let allowed = match policy.kind() {
                CompilerBindingKind::GlobalReference
                | CompilerBindingKind::Var
                | CompilerBindingKind::Function => matches!(
                    opcode,
                    FinalOpcode::GetVarUndef | FinalOpcode::GetVar | FinalOpcode::PutVar
                ),
                _ => false,
            };
            if !allowed {
                if opcode == FinalOpcode::PutVar && policy.writes() != CompilerWritePolicy::Mutable
                {
                    return Err(policy_error(
                        id,
                        BindingSlot::Closure(closure),
                        Some(pc),
                        BindingPolicyViolationReason::ImmutableWrite,
                    ));
                }
                return Err(closure_opcode_mismatch(id, pc, closure, opcode));
            }
            return Ok(());
        }
        CompilerClosureBinding::Captured(_) => {}
    }

    let slot = BindingSlot::Closure(closure);
    let policy = definition.policy();
    let checked = matches!(
        opcode,
        FinalOpcode::GetVarRefCheck | FinalOpcode::PutVarRefCheck
    );
    if checked != policy.temporal_dead_zone {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            if checked {
                BindingPolicyViolationReason::UnexpectedCheckedAccess
            } else {
                BindingPolicyViolationReason::UncheckedTemporalDeadZoneAccess
            },
        ));
    }
    if is_closure_write(opcode) && policy.writes != CompilerWritePolicy::Mutable {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            BindingPolicyViolationReason::ImmutableWrite,
        ));
    }
    Ok(())
}

const fn is_realm_global_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetVarUndef | FinalOpcode::GetVar | FinalOpcode::PutVar
    )
}

fn closure_opcode_mismatch(
    id: FunctionTemplateId,
    pc: BytecodePc,
    closure: u32,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ClosureBindingOpcodeMismatch {
            closure,
            pc,
            opcode,
        },
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "binding-state analysis requires the complete verified function and entry authority"
)]
fn verify_binding_states(
    id: FunctionTemplateId,
    graph: &VerifiedCompilerFunctionGraph,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
    initializers: &VerifiedFunctionInitializers,
    realm_global_initializer_prefix: usize,
    prior_transfers: u64,
    transfer_limit: u64,
) -> Result<u64, BytecodeVerificationError> {
    let flow = function.control_flow();
    let arguments = flow.domains().argument_count() as usize;
    let mut tracked = Vec::new();
    tracked
        .try_reserve_exact(variables.len() - arguments)
        .map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    requested: usize_to_u64(variables.len() - arguments),
                },
            )
        })?;
    tracked.extend(
        variables[arguments..]
            .iter()
            .enumerate()
            .filter_map(|(local, definition)| {
                requires_binding_state(definition).then_some((local, definition))
            }),
    );
    if tracked.is_empty() {
        return Ok(0);
    }
    let mut tracked_by_local = try_filled_vec(
        id,
        variables.len() - arguments,
        None,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    for (position, (local, _)) in tracked.iter().enumerate() {
        tracked_by_local[*local] = Some(position);
    }
    let instructions = flow.instructions();
    let state_cells = instructions
        .len()
        .checked_mul(tracked.len())
        .ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    limit: u64::MAX,
                    observed: u64::MAX,
                },
            )
        })?;
    let mut entries = try_filled_vec(
        id,
        state_cells,
        0_u8,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    for (entry, (_, definition)) in entries[..tracked.len()].iter_mut().zip(&tracked) {
        *entry = initial_binding_state(definition);
    }
    let mut entry_present = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    entry_present[0] = true;
    let mut queued = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    queued[0] = true;
    let mut work = VecDeque::new();
    work.try_reserve_exact(instructions.len()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource: BytecodeGraphResource::PolicyTransfers,
                requested: usize_to_u64(instructions.len()),
            },
        )
    })?;
    work.push_back(0_usize);
    let mut evaluations = 0_u64;
    while let Some(index) = work.pop_front() {
        queued[index] = false;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(tracked.len()),
            prior_transfers,
            transfer_limit,
        )?;
        let start = index * tracked.len();
        let state = &entries[start..start + tracked.len()];
        let mut state = try_copy_slice(id, state, BytecodeGraphResource::FrameStateEntries)?;
        if realm_global_initializer_prefix != 0 && index == initializers.entry_prefix_end {
            for (position, (local, _)) in tracked.iter().enumerate() {
                if state[position] & BindingState::VALUE_INACTIVE != 0
                    && state[position] & BindingState::CELL_ACTIVE != 0
                {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(usize_to_u32(*local)),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                    ));
                }
            }
        }
        let instruction = instructions[index].decoded().instruction();
        let opcode = instruction.opcode();
        if let Some(constant) = closure_constant(opcode, instruction.operands())
            && let Some(crate::CompilerConstant::Function(child_id)) =
                function.constants().get(constant as usize)
            && let Some(child) = graph.function(*child_id)
        {
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(child.closure_sources().len()),
                prior_transfers,
                transfer_limit,
            )?;
            let capture_layout = flow.compiler_capture_layout();
            for source in child.closure_sources() {
                let CompilerClosureSource::ParentVariableReference(reference) = *source else {
                    continue;
                };
                let Some(CompilerCapturedBinding::ScopedLocal(local)) = capture_layout
                    .and_then(|layout| layout.binding_for_variable_reference(reference))
                else {
                    continue;
                };
                let Some(position) = tracked_by_local[local as usize] else {
                    continue;
                };
                let certified_realm_global_initializer =
                    index < realm_global_initializer_prefix && index % 2 == 0;
                if state[position] & BindingState::VALUE_INACTIVE != 0
                    && !certified_realm_global_initializer
                {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                    ));
                }
                state[position] =
                    (state[position] & BindingState::VALUE_MASK) | BindingState::CELL_ACTIVE;
            }
        }
        let mut normal_completion_possible = true;
        if let Some(local) = local_operand(opcode, instruction.operands())
            && let Some(position) = tracked_by_local[local as usize]
        {
            let definition_index = arguments + local as usize;
            normal_completion_possible = transfer_local_state(
                id,
                instructions[index].decoded().pc(),
                local,
                opcode,
                tracked[position].1,
                initializers.put_definitions[index] == Some(definition_index),
                &mut state[position],
            )?;
        }
        if !normal_completion_possible {
            continue;
        }
        let successors = instructions[index].successors();
        for successor in [
            successors.fallthrough(),
            successors.branch_target(),
            successors.jump_target(),
        ]
        .into_iter()
        .flatten()
        {
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(tracked.len()),
                prior_transfers,
                transfer_limit,
            )?;
            propagate_binding_state(
                successor,
                &state,
                &mut entries,
                &mut entry_present,
                tracked.len(),
                &mut queued,
                &mut work,
            );
        }
    }
    Ok(evaluations)
}

fn charge_policy_transfers(
    id: FunctionTemplateId,
    evaluated: &mut u64,
    amount: u64,
    prior: u64,
    limit: u64,
) -> Result<(), BytecodeVerificationError> {
    let Some(local) = evaluated.checked_add(amount) else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::PolicyTransfers,
                limit,
                observed: u64::MAX,
            },
        ));
    };
    let Some(observed) = prior.checked_add(local) else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::PolicyTransfers,
                limit,
                observed: u64::MAX,
            },
        ));
    };
    if observed > limit {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::PolicyTransfers,
                limit,
                observed,
            },
        ));
    }
    *evaluated = local;
    Ok(())
}

struct BindingState;

impl BindingState {
    const VALUE_INACTIVE: u8 = 1;
    const VALUE_UNINITIALIZED: u8 = 2;
    const VALUE_INITIALIZED: u8 = 4;
    const VALUE_MASK: u8 =
        Self::VALUE_INACTIVE | Self::VALUE_UNINITIALIZED | Self::VALUE_INITIALIZED;
    const CELL_CLOSED: u8 = 8;
    const CELL_ACTIVE: u8 = 16;
    const CELL_MASK: u8 = Self::CELL_CLOSED | Self::CELL_ACTIVE;
    const ENTRY: u8 = Self::VALUE_INACTIVE | Self::CELL_CLOSED;
}

fn requires_binding_state(definition: &VariableDefinition) -> bool {
    definition.policy.temporal_dead_zone
        || (definition.has_scope && definition.variable_reference.is_some())
        || definition.function_initializer.is_some()
        || definition.policy.initialization == CompilerInitializationPolicy::FunctionName
}

fn initial_binding_state(definition: &VariableDefinition) -> u8 {
    if definition.policy.initialization == CompilerInitializationPolicy::FunctionName {
        let cell = if definition.variable_reference.is_some() {
            BindingState::CELL_ACTIVE
        } else {
            BindingState::CELL_CLOSED
        };
        BindingState::VALUE_INITIALIZED | cell
    } else {
        BindingState::ENTRY
    }
}

fn transfer_local_state(
    id: FunctionTemplateId,
    pc: BytecodePc,
    local: u32,
    opcode: FinalOpcode,
    definition: &VariableDefinition,
    is_function_initializer: bool,
    state: &mut u8,
) -> Result<bool, BytecodeVerificationError> {
    let slot = BindingSlot::Local(local);
    match opcode {
        FinalOpcode::SetLocUninitialized => {
            if definition.has_scope
                && definition.variable_reference.is_some()
                && *state & BindingState::CELL_ACTIVE != 0
                && *state & (BindingState::VALUE_UNINITIALIZED | BindingState::VALUE_INITIALIZED)
                    != 0
            {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            let cell = if definition.has_scope && definition.variable_reference.is_some() {
                BindingState::CELL_ACTIVE
            } else {
                *state & BindingState::CELL_MASK
            };
            *state = BindingState::VALUE_UNINITIALIZED | cell;
        }
        opcode if is_unchecked_local_put(opcode) => {
            let value = *state & BindingState::VALUE_MASK;
            let cell = *state & BindingState::CELL_MASK;
            let valid = if is_function_initializer {
                match definition.policy.initialization {
                    CompilerInitializationPolicy::FunctionAtScopeEntry => {
                        value == BindingState::VALUE_UNINITIALIZED
                            && (definition.variable_reference.is_none()
                                || cell == BindingState::CELL_ACTIVE)
                    }
                    CompilerInitializationPolicy::FunctionAtInstantiation
                    | CompilerInitializationPolicy::Argument => {
                        value == BindingState::VALUE_INACTIVE
                    }
                    _ => false,
                }
            } else if definition.function_initializer.is_some()
                && value != BindingState::VALUE_INITIALIZED
            {
                false
            } else if definition.policy.writes == CompilerWritePolicy::Mutable {
                value & BindingState::VALUE_INACTIVE == 0
            } else {
                value == BindingState::VALUE_UNINITIALIZED
            };
            if !valid {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            *state = BindingState::VALUE_INITIALIZED | (*state & BindingState::CELL_MASK);
        }
        FinalOpcode::GetLocCheck | FinalOpcode::PutLocCheck | FinalOpcode::SetLocCheck => {
            if *state & BindingState::VALUE_INACTIVE != 0 {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                ));
            }
            if *state & BindingState::VALUE_INITIALIZED == 0 {
                return Ok(false);
            }
            *state = BindingState::VALUE_INITIALIZED | (*state & BindingState::CELL_MASK);
        }
        FinalOpcode::CloseLoc => {
            *state = (*state & BindingState::VALUE_MASK) | BindingState::CELL_CLOSED;
        }
        opcode
            if (is_local_read(opcode) || is_local_write(opcode))
                && *state & BindingState::VALUE_MASK != BindingState::VALUE_INITIALIZED =>
        {
            return Err(policy_error(
                id,
                slot,
                Some(pc),
                BindingPolicyViolationReason::UncheckedTemporalDeadZoneAccess,
            ));
        }
        _ => {}
    }
    Ok(true)
}

fn propagate_binding_state(
    successor: InstructionIndex,
    output: &[u8],
    entries: &mut [u8],
    entry_present: &mut [bool],
    state_width: usize,
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
) {
    let index = successor.get() as usize;
    let start = index * state_width;
    let existing = &mut entries[start..start + state_width];
    let changed = if entry_present[index] {
        let mut changed = false;
        for (target, incoming) in existing.iter_mut().zip(output) {
            let merged = *target | *incoming;
            changed |= merged != *target;
            *target = merged;
        }
        changed
    } else {
        existing.copy_from_slice(output);
        entry_present[index] = true;
        true
    };
    if changed && !queued[index] {
        queued[index] = true;
        work.push_back(index);
    }
}

const fn local_operand(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match operands {
        Operands::Loc(index) => Some(index as u32),
        Operands::Loc8(index) => Some(index as u32),
        Operands::NoneLoc => implied_local_index(opcode),
        _ => None,
    }
}

const fn argument_operand(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match operands {
        Operands::Arg(index) => Some(index as u32),
        Operands::NoneArg => implied_argument_index(opcode),
        _ => None,
    }
}

const fn closure_operand(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match operands {
        Operands::VarRef(index) => Some(index as u32),
        Operands::NoneVarRef => implied_closure_index(opcode),
        _ => None,
    }
}

const fn implied_local_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetLoc0 | FinalOpcode::PutLoc0 | FinalOpcode::SetLoc0 => Some(0),
        FinalOpcode::GetLoc1 | FinalOpcode::PutLoc1 | FinalOpcode::SetLoc1 => Some(1),
        FinalOpcode::GetLoc2 | FinalOpcode::PutLoc2 | FinalOpcode::SetLoc2 => Some(2),
        FinalOpcode::GetLoc3 | FinalOpcode::PutLoc3 | FinalOpcode::SetLoc3 => Some(3),
        _ => None,
    }
}

const fn implied_argument_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetArg0 | FinalOpcode::PutArg0 | FinalOpcode::SetArg0 => Some(0),
        FinalOpcode::GetArg1 | FinalOpcode::PutArg1 | FinalOpcode::SetArg1 => Some(1),
        FinalOpcode::GetArg2 | FinalOpcode::PutArg2 | FinalOpcode::SetArg2 => Some(2),
        FinalOpcode::GetArg3 | FinalOpcode::PutArg3 | FinalOpcode::SetArg3 => Some(3),
        _ => None,
    }
}

const fn implied_closure_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetVarRef0 | FinalOpcode::PutVarRef0 | FinalOpcode::SetVarRef0 => Some(0),
        FinalOpcode::GetVarRef1 | FinalOpcode::PutVarRef1 | FinalOpcode::SetVarRef1 => Some(1),
        FinalOpcode::GetVarRef2 | FinalOpcode::PutVarRef2 | FinalOpcode::SetVarRef2 => Some(2),
        FinalOpcode::GetVarRef3 | FinalOpcode::PutVarRef3 | FinalOpcode::SetVarRef3 => Some(3),
        _ => None,
    }
}

const fn is_checked_local(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetLocCheck | FinalOpcode::PutLocCheck | FinalOpcode::SetLocCheck
    )
}

const fn is_unchecked_local_put(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    )
}

const fn is_local_write(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutLoc
            | FinalOpcode::SetLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::SetLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
            | FinalOpcode::SetLoc0
            | FinalOpcode::SetLoc1
            | FinalOpcode::SetLoc2
            | FinalOpcode::SetLoc3
            | FinalOpcode::PutLocCheck
            | FinalOpcode::SetLocCheck
            | FinalOpcode::SetLocUninitialized
    )
}

const fn is_local_read(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetLoc
            | FinalOpcode::GetLoc8
            | FinalOpcode::GetLoc0
            | FinalOpcode::GetLoc1
            | FinalOpcode::GetLoc2
            | FinalOpcode::GetLoc3
            | FinalOpcode::GetLocCheck
    )
}

const fn is_argument_write(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutArg
            | FinalOpcode::SetArg
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
            | FinalOpcode::SetArg0
            | FinalOpcode::SetArg1
            | FinalOpcode::SetArg2
            | FinalOpcode::SetArg3
    )
}

const fn is_closure_write(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutVarRef
            | FinalOpcode::SetVarRef
            | FinalOpcode::PutVarRef0
            | FinalOpcode::PutVarRef1
            | FinalOpcode::PutVarRef2
            | FinalOpcode::PutVarRef3
            | FinalOpcode::SetVarRef0
            | FinalOpcode::SetVarRef1
            | FinalOpcode::SetVarRef2
            | FinalOpcode::SetVarRef3
            | FinalOpcode::PutVarRefCheck
    )
}

fn policy_error(
    id: FunctionTemplateId,
    slot: BindingSlot,
    pc: Option<BytecodePc>,
    reason: BindingPolicyViolationReason,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::BindingPolicyViolation { slot, pc, reason },
    )
}

#[allow(clippy::too_many_lines)]
fn collect_requirements(
    function: &VerifiedCompilerFunction,
    metadata: &VerifiedFunctionMetadata,
    requirements: &mut Vec<ExecutionRequirement>,
) {
    if !function.atoms().is_empty()
        || function.constants().iter().any(|constant| {
            matches!(
                constant,
                crate::CompilerConstant::Value(crate::CompilerConstantValue::String(_))
            )
        })
    {
        push_requirement(requirements, ExecutionRequirement::Strings);
    }
    if function.constants().iter().any(|constant| {
        matches!(
            constant,
            crate::CompilerConstant::Value(crate::CompilerConstantValue::Number(_))
        )
    }) {
        push_requirement(requirements, ExecutionRequirement::Numbers);
    }
    if !metadata.closures.is_empty()
        || function
            .control_flow()
            .function_header()
            .variable_reference_count()
            != 0
        || function
            .constants()
            .iter()
            .any(|constant| matches!(constant, crate::CompilerConstant::Function(_)))
    {
        push_requirement(requirements, ExecutionRequirement::Closures);
    }
    if metadata
        .variables
        .iter()
        .any(|definition| definition.has_scope || definition.policy.temporal_dead_zone)
        || metadata
            .closures
            .iter()
            .any(|definition| definition.policy().temporal_dead_zone)
    {
        push_requirement(requirements, ExecutionRequirement::LexicalBindings);
    }
    if metadata
        .closures
        .iter()
        .any(|definition| definition.binding().is_realm_global())
    {
        push_requirement(requirements, ExecutionRequirement::RealmGlobalBindings);
    }
    for instruction in function.control_flow().instructions() {
        match instruction.decoded().instruction().opcode() {
            FinalOpcode::CallConstructor
            | FinalOpcode::Call
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::CallMethod
            | FinalOpcode::PushThis => {
                push_requirement(requirements, ExecutionRequirement::Calls);
            }
            FinalOpcode::Object
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::PutField
            | FinalOpcode::DefineField => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
            }
            FinalOpcode::Throw => {
                push_requirement(requirements, ExecutionRequirement::AbruptCompletions);
            }
            FinalOpcode::PushBigIntI32 => {
                push_requirement(requirements, ExecutionRequirement::BigInts);
            }
            FinalOpcode::InstanceOf | FinalOpcode::In => {
                push_requirement(requirements, ExecutionRequirement::ObjectOperators);
            }
            FinalOpcode::PushI32
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
            | FinalOpcode::PushI16 => {
                push_requirement(requirements, ExecutionRequirement::Numbers);
            }
            FinalOpcode::Neg
            | FinalOpcode::Plus
            | FinalOpcode::Dec
            | FinalOpcode::Inc
            | FinalOpcode::PostDec
            | FinalOpcode::PostInc
            | FinalOpcode::Not
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
            | FinalOpcode::Eq
            | FinalOpcode::Neq
            | FinalOpcode::StrictEq
            | FinalOpcode::StrictNeq
            | FinalOpcode::And
            | FinalOpcode::Xor
            | FinalOpcode::Or => {
                push_requirement(requirements, ExecutionRequirement::DynamicOperators);
            }
            FinalOpcode::PushEmptyString | FinalOpcode::Typeof => {
                push_requirement(requirements, ExecutionRequirement::Strings);
            }
            FinalOpcode::CloseLoc => {
                push_requirement(requirements, ExecutionRequirement::LexicalBindings);
                push_requirement(requirements, ExecutionRequirement::Closures);
            }
            _ => {}
        }
    }
}

fn push_requirement(
    requirements: &mut Vec<ExecutionRequirement>,
    requirement: ExecutionRequirement,
) {
    if !requirements.contains(&requirement) {
        requirements.push(requirement);
    }
}

fn function_id(index: usize) -> Result<FunctionTemplateId, BytecodeVerificationError> {
    let index = u32::try_from(index).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::VerifiedMetadata,
            limit: u64::from(u32::MAX),
            observed: usize_to_u64(index),
        })
    })?;
    Ok(FunctionTemplateId::new(index))
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
