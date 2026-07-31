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
    /// A nonconstructable ordinary object-literal method, getter, or setter.
    OrdinaryMethod,
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
    /// Runtime conversion and lookup of computed property keys.
    DynamicPropertyKeys,
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

    /// Returns a copy with another binding, method-target, and typed operand
    /// stack abstract-state entry maximum.
    #[must_use]
    pub const fn with_max_frame_state_entries(mut self, maximum: u64) -> Self {
        self.max_frame_state_entries = maximum;
        self
    }

    /// Returns a copy with another binding-policy, method-target, and typed
    /// operand-stack transfer maximum.
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

    /// Returns the conservative binding, method-target, and typed operand-stack
    /// abstract-state cell maximum.
    #[must_use]
    pub const fn max_frame_state_entries(self) -> u64 {
        self.max_frame_state_entries
    }

    /// Returns the aggregate binding-policy, method-target, and typed
    /// operand-stack state-cell visit maximum.
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

    /// Returns allocated binding, method-target, and typed operand-stack
    /// abstract-state entries.
    #[must_use]
    pub const fn frame_state_entries(self) -> u64 {
        self.frame_state_entries
    }

    /// Returns evaluated binding-policy, method-target, and typed operand-stack
    /// transfers.
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
    /// Binding, method-target, and typed operand-stack abstract-state entries.
    FrameStateEntries,
    /// Binding-policy, method-target, and typed operand-stack evaluations.
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
            Self::PolicyTransfers => "policy, method-target, and typed-stack transfers",
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
    /// An object method or accessor carries a source name before
    /// `define_method` assigns its property-derived observable name.
    OrdinaryMethodHasFunctionName,
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
    /// A metadata field references an atom certified only for static object
    /// property definition.
    StaticPropertyOnlyMetadataAtom {
        /// Invalid atom field.
        field: MetadataAtomField,
        /// Rejected index.
        index: u32,
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
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal `for-in` iterator marker.
    ForInIteratorStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct `for-in` iterator identities or mixed an
    /// iterator marker with an ordinary JavaScript value.
    ForInIteratorJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained an internal `for-in` iterator marker.
    ForInIteratorMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// `define_method` is not paired with one immediately preceding typed
    /// ordinary-method closure.
    DefineMethodTemplateMismatch {
        /// Final bytecode position of `define_method`.
        pc: BytecodePc,
    },
    /// `define_method` does not target one fresh object-literal value on every
    /// incoming control-flow path.
    DefineMethodTargetMismatch {
        /// Final bytecode position of `define_method`.
        pc: BytecodePc,
    },
    /// `define_array_el` did not receive a key converted by `to_propkey`
    /// before its value was evaluated on the same fresh object literal.
    DefineArrayElementKeyMismatch {
        /// Final bytecode position of `define_array_el`.
        pc: BytecodePc,
    },
    /// An ordinary-method template closure is not consumed by its one
    /// compiler-shaped `define_method` site.
    OrdinaryMethodTemplatePlacementMismatch {
        /// Final bytecode position of the method closure.
        pc: BytecodePc,
        /// Child template selected by the closure.
        child: FunctionTemplateId,
    },
    /// An ordinary-method template is not defined by exactly one parent site.
    OrdinaryMethodTemplateOwnershipMismatch {
        /// Method or accessor template.
        child: FunctionTemplateId,
        /// Validated `define_method` sites targeting the template.
        definitions: u32,
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
            Self::OrdinaryMethodHasFunctionName => formatter.write_str(
                "ordinary method carries a source name before define_method initialization",
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
            Self::StaticPropertyOnlyMetadataAtom { field, index } => write!(
                formatter,
                "metadata atom {field:?} references static-property-only atom slot {index}"
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
            Self::ForInIteratorStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed for-in iterator stack"
            ),
            Self::ForInIteratorJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed for-in iterator stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::ForInIteratorMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains an internal for-in iterator marker"
            ),
            Self::DefineMethodTemplateMismatch { pc } => write!(
                formatter,
                "define_method at PC {pc} is not paired with one typed method closure"
            ),
            Self::DefineMethodTargetMismatch { pc } => write!(
                formatter,
                "define_method at PC {pc} does not target one fresh object literal"
            ),
            Self::DefineArrayElementKeyMismatch { pc } => write!(
                formatter,
                "define_array_el at PC {pc} does not use a key converted before its value on one fresh object literal"
            ),
            Self::OrdinaryMethodTemplatePlacementMismatch { pc, child } => write!(
                formatter,
                "ordinary-method template {child} closure at PC {pc} is not consumed by define_method"
            ),
            Self::OrdinaryMethodTemplateOwnershipMismatch { child, definitions } => write!(
                formatter,
                "ordinary-method template {child} has {definitions} define_method sites"
            ),
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
    verify_method_definitions(&graph, &verified, limits, &mut usage)?;

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
    let mut for_in_certificate = verify_for_in_iterator_stack(id, function, limits, usage)?;
    classify_for_in_declarative_local_puts(
        id,
        flow,
        &metadata.variables,
        &mut for_in_certificate,
        limits,
        usage,
    )?;
    verify_binding_opcodes(id, flow, &metadata.variables, &metadata.closures)?;
    let binding_transfers = verify_binding_states(
        id,
        graph,
        function,
        &metadata.variables,
        &initializer_sites,
        &for_in_certificate,
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
        CompilerExecutableKind::OrdinaryMethod => {
            let has_function_name_binding =
                metadata.variables.iter().any(|definition| {
                    definition.policy.kind() == CompilerBindingKind::FunctionName
                }) || metadata.closures.iter().any(|definition| {
                    definition.policy().kind() == CompilerBindingKind::FunctionName
                });
            if metadata.function_name.is_some() || has_function_name_binding {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::OrdinaryMethodHasFunctionName,
                ));
            }
            Ok(())
        }
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
        CompilerExecutableKind::OrdinaryMethod => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0742
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
    if function
        .atoms()
        .get(atom.get() as usize)
        .is_some_and(crate::CompilerAtom::is_static_property_only)
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::StaticPropertyOnlyMetadataAtom {
                field,
                index: atom.get(),
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

#[allow(
    clippy::too_many_lines,
    reason = "typed closure pairing, unique CFG entry, arity, and ownership form one method-definition certificate"
)]
fn verify_method_definitions(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let mut definition_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let instructions = parent.control_flow().instructions();
        let mut predecessor_counts = try_filled_vec(
            parent_id,
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

        for (index, verified) in instructions.iter().enumerate() {
            let decoded = verified.decoded();
            let instruction = decoded.instruction();
            if is_method_definition_opcode(instruction.opcode())
                && method_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    index,
                )
                .is_none()
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::DefineMethodTemplateMismatch {
                        pc: decoded.pc(),
                    },
                ));
            }

            let Some(constant) = closure_constant(instruction.opcode(), instruction.operands())
            else {
                continue;
            };
            let Some(crate::CompilerConstant::Function(child)) =
                parent.constants().get(constant as usize)
            else {
                continue;
            };
            let Some(child_metadata) = usize::try_from(child.get())
                .ok()
                .and_then(|index| metadata.get(index))
            else {
                continue;
            };
            if child_metadata.executable_kind != CompilerExecutableKind::OrdinaryMethod {
                continue;
            }
            let pair = index.checked_add(1).and_then(|definition_index| {
                method_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    definition_index,
                )
            });
            if pair.map(|(defined, _)| defined) != Some(*child) {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch {
                        pc: decoded.pc(),
                        child: *child,
                    },
                ));
            }
            let child_index = usize::try_from(child.get()).map_err(|_| {
                BytecodeVerificationError::function(
                    *child,
                    BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch {
                        pc: decoded.pc(),
                        child: *child,
                    },
                )
            })?;
            let count = &mut definition_counts[child_index];
            *count = count.saturating_add(1);
        }
        if instructions.iter().any(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::DefineMethod
                    | FinalOpcode::DefineMethodComputed
                    | FinalOpcode::DefineArrayEl
            )
        }) {
            verify_object_definition_provenance(parent_id, parent, limits, usage)?;
        }
    }

    for (index, (metadata, &definitions)) in metadata.iter().zip(&definition_counts).enumerate() {
        if metadata.executable_kind != CompilerExecutableKind::OrdinaryMethod {
            continue;
        }
        let child = function_id(index)?;
        if definitions != 1 {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::OrdinaryMethodTemplateOwnershipMismatch {
                    child,
                    definitions,
                },
            ));
        }
    }
    Ok(())
}

fn method_definition_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    definition_index: usize,
) -> Option<(FunctionTemplateId, u8)> {
    let definition = instructions.get(definition_index)?;
    let definition_instruction = definition.decoded().instruction();
    let ((FinalOpcode::DefineMethod, Operands::AtomU8 { value: flags, .. })
    | (FinalOpcode::DefineMethodComputed, Operands::U8(flags))) = (
        definition_instruction.opcode(),
        definition_instruction.operands(),
    )
    else {
        return None;
    };
    if !(4..=6).contains(&flags) || predecessor_counts.get(definition_index) != Some(&1) {
        return None;
    }
    let closure_index = definition_index.checked_sub(1)?;
    let closure = instructions.get(closure_index)?;
    if closure
        .successors()
        .fallthrough()
        .map(InstructionIndex::get)
        != Some(usize_to_u32(definition_index))
    {
        return None;
    }
    let closure_instruction = closure.decoded().instruction();
    let constant = closure_constant(closure_instruction.opcode(), closure_instruction.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_index = usize::try_from(child.get()).ok()?;
    let child_metadata = metadata.get(child_index)?;
    if child_metadata.executable_kind != CompilerExecutableKind::OrdinaryMethod {
        return None;
    }
    let arguments = graph
        .function(*child)?
        .control_flow()
        .function_header()
        .defined_argument_count();
    if (flags == 5 && arguments != 0) || (flags == 6 && arguments != 1) {
        return None;
    }
    Some((*child, flags))
}

const fn is_method_definition_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::DefineMethod | FinalOpcode::DefineMethodComputed
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectDefinitionProvenance {
    Unknown,
    FreshObject(u32),
    ConvertedPropertyKey(u32),
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded CFG worklist and exact operand-stack transfer form one fresh-object certificate"
)]
fn verify_object_definition_provenance(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let instructions = function.control_flow().instructions();
    let mut entries = try_filled_vec(
        id,
        instructions.len(),
        None::<Vec<ObjectDefinitionProvenance>>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut queued = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::PolicyTransfers,
    )?;
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

    let mut next_seed = 0_usize;
    let mut evaluations = 0_u64;
    loop {
        if work.is_empty() {
            while entries.get(next_seed).is_some_and(Option::is_some) {
                next_seed = next_seed.saturating_add(1);
            }
            if next_seed == entries.len() {
                break;
            }
            entries[next_seed] = Some(Vec::new());
            queued[next_seed] = true;
            work.push_back(next_seed);
        }

        let Some(index) = work.pop_front() else {
            continue;
        };
        queued[index] = false;
        let entry = entries[index]
            .as_deref()
            .ok_or_else(|| object_definition_error(id, instructions[index].decoded().pc()))?;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(entry.len()).saturating_add(1),
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let mut state = try_copy_slice(id, entry, BytecodeGraphResource::FrameStateEntries)?;
        let decoded = instructions[index].decoded();
        match decoded.instruction().opcode() {
            FinalOpcode::DefineMethod
                if !matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(ObjectDefinitionProvenance::FreshObject(_))
                ) =>
            {
                return Err(method_target_error(id, decoded.pc()));
            }
            FinalOpcode::DefineMethodComputed
                if !matches!(
                    state.get(state.len().saturating_sub(3)),
                    Some(ObjectDefinitionProvenance::FreshObject(_))
                ) =>
            {
                return Err(method_target_error(id, decoded.pc()));
            }
            FinalOpcode::DefineArrayEl => {
                let object = state.get(state.len().saturating_sub(3));
                let key = state.get(state.len().saturating_sub(2));
                if !matches!(
                    (object, key),
                    (
                        Some(ObjectDefinitionProvenance::FreshObject(object_site)),
                        Some(ObjectDefinitionProvenance::ConvertedPropertyKey(key_site))
                    ) if object_site == key_site
                ) {
                    return Err(define_array_element_key_error(id, decoded.pc()));
                }
            }
            _ => {}
        }
        if !transfer_object_definition_provenance(id, index, decoded, &mut state)? {
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
                usize_to_u64(state.len()).saturating_add(1),
                usage.policy_transfers,
                limits.max_policy_transfers,
            )?;
            propagate_object_definition_provenance(
                id,
                decoded.pc(),
                successor,
                &state,
                &mut entries,
                &mut queued,
                &mut work,
                limits.max_frame_state_entries,
                usage,
            )?;
        }
    }
    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )
}

fn transfer_object_definition_provenance(
    id: FunctionTemplateId,
    instruction_index: usize,
    decoded: crate::DecodedInstruction,
    state: &mut Vec<ObjectDefinitionProvenance>,
) -> Result<bool, BytecodeVerificationError> {
    let instruction = decoded.instruction();
    let effect = instruction
        .stack_effect()
        .map_err(|_| object_definition_error(id, decoded.pc()))?;
    let pops = effect.pops() as usize;
    let pushes = effect.pushes() as usize;
    if state.len() < pops {
        return Ok(false);
    }
    let output_len = state
        .len()
        .checked_sub(pops)
        .and_then(|length| length.checked_add(pushes))
        .ok_or_else(|| object_definition_error(id, decoded.pc()))?;
    if output_len > state.len() {
        let additional = output_len - state.len();
        state.try_reserve(additional).map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    requested: usize_to_u64(additional),
                },
            )
        })?;
    }

    match instruction.opcode() {
        FinalOpcode::Object => state.push(ObjectDefinitionProvenance::FreshObject(usize_to_u32(
            instruction_index,
        ))),
        FinalOpcode::Dup => {
            let value = *state
                .last()
                .ok_or_else(|| object_definition_error(id, decoded.pc()))?;
            state.push(shuffled_object_definition_provenance(value));
        }
        FinalOpcode::Insert2 => {
            let left_index = state.len() - 2;
            let left = shuffled_object_definition_provenance(state[left_index]);
            let right = shuffled_object_definition_provenance(state[left_index + 1]);
            state[left_index] = right;
            state[left_index + 1] = left;
            state.push(right);
        }
        FinalOpcode::Insert3 => {
            let first_index = state.len() - 3;
            let first = shuffled_object_definition_provenance(state[first_index]);
            let second = shuffled_object_definition_provenance(state[first_index + 1]);
            let third = shuffled_object_definition_provenance(state[first_index + 2]);
            state[first_index] = third;
            state[first_index + 1] = first;
            state[first_index + 2] = second;
            state.push(third);
        }
        FinalOpcode::GetField2 => {
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        FinalOpcode::GetArrayEl2 => {
            let base = state[state.len() - 2];
            state.truncate(state.len() - 2);
            state.push(base);
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        FinalOpcode::ToPropKey => convert_property_key_provenance(state),
        FinalOpcode::DefineField | FinalOpcode::DefineMethod => {
            let base = state[state.len() - 2];
            state.truncate(state.len() - 2);
            state.push(base);
        }
        FinalOpcode::DefineArrayEl => {
            let base = state[state.len() - 3];
            let key = state[state.len() - 2];
            state.truncate(state.len() - 3);
            state.push(base);
            state.push(key);
        }
        FinalOpcode::DefineMethodComputed => {
            let base = state[state.len() - 3];
            state.truncate(state.len() - 3);
            state.push(base);
        }
        _ => {
            state.truncate(state.len() - pops);
            state.resize(output_len, ObjectDefinitionProvenance::Unknown);
        }
    }
    if state.len() != output_len {
        return Err(object_definition_error(id, decoded.pc()));
    }
    Ok(true)
}

// A converted key is also a temporal anchor: the fresh object must remain
// immediately below that exact stack slot while the value is evaluated.
// Copying or moving the marker would let a value evaluated earlier be rotated
// across the pair and masquerade as the compiler's post-conversion RHS.
const fn shuffled_object_definition_provenance(
    value: ObjectDefinitionProvenance,
) -> ObjectDefinitionProvenance {
    match value {
        ObjectDefinitionProvenance::ConvertedPropertyKey(_) => ObjectDefinitionProvenance::Unknown,
        value => value,
    }
}

fn convert_property_key_provenance(state: &mut [ObjectDefinitionProvenance]) {
    let key_index = state.len() - 1;
    let converted = key_index
        .checked_sub(1)
        .and_then(|object_index| state.get(object_index))
        .and_then(|provenance| match provenance {
            ObjectDefinitionProvenance::FreshObject(site) => Some(*site),
            _ => None,
        })
        .map_or(
            ObjectDefinitionProvenance::Unknown,
            ObjectDefinitionProvenance::ConvertedPropertyKey,
        );
    state[key_index] = converted;
}

#[allow(clippy::too_many_arguments)]
fn propagate_object_definition_provenance(
    id: FunctionTemplateId,
    source_pc: BytecodePc,
    successor: InstructionIndex,
    output: &[ObjectDefinitionProvenance],
    entries: &mut [Option<Vec<ObjectDefinitionProvenance>>],
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
    state_limit: u64,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let index = successor.get() as usize;
    let entry = entries
        .get_mut(index)
        .ok_or_else(|| method_target_error(id, source_pc))?;
    let changed = match entry {
        None => {
            charge_frame_state_entries(id, usage, output.len(), state_limit)?;
            *entry = Some(try_copy_slice(
                id,
                output,
                BytecodeGraphResource::FrameStateEntries,
            )?);
            true
        }
        Some(existing) if existing.len() == output.len() => {
            let mut changed = false;
            for (target, incoming) in existing.iter_mut().zip(output) {
                let merged = match (*target, *incoming) {
                    (
                        ObjectDefinitionProvenance::FreshObject(left),
                        ObjectDefinitionProvenance::FreshObject(right),
                    ) if left == right => *target,
                    (
                        ObjectDefinitionProvenance::ConvertedPropertyKey(left),
                        ObjectDefinitionProvenance::ConvertedPropertyKey(right),
                    ) if left == right => *target,
                    _ => ObjectDefinitionProvenance::Unknown,
                };
                changed |= merged != *target;
                *target = merged;
            }
            changed
        }
        Some(existing) => {
            let changed = existing
                .iter()
                .any(|value| *value != ObjectDefinitionProvenance::Unknown);
            existing.fill(ObjectDefinitionProvenance::Unknown);
            changed
        }
    };
    if changed && !queued[index] {
        queued[index] = true;
        work.push_back(index);
    }
    Ok(())
}

fn charge_frame_state_entries(
    id: FunctionTemplateId,
    usage: &mut BytecodeGraphUsage,
    amount: usize,
    limit: u64,
) -> Result<(), BytecodeVerificationError> {
    let amount = usize_to_u64(amount);
    let observed = usage
        .frame_state_entries
        .checked_add(amount)
        .ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    limit,
                    observed: u64::MAX,
                },
            )
        })?;
    if observed > limit {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::FrameStateEntries,
                limit,
                observed,
            },
        ));
    }
    usage.frame_state_entries = observed;
    Ok(())
}

fn method_target_error(id: FunctionTemplateId, pc: BytecodePc) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::DefineMethodTargetMismatch { pc },
    )
}

fn define_array_element_key_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::DefineArrayElementKeyMismatch { pc },
    )
}

fn object_definition_error(id: FunctionTemplateId, pc: BytecodePc) -> BytecodeVerificationError {
    method_target_error(id, pc)
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
    executable_kind: CompilerExecutableKind,
    authority_kind: CompilerExecutableKind,
) -> Result<(), BytecodeVerificationError> {
    for instruction in flow.instructions() {
        let decoded = instruction.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if !supported_compiler_opcode(opcode)
            || (opcode == FinalOpcode::PushThis
                && !flow.function_header().mode().is_strict()
                && executable_kind != CompilerExecutableKind::OrdinaryMethod
                && authority_kind != CompilerExecutableKind::DynamicFunctionScript)
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::DefineMethod, Operands::AtomU8 { value, .. })
                    | (FinalOpcode::DefineMethodComputed, Operands::U8(value))
                    if !(4..=6).contains(&value)
            )
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
            | FinalOpcode::Nip
            | FinalOpcode::Dup
            | FinalOpcode::Insert2
            | FinalOpcode::Insert3
            | FinalOpcode::Swap
            | FinalOpcode::Rot3l
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
            | FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::PutField
            | FinalOpcode::PutArrayEl
            | FinalOpcode::ToPropKey
            | FinalOpcode::DefineField
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::DefineMethod
            | FinalOpcode::DefineMethodComputed
            | FinalOpcode::ForInStart
            | FinalOpcode::ForInNext
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForInStackValue {
    Ordinary,
    Iterator(BytecodePc),
    Key(BytecodePc),
    Done(BytecodePc),
    HeadKey(BytecodePc),
}

impl ForInStackValue {
    const fn is_javascript_value(self) -> bool {
        !matches!(self, Self::Iterator(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedForInLocalPut {
    local: u32,
    cursor_site: BytecodePc,
}

#[derive(Default)]
struct ForInIteratorCertificate {
    local_key_puts: Vec<Option<CertifiedForInLocalPut>>,
}

impl ForInIteratorCertificate {
    fn certifies_local_key_put(&self, instruction: usize, local: u32) -> bool {
        self.local_key_puts
            .get(instruction)
            .copied()
            .flatten()
            .is_some_and(|certificate| certificate.local == local)
    }
}

#[derive(Clone, Copy, Default)]
struct ForInLocalPutSummary {
    unchecked_puts: u32,
    certified_puts: u32,
    cursor_site: Option<BytecodePc>,
    first_certified_pc: Option<BytecodePc>,
    has_uncertified_put: bool,
    multiple_cursor_sites: bool,
    declarative_authority: bool,
}

#[derive(Clone, Copy)]
struct ForInStackTransfer {
    normal_completion: bool,
    iteration_branch_key: Option<BytecodePc>,
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded CFG worklist and exact operand-stack transfer form one for-in iterator certificate"
)]
fn verify_for_in_iterator_stack(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<ForInIteratorCertificate, BytecodeVerificationError> {
    let instructions = function.control_flow().instructions();
    if !instructions.iter().any(|verified| {
        matches!(
            verified.decoded().instruction().opcode(),
            FinalOpcode::ForInStart | FinalOpcode::ForInNext | FinalOpcode::Nip
        )
    }) {
        return Ok(ForInIteratorCertificate::default());
    }

    let mut entries = try_filled_vec(
        id,
        instructions.len(),
        None::<Vec<ForInStackValue>>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut queued = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    let mut components = try_filled_vec(
        id,
        instructions.len(),
        None::<u32>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut local_key_puts = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedForInLocalPut>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
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

    let mut next_seed = 0_usize;
    let mut evaluations = 0_u64;
    loop {
        if work.is_empty() {
            while entries.get(next_seed).is_some_and(Option::is_some) {
                next_seed = next_seed.saturating_add(1);
            }
            if next_seed == entries.len() {
                break;
            }
            entries[next_seed] = Some(Vec::new());
            components[next_seed] = Some(usize_to_u32(next_seed));
            queued[next_seed] = true;
            work.push_back(next_seed);
        }

        let Some(index) = work.pop_front() else {
            continue;
        };
        queued[index] = false;
        let decoded = instructions[index].decoded();
        let component = components[index]
            .ok_or_else(|| for_in_stack_error(id, decoded.pc(), decoded.instruction().opcode()))?;
        let entry = entries[index]
            .as_deref()
            .ok_or_else(|| for_in_stack_error(id, decoded.pc(), decoded.instruction().opcode()))?;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(entry.len()).saturating_add(1),
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let mut state = try_copy_slice(id, entry, BytecodeGraphResource::FrameStateEntries)?;
        let transfer =
            transfer_for_in_iterator_stack(id, index, decoded, &mut state, &mut local_key_puts)?;
        if !transfer.normal_completion {
            continue;
        }

        let successors = instructions[index].successors();
        let mut has_successor = false;
        for (successor, is_branch_target) in [
            (successors.fallthrough(), false),
            (successors.branch_target(), true),
            (successors.jump_target(), false),
        ]
        .into_iter()
        .filter_map(|(successor, is_branch_target)| {
            successor.map(|successor| (successor, is_branch_target))
        }) {
            has_successor = true;
            let target_pc = instructions
                .get(successor.get() as usize)
                .map(|instruction| instruction.decoded().pc())
                .ok_or_else(|| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ForInIteratorJoinMismatch {
                            target: BytecodePc::new(successor.get()),
                            incoming_from: decoded.pc(),
                        },
                    )
                })?;
            let branch_key_index = transfer
                .iteration_branch_key
                .map(|site| {
                    let key_index = state.len().checked_sub(1).ok_or_else(|| {
                        for_in_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                    })?;
                    if state[key_index] != ForInStackValue::Key(site) {
                        return Err(for_in_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                    state[key_index] = if is_branch_target {
                        ForInStackValue::HeadKey(site)
                    } else {
                        ForInStackValue::Ordinary
                    };
                    Ok(key_index)
                })
                .transpose()?;
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(state.len()).saturating_add(1),
                usage.policy_transfers,
                limits.max_policy_transfers,
            )?;
            propagate_for_in_iterator_stack(
                id,
                decoded.pc(),
                successor,
                target_pc,
                component,
                &state,
                &mut entries,
                &mut components,
                &mut queued,
                &mut work,
                limits.max_frame_state_entries,
                usage,
            )?;
            if let (Some(key_index), Some(site)) = (branch_key_index, transfer.iteration_branch_key)
            {
                state[key_index] = ForInStackValue::Key(site);
            }
        }
        if !has_successor
            && state
                .iter()
                .any(|value| matches!(value, ForInStackValue::Iterator(_)))
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::ForInIteratorMarkerAtExit { pc: decoded.pc() },
            ));
        }
    }

    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    Ok(ForInIteratorCertificate { local_key_puts })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the classifier shares the graph resource limits and usage ledger with the typed stack pass"
)]
fn classify_for_in_declarative_local_puts(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
    certificate: &mut ForInIteratorCertificate,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    if certificate.local_key_puts.iter().all(Option::is_none) {
        return Ok(());
    }

    let argument_count = flow.domains().argument_count() as usize;
    let local_count = variables.len() - argument_count;
    charge_frame_state_entries(id, usage, local_count, limits.max_frame_state_entries)?;
    let mut summaries = try_filled_vec(
        id,
        local_count,
        ForInLocalPutSummary::default(),
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut evaluations = 0_u64;

    for (index, verified) in flow.instructions().iter().enumerate() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if !is_unchecked_local_put(opcode) {
            continue;
        }
        charge_policy_transfers(
            id,
            &mut evaluations,
            1,
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let Some(local) = local_operand(opcode, instruction.operands()) else {
            return Err(for_in_stack_error(id, decoded.pc(), opcode));
        };
        let Some(summary) = summaries.get_mut(local as usize) else {
            return Err(for_in_stack_error(id, decoded.pc(), opcode));
        };
        summary.unchecked_puts = summary.unchecked_puts.saturating_add(1);

        let certified = certificate.local_key_puts.get(index).copied().flatten();
        let Some(certified) = certified else {
            summary.has_uncertified_put = true;
            continue;
        };
        if certified.local != local {
            return Err(for_in_stack_error(id, decoded.pc(), opcode));
        }
        summary.certified_puts = summary.certified_puts.saturating_add(1);
        summary.first_certified_pc.get_or_insert(decoded.pc());
        match summary.cursor_site {
            Some(site) if site != certified.cursor_site => {
                summary.multiple_cursor_sites = true;
            }
            Some(_) => {}
            None => summary.cursor_site = Some(certified.cursor_site),
        }
    }

    for (local, summary) in summaries.iter_mut().enumerate() {
        if summary.certified_puts == 0 {
            continue;
        }
        charge_policy_transfers(
            id,
            &mut evaluations,
            1,
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let definition = &variables[argument_count + local];
        if definition.policy.temporal_dead_zone && summary.multiple_cursor_sites {
            return Err(policy_error(
                id,
                BindingSlot::Local(usize_to_u32(local)),
                summary.first_certified_pc,
                BindingPolicyViolationReason::InvalidLexicalInitialization,
            ));
        }
        summary.declarative_authority = definition.policy.temporal_dead_zone
            && !summary.has_uncertified_put
            && summary.unchecked_puts == summary.certified_puts;
    }

    for certified in &mut certificate.local_key_puts {
        let Some(local_put) = *certified else {
            continue;
        };
        charge_policy_transfers(
            id,
            &mut evaluations,
            1,
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        if !summaries[local_put.local as usize].declarative_authority {
            *certified = None;
        }
    }

    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "marker isolation, edge-specific key provenance, and ordinary stack transfer form one typed opcode boundary"
)]
fn transfer_for_in_iterator_stack(
    id: FunctionTemplateId,
    instruction_index: usize,
    decoded: crate::DecodedInstruction,
    state: &mut Vec<ForInStackValue>,
    local_key_puts: &mut [Option<CertifiedForInLocalPut>],
) -> Result<ForInStackTransfer, BytecodeVerificationError> {
    let instruction = decoded.instruction();
    let opcode = instruction.opcode();
    match opcode {
        FinalOpcode::ForInStart => {
            invalidate_for_in_key_provenance(state);
            let Some(input) = state.last_mut() else {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            };
            if *input != ForInStackValue::Ordinary {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            }
            *input = ForInStackValue::Iterator(decoded.pc());
            return Ok(ForInStackTransfer {
                normal_completion: true,
                iteration_branch_key: None,
            });
        }
        FinalOpcode::ForInNext => {
            invalidate_for_in_key_provenance(state);
            let Some(ForInStackValue::Iterator(site)) = state.last().copied() else {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            };
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(ForInStackValue::Key(site));
            state.push(ForInStackValue::Done(site));
            return Ok(ForInStackTransfer {
                normal_completion: true,
                iteration_branch_key: None,
            });
        }
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => {
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    ForInStackValue::Iterator(iterator),
                    ForInStackValue::Key(key),
                    ForInStackValue::Done(done),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == key
                && key == done
            {
                state.pop();
                invalidate_for_in_key_provenance(&mut state[..base]);
                return Ok(ForInStackTransfer {
                    normal_completion: true,
                    iteration_branch_key: Some(key),
                });
            }
        }
        opcode if is_unchecked_local_put(opcode) => {
            if let Some(marker) = state.len().checked_sub(2)
                && let (ForInStackValue::Iterator(iterator), ForInStackValue::HeadKey(key)) =
                    (state[marker], state[marker + 1])
                && iterator == key
            {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(for_in_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = local_key_puts.get_mut(instruction_index) else {
                    return Err(for_in_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedForInLocalPut {
                    local,
                    cursor_site: key,
                });
                state.pop();
                invalidate_for_in_key_provenance(state);
                return Ok(ForInStackTransfer {
                    normal_completion: true,
                    iteration_branch_key: None,
                });
            }
        }
        FinalOpcode::Drop => {
            if state.pop().is_none() {
                return Ok(ForInStackTransfer {
                    normal_completion: false,
                    iteration_branch_key: None,
                });
            }
            invalidate_for_in_key_provenance(state);
            return Ok(ForInStackTransfer {
                normal_completion: true,
                iteration_branch_key: None,
            });
        }
        FinalOpcode::Nip => {
            let marker_index = state.len().checked_sub(2);
            if !matches!(
                marker_index.map(|index| (state[index], state[index + 1])),
                Some((ForInStackValue::Iterator(_), value))
                    if value.is_javascript_value()
            ) {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            }
            state.truncate(state.len() - 2);
            state.push(ForInStackValue::Ordinary);
            invalidate_for_in_key_provenance(state);
            return Ok(ForInStackTransfer {
                normal_completion: true,
                iteration_branch_key: None,
            });
        }
        _ => {}
    }

    invalidate_for_in_key_provenance(state);
    let effect = instruction
        .stack_effect()
        .map_err(|_| for_in_stack_error(id, decoded.pc(), opcode))?;
    let pops = effect.pops() as usize;
    let pushes = effect.pushes() as usize;
    let Some(input_start) = state.len().checked_sub(pops) else {
        return Ok(ForInStackTransfer {
            normal_completion: false,
            iteration_branch_key: None,
        });
    };
    if state[input_start..]
        .iter()
        .any(|value| !value.is_javascript_value())
    {
        return Err(for_in_stack_error(id, decoded.pc(), opcode));
    }
    let output_len = input_start
        .checked_add(pushes)
        .ok_or_else(|| for_in_stack_error(id, decoded.pc(), opcode))?;
    if output_len > state.len() {
        let additional = output_len - state.len();
        state.try_reserve(additional).map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    requested: usize_to_u64(additional),
                },
            )
        })?;
    }
    state.truncate(input_start);
    state.resize(output_len, ForInStackValue::Ordinary);
    Ok(ForInStackTransfer {
        normal_completion: true,
        iteration_branch_key: None,
    })
}

fn invalidate_for_in_key_provenance(state: &mut [ForInStackValue]) {
    for value in state {
        if matches!(
            value,
            ForInStackValue::Key(_) | ForInStackValue::Done(_) | ForInStackValue::HeadKey(_)
        ) {
            *value = ForInStackValue::Ordinary;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn propagate_for_in_iterator_stack(
    id: FunctionTemplateId,
    source_pc: BytecodePc,
    successor: InstructionIndex,
    target_pc: BytecodePc,
    component: u32,
    output: &[ForInStackValue],
    entries: &mut [Option<Vec<ForInStackValue>>],
    components: &mut [Option<u32>],
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
    state_limit: u64,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let index = successor.get() as usize;
    let component_slot = components
        .get_mut(index)
        .ok_or_else(|| for_in_join_error(id, target_pc, source_pc))?;
    match *component_slot {
        Some(established) if established != component => return Ok(()),
        Some(_) => {}
        None => *component_slot = Some(component),
    }
    let entry = entries
        .get_mut(index)
        .ok_or_else(|| for_in_join_error(id, target_pc, source_pc))?;
    match entry {
        None => {
            charge_frame_state_entries(id, usage, output.len(), state_limit)?;
            *entry = Some(try_copy_slice(
                id,
                output,
                BytecodeGraphResource::FrameStateEntries,
            )?);
            if !queued[index] {
                queued[index] = true;
                work.push_back(index);
            }
        }
        Some(existing) if existing == output => {}
        Some(_) => {
            return Err(for_in_join_error(id, target_pc, source_pc));
        }
    }
    Ok(())
}

fn for_in_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ForInIteratorStackMismatch { pc, opcode },
    )
}

fn for_in_join_error(
    id: FunctionTemplateId,
    target: BytecodePc,
    incoming_from: BytecodePc,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ForInIteratorJoinMismatch {
            target,
            incoming_from,
        },
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
    for_in_certificate: &ForInIteratorCertificate,
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
                if state[position] & BindingState::INACTIVE_ACTIVE != 0 {
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
                if state[position] & BindingState::INACTIVE != 0
                    && !certified_realm_global_initializer
                {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                    ));
                }
                state[position] = BindingState::with_active_cell(state[position]);
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
                for_in_certificate.certifies_local_key_put(index, local),
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
    const INACTIVE_CLOSED: u8 = 1 << 0;
    const INACTIVE_ACTIVE: u8 = 1 << 1;
    const UNINITIALIZED_CLOSED: u8 = 1 << 2;
    const UNINITIALIZED_ACTIVE: u8 = 1 << 3;
    const INITIALIZED_CLOSED: u8 = 1 << 4;
    const INITIALIZED_ACTIVE: u8 = 1 << 5;

    const INACTIVE: u8 = Self::INACTIVE_CLOSED | Self::INACTIVE_ACTIVE;
    const UNINITIALIZED: u8 = Self::UNINITIALIZED_CLOSED | Self::UNINITIALIZED_ACTIVE;
    const INITIALIZED: u8 = Self::INITIALIZED_CLOSED | Self::INITIALIZED_ACTIVE;
    const CLOSED: u8 =
        Self::INACTIVE_CLOSED | Self::UNINITIALIZED_CLOSED | Self::INITIALIZED_CLOSED;
    const ACTIVE: u8 =
        Self::INACTIVE_ACTIVE | Self::UNINITIALIZED_ACTIVE | Self::INITIALIZED_ACTIVE;
    const ENTRY: u8 = Self::INACTIVE_CLOSED;

    const fn only(state: u8, allowed: u8) -> bool {
        state != 0 && state & !allowed == 0
    }

    const fn with_uninitialized_value(state: u8) -> u8 {
        let mut output = 0;
        if state & Self::CLOSED != 0 {
            output |= Self::UNINITIALIZED_CLOSED;
        }
        if state & Self::ACTIVE != 0 {
            output |= Self::UNINITIALIZED_ACTIVE;
        }
        output
    }

    const fn with_initialized_value(state: u8) -> u8 {
        let mut output = 0;
        if state & Self::CLOSED != 0 {
            output |= Self::INITIALIZED_CLOSED;
        }
        if state & Self::ACTIVE != 0 {
            output |= Self::INITIALIZED_ACTIVE;
        }
        output
    }

    const fn with_closed_cell(state: u8) -> u8 {
        (state & Self::CLOSED) | ((state & Self::ACTIVE) >> 1)
    }

    const fn with_active_cell(state: u8) -> u8 {
        ((state & Self::CLOSED) << 1) | (state & Self::ACTIVE)
    }
}

fn requires_binding_state(definition: &VariableDefinition) -> bool {
    definition.policy.temporal_dead_zone
        || (definition.has_scope && definition.variable_reference.is_some())
        || definition.function_initializer.is_some()
        || definition.policy.initialization == CompilerInitializationPolicy::FunctionName
}

fn initial_binding_state(definition: &VariableDefinition) -> u8 {
    if definition.policy.initialization == CompilerInitializationPolicy::FunctionName {
        if definition.variable_reference.is_some() {
            BindingState::INITIALIZED_ACTIVE
        } else {
            BindingState::INITIALIZED_CLOSED
        }
    } else {
        BindingState::ENTRY
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "binding identity, declaration policy, initializer authority, and for-in key authority are checked together"
)]
fn transfer_local_state(
    id: FunctionTemplateId,
    pc: BytecodePc,
    local: u32,
    opcode: FinalOpcode,
    definition: &VariableDefinition,
    is_function_initializer: bool,
    is_for_in_key_put: bool,
    state: &mut u8,
) -> Result<bool, BytecodeVerificationError> {
    let slot = BindingSlot::Local(local);
    match opcode {
        FinalOpcode::SetLocUninitialized => {
            if definition.has_scope
                && definition.variable_reference.is_some()
                && *state & (BindingState::UNINITIALIZED_ACTIVE | BindingState::INITIALIZED_ACTIVE)
                    != 0
            {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            *state = if definition.has_scope && definition.variable_reference.is_some() {
                BindingState::UNINITIALIZED_ACTIVE
            } else {
                BindingState::with_uninitialized_value(*state)
            };
        }
        opcode if is_unchecked_local_put(opcode) => {
            let valid = if is_function_initializer {
                match definition.policy.initialization {
                    CompilerInitializationPolicy::FunctionAtScopeEntry => {
                        BindingState::only(*state, BindingState::UNINITIALIZED)
                            && (definition.variable_reference.is_none()
                                || BindingState::only(*state, BindingState::UNINITIALIZED_ACTIVE))
                    }
                    CompilerInitializationPolicy::FunctionAtInstantiation
                    | CompilerInitializationPolicy::Argument => {
                        BindingState::only(*state, BindingState::INACTIVE)
                    }
                    _ => false,
                }
            } else if definition.function_initializer.is_some()
                && !BindingState::only(*state, BindingState::INITIALIZED)
            {
                false
            } else if is_for_in_key_put {
                BindingState::only(
                    *state,
                    BindingState::UNINITIALIZED | BindingState::INITIALIZED_CLOSED,
                )
            } else if definition.policy.writes == CompilerWritePolicy::Mutable {
                *state & BindingState::INACTIVE == 0
            } else {
                BindingState::only(*state, BindingState::UNINITIALIZED)
            };
            if !valid {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            *state = BindingState::with_initialized_value(*state);
        }
        FinalOpcode::GetLocCheck | FinalOpcode::PutLocCheck | FinalOpcode::SetLocCheck => {
            if *state & BindingState::INACTIVE != 0 {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                ));
            }
            let normal = *state & BindingState::INITIALIZED;
            if normal == 0 {
                return Ok(false);
            }
            *state = normal;
        }
        FinalOpcode::CloseLoc => {
            *state = BindingState::with_closed_cell(*state);
        }
        opcode
            if (is_local_read(opcode) || is_local_write(opcode))
                && !BindingState::only(*state, BindingState::INITIALIZED) =>
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
            | FinalOpcode::DefineField
            | FinalOpcode::DefineMethod
            | FinalOpcode::ForInStart => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
            }
            FinalOpcode::ForInNext => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::Strings);
            }
            FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::PutArrayEl
            | FinalOpcode::ToPropKey
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::DefineMethodComputed => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::DynamicPropertyKeys);
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
