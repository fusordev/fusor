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
    verifier::{CompilerCaptureLayout, CompilerCapturedBinding, InstructionIndex},
};

mod lexical_environment;
mod object_provenance;

use lexical_environment::verify_lexical_arrow_environments;
use object_provenance::{charge_frame_state_entries, verify_object_definition_provenance};

const DEFAULT_MAX_VARIABLE_DEFINITIONS: u64 = 1_048_576;
const DEFAULT_MAX_CLOSURE_DEFINITIONS: u64 = 1_048_576;
const DEFAULT_MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_SOURCE_MAPPINGS: u64 = 8_388_608;
const DEFAULT_MAX_FRAME_STATE_ENTRIES: u64 = 33_554_432;
const DEFAULT_MAX_POLICY_TRANSFERS: u64 = 33_554_432;

/// Maximum `gosub` sites accepted in one function by the pinned compatibility
/// profile.
pub const MAX_GOSUB_SITES_PER_FUNCTION: u32 = 65_534;

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
    /// The immutable inner binding created for a named class definition.
    ClassName,
    /// The compiler-created immutable class-scope cell for one evaluated
    /// computed public field key.
    ClassFieldKey,
    /// The compiler-created immutable class-scope cell holding the hidden
    /// instance-element initializer method.
    ClassInstanceInitializer,
    /// The compiler-created immutable class-scope cell for one fresh private
    /// instance-field name.
    ClassPrivateName,
    /// The compiler-created immutable class-scope receiver cell used by
    /// static field initializers that lexically observe `this` or resolve a
    /// `super` property.
    ClassStaticReceiver,
    /// The compiler-created immutable cell holding an active Object
    /// Environment Record's binding object for a sloppy `with` statement.
    ///
    /// This is not an ECMAScript declarative binding. Its distinct metadata
    /// kind lets direct eval reconstruct the intervening dynamic environment
    /// without exposing the compiler's hidden cell name to source code.
    WithObject,
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
            CompilerBindingKind::Let
                | CompilerBindingKind::Const
                | CompilerBindingKind::ClassName
                | CompilerBindingKind::ClassFieldKey
                | CompilerBindingKind::ClassInstanceInitializer
                | CompilerBindingKind::ClassPrivateName
                | CompilerBindingKind::ClassStaticReceiver
                | CompilerBindingKind::WithObject
                | CompilerBindingKind::Catch
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
            CompilerBindingKind::Const
            | CompilerBindingKind::ClassName
            | CompilerBindingKind::ClassFieldKey
            | CompilerBindingKind::ClassInstanceInitializer
            | CompilerBindingKind::ClassPrivateName
            | CompilerBindingKind::ClassStaticReceiver
            | CompilerBindingKind::WithObject => {
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
            CompilerBindingKind::Catch => {
                matches!(self.initialization, CompilerInitializationPolicy::Catch)
                    && matches!(self.writes, CompilerWritePolicy::Mutable)
            }
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
    arguments_object: bool,
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
            arguments_object: false,
            variable_reference,
            function_initializer: None,
        }
    }

    /// Marks the compiler-synthesized function `arguments` object binding.
    #[must_use]
    pub const fn with_arguments_object(mut self, arguments_object: bool) -> Self {
        self.arguments_object = arguments_object;
        self
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

    /// Returns whether this is the compiler-synthesized `arguments` object.
    #[must_use]
    pub const fn is_arguments_object(&self) -> bool {
        self.arguments_object
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
    arguments_object: bool,
    deletable_eval_variable: bool,
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
            arguments_object: false,
            deletable_eval_variable: false,
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
            arguments_object: false,
            deletable_eval_variable: false,
            function_initializer: None,
        }
    }

    /// Marks a captured compiler-synthesized `arguments` object binding.
    #[must_use]
    pub const fn with_arguments_object(mut self, arguments_object: bool) -> Self {
        self.arguments_object = arguments_object;
        self
    }

    /// Marks a captured binding created by sloppy direct eval. Such bindings
    /// remain dynamically name-resolved because `DeleteBinding` may remove
    /// them from the caller's variable environment.
    #[must_use]
    pub const fn with_deletable_eval_variable(mut self, deletable: bool) -> Self {
        self.deletable_eval_variable = deletable;
        self
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

    /// Returns whether this capture originates at a synthesized `arguments`
    /// object binding.
    #[must_use]
    pub const fn is_arguments_object(&self) -> bool {
        self.arguments_object
    }

    /// Returns whether sloppy direct eval created this deletable binding.
    #[must_use]
    pub const fn is_deletable_eval_variable(&self) -> bool {
        self.deletable_eval_variable
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
    strict_mode_pcs: Option<Arc<[BytecodePc]>>,
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
            strict_mode_pcs: None,
        }
    }

    /// Attaches the sorted instruction PCs whose source regions are strict
    /// even though their surrounding executable is not.
    #[must_use]
    pub fn with_strict_mode_pcs(mut self, strict_mode_pcs: Arc<[BytecodePc]>) -> Self {
        self.strict_mode_pcs = (!strict_mode_pcs.is_empty()).then_some(strict_mode_pcs);
        self
    }
}

/// Compiler-owned execution role assigned to one function-graph record.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CompilerExecutableKind {
    /// A host-loaded ECMAScript Global Script.
    GlobalScript,
    /// A Script compiled for an indirect invocation of `%eval%`.
    IndirectEvalScript,
    /// A Script compiled for a direct invocation of the caller's `%eval%`.
    DirectEvalScript,
    /// An ordinary callable JavaScript function.
    #[default]
    OrdinaryFunction,
    /// A synchronous lexical-this arrow function.
    OrdinaryArrow,
    /// An asynchronous lexical-this arrow function.
    AsyncArrow,
    /// A nonconstructable ordinary object-literal method, getter, or setter.
    OrdinaryMethod,
    /// The hidden strict method that initializes one class's instance
    /// elements. It is retained only in a compiler-owned class-scope cell.
    ClassInstanceInitializer,
    /// A strict constructable base-class constructor. Its public prototype
    /// object is installed by the paired `define_class` instruction.
    ClassConstructor,
    /// A nonconstructable synchronous generator function.
    GeneratorFunction,
    /// A synchronous generator object-literal method.
    GeneratorMethod,
    /// A nonconstructable asynchronous function.
    AsyncFunction,
    /// An asynchronous object-literal method.
    AsyncMethod,
    /// A nonconstructable asynchronous generator function.
    AsyncGeneratorFunction,
    /// An asynchronous generator object-literal method.
    AsyncGeneratorMethod,
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
    strict_mode_pcs: Option<Arc<[BytecodePc]>>,
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

    /// Returns whether this instruction belongs to a nested strict source
    /// region inside an otherwise non-strict executable.
    #[must_use]
    pub fn is_strict_mode_pc(&self, pc: BytecodePc) -> bool {
        self.strict_mode_pcs
            .as_deref()
            .is_some_and(|strict_mode_pcs| strict_mode_pcs.binary_search(&pc).is_ok())
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
    internal_stack: InternalStackCertificate,
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
    /// Dense ordinary array construction.
    Arrays,
    /// Synchronous iterator acquisition, stepping, closing, and append.
    Iterators,
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

/// Number of conservative runtime implementation families selectable by the
/// whole-graph compiler authority.
pub const EXECUTION_REQUIREMENT_COUNT: usize = 15;

const fn execution_requirement_ordinal(requirement: ExecutionRequirement) -> usize {
    match requirement {
        ExecutionRequirement::CoreValues => 0,
        ExecutionRequirement::Numbers => 1,
        ExecutionRequirement::Strings => 2,
        ExecutionRequirement::BigInts => 3,
        ExecutionRequirement::Closures => 4,
        ExecutionRequirement::Arrays => 5,
        ExecutionRequirement::Iterators => 6,
        ExecutionRequirement::OrdinaryObjects => 7,
        ExecutionRequirement::DynamicPropertyKeys => 8,
        ExecutionRequirement::Calls => 9,
        ExecutionRequirement::AbruptCompletions => 10,
        ExecutionRequirement::LexicalBindings => 11,
        ExecutionRequirement::RealmGlobalBindings => 12,
        ExecutionRequirement::ObjectOperators => 13,
        ExecutionRequirement::DynamicOperators => 14,
    }
}

const _: () = assert!(
    EXECUTION_REQUIREMENT_COUNT
        == execution_requirement_ordinal(ExecutionRequirement::DynamicOperators) + 1
);

/// Borrowed complete view of one function in [`VerifiedBytecode`].
#[derive(Clone, Copy, Debug)]
pub struct VerifiedBytecodeFunction<'graph> {
    function: &'graph VerifiedCompilerFunction,
    metadata: &'graph VerifiedFunctionMetadata,
    lexical_derived_this: bool,
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

    /// Returns whether this arrow closes over a derived constructor's mutable
    /// `this` binding. The whole-graph verifier derives this authority from an
    /// arrow-only ancestry ending at a derived class constructor.
    #[must_use]
    pub const fn lexical_derived_this(self) -> bool {
        self.lexical_derived_this
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
    lexical_derived_this: Arc<[bool]>,
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
            lexical_derived_this: self.lexical_derived_this[index],
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
            lexical_derived_this: *self.lexical_derived_this.get(index)?,
        })
    }

    /// Iterates complete function views in dense template order.
    #[must_use]
    pub fn functions(&self) -> impl ExactSizeIterator<Item = VerifiedBytecodeFunction<'_>> {
        self.graph
            .functions()
            .iter()
            .zip(self.metadata.iter())
            .zip(self.lexical_derived_this.iter().copied())
            .map(
                |((function, metadata), lexical_derived_this)| VerifiedBytecodeFunction {
                    function,
                    metadata,
                    lexical_derived_this,
                },
            )
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
    /// A Global Script record is not the graph root.
    GlobalScriptNotRoot,
    /// A Global Script record declares a call-argument domain.
    GlobalScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A Global Script record carries function-name metadata or a named
    /// function self binding.
    GlobalScriptHasFunctionName,
    /// An indirect-eval Script record is not the graph root.
    IndirectEvalScriptNotRoot,
    /// An indirect-eval Script record declares a call-argument domain.
    IndirectEvalScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// An indirect-eval Script record carries function-name metadata or a
    /// named-function self binding.
    IndirectEvalScriptHasFunctionName,
    /// A direct-eval Script record is not the graph root.
    DirectEvalScriptNotRoot,
    /// A direct-eval Script record declares a call-argument domain.
    DirectEvalScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A direct-eval Script record carries function-name metadata or a
    /// named-function self binding.
    DirectEvalScriptHasFunctionName,
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
    /// An arrow carries compiler source-name metadata or a self binding even
    /// though observable names are assigned only when its closure is created.
    OrdinaryArrowHasFunctionName,
    /// A constructor-realm global source appears outside a Script authority
    /// root.
    ConstructorRealmGlobalSourceRequiresDynamicFunctionScript {
        /// Closure-domain slot containing the source.
        closure: u32,
    },
    /// A caller-binding source appears outside a direct-eval Script authority
    /// root.
    DirectEvalBindingSourceRequiresDirectEvalScript {
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
    /// `delete_var` names no object-backed or unresolved realm-global
    /// reference declared by this function's verified closure metadata.
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
    /// The synthesized `arguments` binding marker does not match its metadata
    /// or bytecode initializer.
    ArgumentsObjectMetadataMismatch {
        /// Marked variable definition, when one was supplied.
        definition: Option<u32>,
        /// `special_object` initializer site, when one was encoded.
        pc: Option<BytecodePc>,
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
    /// An adjusted `eval` scope operand points past the local domain.
    EvalScopeIndexOutOfBounds {
        /// Final bytecode position of the eval operation.
        pc: BytecodePc,
        /// Rejected adjusted scope operand.
        scope_index: u16,
        /// Local-variable count.
        locals: u32,
    },
    /// An adjusted `eval` scope operand selects a function-scoped local.
    EvalScopeHeadNotLexical {
        /// Final bytecode position of the eval operation.
        pc: BytecodePc,
        /// Rejected zero-based local index.
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
    /// A realm-global lexical binding has the wrong number of certified
    /// declaration-initialization sites.
    RealmGlobalLexicalInitializerCountMismatch {
        /// Affected closure-domain slot.
        closure: u32,
        /// Matching `PutVarInit` sites.
        matches: u32,
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
    /// More strict-source PCs were supplied than verified instructions.
    StrictModeInstructionCountOutOfBounds {
        /// Supplied strict-source PCs.
        strict_instructions: u64,
        /// Verified instructions.
        instructions: u64,
    },
    /// Strict-source PCs are not strictly increasing.
    StrictModePcNotIncreasing {
        /// Position of the rejected PC.
        index: u32,
        /// Previous PC.
        previous: BytecodePc,
        /// Rejected PC.
        current: BytecodePc,
    },
    /// A strict-source PC is not a verified instruction boundary.
    StrictModePcNotInstruction {
        /// Position of the rejected PC.
        index: u32,
        /// Rejected PC.
        pc: BytecodePc,
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
    /// A function contains more `gosub` sites than the pinned compatibility
    /// profile permits.
    GosubSiteCountOutOfRange {
        /// Observed `gosub` sites in the function.
        sites: u64,
        /// Inclusive compatibility maximum.
        maximum: u32,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal finally return-address marker.
    FinallyReturnStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow entered a finalizer ordinarily, merged distinct finalizer
    /// identities, or mixed a finally return marker with a JavaScript value.
    FinallyReturnJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained a malformed internal finally return marker.
    FinallyReturnMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
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
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal synchronous `for-of` iterator record.
    ForOfIteratorStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct synchronous `for-of` record identities or
    /// mixed an iterator record with ordinary JavaScript values.
    ForOfIteratorJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained a malformed synchronous `for-of` record.
    ForOfIteratorMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal catch marker.
    CatchMarkerStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct catch identities, entered a handler
    /// normally, or mixed a catch marker with an ordinary JavaScript value.
    CatchMarkerJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained an internal catch marker.
    CatchMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// `define_method` is not paired with one immediately preceding typed
    /// ordinary-method closure.
    DefineMethodTemplateMismatch {
        /// Final bytecode position of `define_method`.
        pc: BytecodePc,
    },
    /// `define_class` is not paired with one immediately preceding typed
    /// base-class constructor closure.
    DefineClassTemplateMismatch {
        /// Final bytecode position of `define_class`.
        pc: BytecodePc,
    },
    /// An inferred-name opcode is not paired with an anonymous ordinary
    /// closure or isolated base-class definition on its unique incoming
    /// edge, or a computed name is detached from its data-property definition.
    SetNameTemplateMismatch {
        /// Final bytecode position of the inferred-name opcode.
        pc: BytecodePc,
    },
    /// `define_method` does not target one certified object-literal or class
    /// slot with the matching enumerability on every incoming control-flow
    /// path.
    DefineMethodTargetMismatch {
        /// Final bytecode position of `define_method`.
        pc: BytecodePc,
    },
    /// A base-class constructor closure was not consumed by exactly one
    /// paired `define_class` instruction.
    ClassConstructorTemplateOwnershipMismatch {
        /// Class-constructor template with the invalid ownership count.
        child: FunctionTemplateId,
        /// Number of paired `define_class` instructions that consumed it.
        definitions: u32,
    },
    /// `define_array_el` did not receive a key converted by `to_propkey`
    /// before its value was evaluated on the same fresh object literal.
    DefineArrayElementKeyMismatch {
        /// Final bytecode position of `define_array_el`.
        pc: BytecodePc,
    },
    /// `append` or one of its compiler-owned setup operations did not retain
    /// one unaliased `array_from` destination and its checked integer cursor.
    AppendOperandStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// A terminal path retained compiler-internal array-append provenance.
    AppendMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// Control flow merged distinct linear array-append ownership states.
    AppendProvenanceJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with established provenance.
        incoming_from: BytecodePc,
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
            Self::GlobalScriptNotRoot => {
                formatter.write_str("Global Script executable is not the graph root")
            }
            Self::GlobalScriptHasArguments { defined, arguments } => write!(
                formatter,
                "Global Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::GlobalScriptHasFunctionName => formatter
                .write_str("Global Script carries function-name metadata or a self binding"),
            Self::IndirectEvalScriptNotRoot => {
                formatter.write_str("indirect-eval Script executable is not the graph root")
            }
            Self::IndirectEvalScriptHasArguments { defined, arguments } => write!(
                formatter,
                "indirect-eval Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::IndirectEvalScriptHasFunctionName => formatter
                .write_str("indirect-eval Script carries function-name metadata or a self binding"),
            Self::DirectEvalScriptNotRoot => {
                formatter.write_str("direct-eval Script executable is not the graph root")
            }
            Self::DirectEvalScriptHasArguments { defined, arguments } => write!(
                formatter,
                "direct-eval Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::DirectEvalScriptHasFunctionName => formatter
                .write_str("direct-eval Script carries function-name metadata or a self binding"),
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
            Self::OrdinaryArrowHasFunctionName => {
                formatter.write_str("ordinary arrow carries source-name metadata or a self binding")
            }
            Self::ConstructorRealmGlobalSourceRequiresDynamicFunctionScript { closure } => write!(
                formatter,
                "closure slot {closure} originates a constructor-realm global outside a Script root"
            ),
            Self::DirectEvalBindingSourceRequiresDirectEvalScript { closure } => write!(
                formatter,
                "closure slot {closure} originates a caller binding outside a direct-eval Script root"
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
                "delete_var at PC {pc} names atom {} without an eligible verified realm-global binding",
                atom.get()
            ),
            Self::UnsupportedFunctionHeader => {
                formatter.write_str("function header is outside its compiler executable profile")
            }
            Self::DefinedArgumentCountMismatch { defined, arguments } => write!(
                formatter,
                "defined argument count {defined} is incompatible with argument domain {arguments} and the parameter-list form"
            ),
            Self::VariableDefinitionCountMismatch { declared, entries } => write!(
                formatter,
                "variable definition count {entries} does not equal frame count {declared}"
            ),
            Self::ArgumentsObjectMetadataMismatch { definition, pc } => write!(
                formatter,
                "arguments-object metadata definition {definition:?} disagrees with initializer site {pc:?}"
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
            Self::EvalScopeIndexOutOfBounds {
                pc,
                scope_index,
                locals,
            } => write!(
                formatter,
                "eval scope index {scope_index} at PC {pc} is outside adjusted local count {locals}"
            ),
            Self::EvalScopeHeadNotLexical { pc, local } => write!(
                formatter,
                "eval scope head local {local} at PC {pc} is not lexical"
            ),
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
            Self::RealmGlobalLexicalInitializerCountMismatch { closure, matches } => write!(
                formatter,
                "realm-global lexical closure {closure} has {matches} put_var_init sites"
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
            Self::StrictModeInstructionCountOutOfBounds {
                strict_instructions,
                instructions,
            } => write!(
                formatter,
                "strict-source instruction count {strict_instructions} exceeds instruction count {instructions}"
            ),
            Self::StrictModePcNotIncreasing {
                index,
                previous,
                current,
            } => write!(
                formatter,
                "strict-source PC {index} uses {current}, which does not follow {previous}"
            ),
            Self::StrictModePcNotInstruction { index, pc } => write!(
                formatter,
                "strict-source PC {index} uses {pc}, which is not an instruction boundary"
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
            Self::GosubSiteCountOutOfRange { sites, maximum } => write!(
                formatter,
                "gosub site count {sites} exceeds compatibility maximum {maximum}"
            ),
            Self::FinallyReturnStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed finally return-address stack"
            ),
            Self::FinallyReturnJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed finally return-address stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::FinallyReturnMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains a malformed finally return-address marker"
            ),
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
            Self::ForOfIteratorStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed for-of iterator stack"
            ),
            Self::ForOfIteratorJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed for-of iterator stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::ForOfIteratorMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains an internal for-of iterator marker"
            ),
            Self::CatchMarkerStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed catch-marker stack"
            ),
            Self::CatchMarkerJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed catch-marker stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::CatchMarkerAtExit { pc } => {
                write!(
                    formatter,
                    "terminal at PC {pc} retains an internal catch marker"
                )
            }
            Self::DefineMethodTemplateMismatch { pc } => write!(
                formatter,
                "define_method at PC {pc} is not paired with one typed method closure"
            ),
            Self::DefineClassTemplateMismatch { pc } => write!(
                formatter,
                "define_class at PC {pc} is not paired with one typed class-constructor closure"
            ),
            Self::SetNameTemplateMismatch { pc } => write!(
                formatter,
                "inferred-name opcode at PC {pc} is not paired with one anonymous ordinary-function closure and its required definition"
            ),
            Self::DefineMethodTargetMismatch { pc } => write!(
                formatter,
                "define_method at PC {pc} does not target one certified object or class slot"
            ),
            Self::ClassConstructorTemplateOwnershipMismatch { child, definitions } => write!(
                formatter,
                "class-constructor template {child:?} is consumed by {definitions} define_class instructions"
            ),
            Self::DefineArrayElementKeyMismatch { pc } => write!(
                formatter,
                "define_array_el at PC {pc} does not use a key converted before its value on one fresh object literal"
            ),
            Self::AppendOperandStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} does not retain one fresh array_from destination and checked append cursor"
            ),
            Self::AppendMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains compiler-internal array append provenance"
            ),
            Self::AppendProvenanceJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "linear array append provenance at PC {target} disagrees with the edge from PC {incoming_from}"
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
    let function_parents = verify_function_tree_ownership(&graph)?;
    let mut verified = Vec::new();
    verified.try_reserve_exact(function_count).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::VerifiedMetadata,
            requested: usize_to_u64(function_count),
        })
    })?;
    let mut requirements = Vec::new();
    requirements
        .try_reserve_exact(EXECUTION_REQUIREMENT_COUNT)
        .map_err(|_| {
            BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
                resource: BytecodeGraphResource::VerifiedMetadata,
                requested: usize_to_u64(EXECUTION_REQUIREMENT_COUNT),
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
    let lexical_derived_this =
        verify_lexical_arrow_environments(&graph, &verified, &function_parents)?;
    verify_class_field_key_bindings(&graph, &verified)?;
    verify_inferred_function_names(&graph, &verified)?;
    verify_method_definitions(&graph, &verified, limits, &mut usage)?;

    requirements.sort_unstable();
    Ok(VerifiedBytecode {
        graph,
        metadata: Arc::new(verified),
        lexical_derived_this: lexical_derived_this.into(),
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
) -> Result<Vec<Option<FunctionTemplateId>>, BytecodeVerificationError> {
    let functions = graph.functions();
    let mut incoming = try_filled_vec(
        graph.root_id(),
        functions.len(),
        0_u64,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    let mut parents = try_filled_vec(
        graph.root_id(),
        functions.len(),
        None,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for (parent_index, parent) in functions.iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        for constant in parent.constants() {
            let crate::CompilerConstant::Function(child) = constant else {
                continue;
            };
            let Some(child_index) = usize::try_from(child.get())
                .ok()
                .filter(|&index| index < incoming.len())
            else {
                return Err(BytecodeVerificationError::function(
                    *child,
                    BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
                        child: *child,
                        incoming: 0,
                    },
                ));
            };
            let count = &mut incoming[child_index];
            *count = count.saturating_add(1);
            parents[child_index] = Some(parent_id);
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
    Ok(parents)
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

#[allow(
    clippy::too_many_lines,
    reason = "whole-function metadata validation remains one ordered admission boundary"
)]
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
    verify_eval_scope_operands(id, flow, &metadata.variables)?;
    verify_closures(
        id,
        graph.root_id(),
        authority_kind,
        function,
        &metadata.closures,
    )?;
    verify_source(id, flow, metadata)?;
    verify_supported_opcodes(id, flow, metadata)?;
    let mut internal_stack = verify_internal_operand_stack(id, function, limits, usage)?;
    let realm_global_initializer_prefix = verify_realm_global_function_initializers(
        id,
        graph.root_id(),
        function,
        &metadata.closures,
        &internal_stack,
    )?;
    let function_initializer_prefix =
        realm_global_initializer_prefix.max(function.function_initializer_prefix_start() as usize);
    let initializer_sites = verify_function_initializers(
        id,
        function,
        &metadata.variables,
        function_initializer_prefix,
        &internal_stack,
    )?;
    classify_iteration_declarative_local_puts(
        id,
        flow,
        &metadata.variables,
        &mut internal_stack,
        limits,
        usage,
    )?;
    verify_binding_opcodes(
        id,
        flow,
        &metadata.variables,
        &metadata.closures,
        &internal_stack,
    )?;
    let binding_transfers = verify_binding_states(
        id,
        graph,
        function,
        &metadata.variables,
        &initializer_sites,
        &internal_stack,
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
            strict_mode_pcs: metadata.source.strict_mode_pcs.as_ref().map(Arc::clone),
        },
        internal_stack,
    })
}

fn verify_executable_kind(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    match metadata.executable_kind {
        CompilerExecutableKind::GlobalScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::GlobalScriptNotRoot,
                ));
            }
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::GlobalScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::IndirectEvalScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::IndirectEvalScriptNotRoot,
                ));
            }
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::IndirectEvalScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::DirectEvalScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DirectEvalScriptNotRoot,
                ));
            }
            if metadata_has_local_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DirectEvalScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::OrdinaryFunction
        | CompilerExecutableKind::GeneratorFunction
        | CompilerExecutableKind::AsyncFunction
        | CompilerExecutableKind::AsyncGeneratorFunction => Ok(()),
        CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow => {
            if metadata_has_local_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::OrdinaryArrowHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::OrdinaryMethod
        | CompilerExecutableKind::ClassInstanceInitializer
        | CompilerExecutableKind::GeneratorMethod
        | CompilerExecutableKind::AsyncMethod
        | CompilerExecutableKind::AsyncGeneratorMethod
        | CompilerExecutableKind::ClassConstructor => {
            if metadata_has_local_function_name(metadata) {
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
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptHasFunctionName,
                ));
            }
            Ok(())
        }
    }
}

fn metadata_has_function_name(metadata: &UnverifiedFunctionMetadata) -> bool {
    metadata_has_local_function_name(metadata)
        || metadata
            .closures
            .iter()
            .any(|definition| definition.policy().kind() == CompilerBindingKind::FunctionName)
}

fn metadata_has_local_function_name(metadata: &UnverifiedFunctionMetadata) -> bool {
    metadata.function_name.is_some()
        || metadata
            .variables
            .iter()
            .any(|definition| definition.policy.kind() == CompilerBindingKind::FunctionName)
}

#[allow(
    clippy::too_many_lines,
    reason = "all compiler executable kinds and their exact pinned headers are audited together"
)]
fn verify_header(
    id: FunctionTemplateId,
    executable_kind: CompilerExecutableKind,
    flow: &VerifiedControlFlow,
) -> Result<(), BytecodeVerificationError> {
    let header = *flow.function_header();
    let arguments = flow.domains().argument_count();
    match executable_kind {
        CompilerExecutableKind::GlobalScript => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::GlobalScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::IndirectEvalScript => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::IndirectEvalScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::DirectEvalScript => {
            let flags = header.flags().bits();
            if header.kind() != FunctionKind::Normal
                || flags & !0x15c0 != 0
                || flags & 0x0400 == 0
                || (flags & 0x1000 != 0 && flags & 0x0080 == 0)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DirectEvalScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::OrdinaryFunction => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0641 | 0x0643)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::OrdinaryArrow => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0440 | 0x0442)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncArrow => {
            if header.kind() != FunctionKind::Async
                || !matches!(header.flags().bits(), 0x0460 | 0x0462)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
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
                || !matches!(header.flags().bits(), 0x0740 | 0x0742)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::ClassInstanceInitializer => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0742
                || !header.mode().is_strict()
                || header.defined_argument_count() != 0
                || arguments != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
        }
        CompilerExecutableKind::ClassConstructor => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0748 | 0x074a | 0x07cc | 0x07ce)
                || !header.mode().is_strict()
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::GeneratorFunction => {
            if header.kind() != FunctionKind::Generator
                || !matches!(header.flags().bits(), 0x0650 | 0x0652)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::GeneratorMethod => {
            if header.kind() != FunctionKind::Generator
                || !matches!(header.flags().bits(), 0x0750 | 0x0752)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncFunction => {
            if header.kind() != FunctionKind::Async
                || !matches!(header.flags().bits(), 0x0660 | 0x0662)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncMethod => {
            if header.kind() != FunctionKind::Async
                || !matches!(header.flags().bits(), 0x0760 | 0x0762)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncGeneratorFunction => {
            if header.kind() != FunctionKind::AsyncGenerator
                || !matches!(header.flags().bits(), 0x0670 | 0x0672)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncGeneratorMethod => {
            if header.kind() != FunctionKind::AsyncGenerator
                || !matches!(header.flags().bits(), 0x0770 | 0x0772)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
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
    let mut arguments_object_definition = None;
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
        if definition.arguments_object {
            let valid = index >= arguments
                && arguments_object_definition.is_none()
                && atom_contents(definition.name, function.atoms())
                    .is_some_and(|name| name.code_units().eq("arguments".encode_utf16()))
                && definition.policy.kind() == CompilerBindingKind::Var
                && definition.policy.initialization()
                    == CompilerInitializationPolicy::UndefinedAtInstantiation
                && definition.policy.writes() == CompilerWritePolicy::Mutable
                && !definition.policy.has_temporal_dead_zone()
                && !definition.has_scope;
            if !valid {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::ArgumentsObjectMetadataMismatch {
                        definition: Some(definition_index),
                        pc: None,
                    },
                ));
            }
            arguments_object_definition = Some(definition_index);
        }
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
                || definition.policy.temporal_dead_zone
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
    internal_stack: &InternalStackCertificate,
) -> Result<VerifiedFunctionInitializers, BytecodeVerificationError> {
    let flow = function.control_flow();
    let instructions = flow.instructions();
    let mut predecessor_counts = try_filled_vec(
        id,
        instructions.len(),
        0_u32,
        BytecodeGraphResource::SourceMappings,
    )?;
    for index in 0..instructions.len() {
        for edge in internal_stack.effective_successors(instructions, index) {
            let successor = edge.target;
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
            || !internal_stack.has_effective_successor(instructions, index, usize_to_u32(index + 1))
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
        internal_stack,
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
                || !internal_stack.has_effective_successor(
                    instructions,
                    prefix_index,
                    usize_to_u32(prefix_index + 1),
                )
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

#[allow(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
fn verify_scope_function_initializer_groups(
    id: FunctionTemplateId,
    variables: &[VariableDefinition],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    closure_definitions: &[Option<usize>],
    put_definitions: &[Option<usize>],
    argument_count: usize,
    internal_stack: &InternalStackCertificate,
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
                && !internal_stack.has_effective_successor(
                    instructions,
                    edge,
                    usize_to_u32(edge + 1),
                )
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

/// Validates `QuickJS`'s adjusted direct-eval scope encoding.
///
/// `0` is `ARG_SCOPE_END`, `1` is the ordinary `-1` end sentinel, and every
/// larger value is a zero-based local index plus two. A concrete head must be
/// lexical; function-scoped arguments and locals are appended by direct-eval
/// environment construction after walking this lexical chain.
fn verify_eval_scope_operands(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let arguments = flow.domains().argument_count() as usize;
    let locals = &variables[arguments..];
    for verified in flow.instructions() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let ((FinalOpcode::Eval, Operands::NPopU16 { scope_index, .. })
        | (FinalOpcode::ApplyEval, Operands::U16(scope_index))) =
            (instruction.opcode(), instruction.operands())
        else {
            continue;
        };
        let Some(local) = scope_index.checked_sub(2).map(u32::from) else {
            continue;
        };
        let Some(definition) = locals.get(local as usize) else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::EvalScopeIndexOutOfBounds {
                    pc: decoded.pc(),
                    scope_index,
                    locals: usize_to_u32(locals.len()),
                },
            ));
        };
        if !definition.has_scope {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::EvalScopeHeadNotLexical {
                    pc: decoded.pc(),
                    local,
                },
            ));
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
    internal_stack: &InternalStackCertificate,
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
    for index in 0..instructions.len() {
        for edge in internal_stack.effective_successors(instructions, index) {
            let successor = edge.target;
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
            || !internal_stack.has_effective_successor(instructions, index, usize_to_u32(index + 1))
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

#[allow(
    clippy::too_many_lines,
    reason = "closure provenance, storage policy, and initializer checks form one audited boundary"
)]
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
        if matches!(
            staged_source,
            CompilerClosureSource::DirectEvalBinding { .. }
                | CompilerClosureSource::DirectEvalVariable { .. }
        ) && (id != root || authority_kind != CompilerExecutableKind::DirectEvalScript)
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::DirectEvalBindingSourceRequiresDirectEvalScript {
                    closure: usize_to_u32(index),
                },
            ));
        }
        let slot = BindingSlot::Closure(usize_to_u32(index));
        verify_required_atom(
            id,
            closure.name,
            MetadataAtomField::ClosureName(usize_to_u32(index)),
            function,
        )?;
        let policy = closure.policy();
        let arguments_object_valid = !closure.arguments_object
            || (atom_contents(closure.name, function.atoms())
                .is_some_and(|name| name.code_units().eq("arguments".encode_utf16()))
                && policy.kind() == CompilerBindingKind::Var
                && policy.initialization()
                    == CompilerInitializationPolicy::UndefinedAtInstantiation
                && policy.writes() == CompilerWritePolicy::Mutable
                && !policy.has_temporal_dead_zone());
        let deletable_eval_variable_valid = match staged_source {
            CompilerClosureSource::DirectEvalVariable { .. } => closure.deletable_eval_variable,
            CompilerClosureSource::ParentClosure(_) => true,
            CompilerClosureSource::ParentVariableReference(_)
            | CompilerClosureSource::ConstructorRealmGlobal(_)
            | CompilerClosureSource::DirectEvalBinding { .. } => !closure.deletable_eval_variable,
        };
        let binding_valid = match closure.binding {
            CompilerClosureBinding::Captured(_) => {
                policy.is_valid()
                    && arguments_object_valid
                    && deletable_eval_variable_valid
                    && policy.kind() != CompilerBindingKind::GlobalReference
                    && closure.function_initializer.is_none()
                    && matches!(
                        staged_source,
                        CompilerClosureSource::ParentVariableReference(_)
                            | CompilerClosureSource::ParentClosure(_)
                            | CompilerClosureSource::DirectEvalBinding { .. }
                            | CompilerClosureSource::DirectEvalVariable { .. }
                    )
                    && (!matches!(
                        staged_source,
                        CompilerClosureSource::DirectEvalVariable { .. }
                    ) || (matches!(
                        policy.kind(),
                        CompilerBindingKind::Var | CompilerBindingKind::Function
                    ) && closure.name.is_some()))
            }
            CompilerClosureBinding::RealmGlobal(_) => {
                !closure.arguments_object
                    && !closure.deletable_eval_variable
                    && realm_global_policy_supported(policy)
                    && (!matches!(
                        policy.kind(),
                        CompilerBindingKind::Let | CompilerBindingKind::Const
                    ) || authority_kind == CompilerExecutableKind::GlobalScript)
                    && match *staged_source {
                        CompilerClosureSource::ConstructorRealmGlobal(atom) => {
                            if id != root || !is_script_authority_kind(authority_kind) {
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
                        CompilerClosureSource::ParentVariableReference(_)
                        | CompilerClosureSource::DirectEvalBinding { .. }
                        | CompilerClosureSource::DirectEvalVariable { .. } => false,
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
        let initializer_valid = realm_global_function_initializer_is_valid(
            function,
            closure,
            realm_global_function,
            originates_in_constructor_realm,
        );
        if !initializer_valid {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                    closure: usize_to_u32(index),
                    constant: closure.function_initializer,
                },
            ));
        }
        verify_realm_global_lexical_initializer_sites(
            id,
            usize_to_u32(index),
            function,
            closure,
            originates_in_constructor_realm,
        )?;
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

fn realm_global_function_initializer_is_valid(
    function: &VerifiedCompilerFunction,
    closure: &ClosureVariableDefinition,
    realm_global_function: bool,
    originates_in_constructor_realm: bool,
) -> bool {
    match (
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
    }
}

fn verify_realm_global_lexical_initializer_sites(
    id: FunctionTemplateId,
    closure_index: u32,
    function: &VerifiedCompilerFunction,
    closure: &ClosureVariableDefinition,
    originates_in_constructor_realm: bool,
) -> Result<(), BytecodeVerificationError> {
    let realm_global_lexical = matches!(
        closure.binding,
        CompilerClosureBinding::RealmGlobal(policy)
            if matches!(policy.kind(), CompilerBindingKind::Let | CompilerBindingKind::Const)
    );
    if !realm_global_lexical {
        return Ok(());
    }
    let lexical_initializers = function
        .control_flow()
        .instructions()
        .iter()
        .filter(|instruction| {
            matches!(
                (
                    instruction.decoded().instruction().opcode(),
                    instruction.decoded().instruction().operands(),
                ),
                (FinalOpcode::PutVarInit, Operands::VarRef(slot))
                    if u32::from(slot) == closure_index
            )
        })
        .count();
    let expected = usize::from(originates_in_constructor_realm);
    if lexical_initializers != expected {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::RealmGlobalLexicalInitializerCountMismatch {
                closure: closure_index,
                matches: usize_to_u32(lexical_initializers),
            },
        ));
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
        CompilerBindingKind::Let => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::AtDeclaration
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Const => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::AtDeclaration
            ) && matches!(policy.writes(), CompilerWritePolicy::Immutable)
                && policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Parameter
        | CompilerBindingKind::FunctionName
        | CompilerBindingKind::ClassName
        | CompilerBindingKind::ClassFieldKey
        | CompilerBindingKind::ClassInstanceInitializer
        | CompilerBindingKind::ClassPrivateName
        | CompilerBindingKind::ClassStaticReceiver
        | CompilerBindingKind::WithObject
        | CompilerBindingKind::Catch => false,
    }
}

const fn is_script_authority_kind(kind: CompilerExecutableKind) -> bool {
    matches!(
        kind,
        CompilerExecutableKind::GlobalScript
            | CompilerExecutableKind::IndirectEvalScript
            | CompilerExecutableKind::DirectEvalScript
            | CompilerExecutableKind::DynamicFunctionScript
    )
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
    let strict_mode_pcs = source.strict_mode_pcs.as_deref().unwrap_or_default();
    if strict_mode_pcs.len() > flow.instructions().len() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::StrictModeInstructionCountOutOfBounds {
                strict_instructions: usize_to_u64(strict_mode_pcs.len()),
                instructions: usize_to_u64(flow.instructions().len()),
            },
        ));
    }
    for (index, window) in strict_mode_pcs.windows(2).enumerate() {
        let [previous, current] = window else {
            unreachable!("two-entry strict-source window")
        };
        if previous >= current {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::StrictModePcNotIncreasing {
                    index: usize_to_u32(index + 1),
                    previous: *previous,
                    current: *current,
                },
            ));
        }
    }
    for (index, pc) in strict_mode_pcs.iter().copied().enumerate() {
        if !flow.is_instruction_start(pc) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::StrictModePcNotInstruction {
                    index: usize_to_u32(index),
                    pc,
                },
            ));
        }
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
                        .map(|definition| ParentClosureDefinition {
                            name: definition.name,
                            binding: definition.binding,
                            arguments_object: definition.arguments_object,
                            deletable_eval_variable: definition.deletable_eval_variable,
                            atoms: parent.atoms(),
                        }),
                    CompilerClosureSource::ConstructorRealmGlobal(_)
                    | CompilerClosureSource::DirectEvalBinding { .. }
                    | CompilerClosureSource::DirectEvalVariable { .. } => None,
                };
                let matches = expected.is_some_and(|expected| {
                    expected.binding == closure.binding
                        && expected.arguments_object == closure.arguments_object
                        && expected.deletable_eval_variable == closure.deletable_eval_variable
                        && atom_contents(expected.name, expected.atoms)
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

/// Certifies the synthetic cell that retains one computed public field key.
/// The cell is not source-addressable: it has one lexical activation and one
/// immediately-post-`to_prop_key` initialization. An instance key is captured
/// once by its constructor; a static key is instead read once by the class
/// definition after all element keys have been evaluated.
#[allow(
    clippy::too_many_lines,
    reason = "local initialization and every parent-child capture edge are one certificate"
)]
fn verify_class_field_key_bindings(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
) -> Result<(), BytecodeVerificationError> {
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let parent_metadata = &metadata[parent_index];
        let arguments = parent.control_flow().domains().argument_count() as usize;
        let mut captures = try_filled_vec(
            parent_id,
            parent_metadata.variables.len(),
            0_u32,
            BytecodeGraphResource::VariableDefinitions,
        )?;
        let mut direct_reads = try_filled_vec(
            parent_id,
            parent_metadata.variables.len(),
            0_u32,
            BytecodeGraphResource::VariableDefinitions,
        )?;

        for (definition_index, definition) in parent_metadata.variables.iter().enumerate() {
            if definition.policy.kind() != CompilerBindingKind::ClassFieldKey {
                continue;
            }
            let Some(local) = definition_index
                .checked_sub(arguments)
                .and_then(|index| u32::try_from(index).ok())
            else {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Argument(usize_to_u32(definition_index)),
                    None,
                    BindingPolicyViolationReason::InvalidDeclarationPolicy,
                ));
            };
            if !definition.has_scope || definition.function_initializer.is_some() {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Local(local),
                    None,
                    BindingPolicyViolationReason::InvalidDeclarationPolicy,
                ));
            }

            let instructions = parent.control_flow().instructions();
            let mut initialization = None;
            let mut initialization_count = 0_u32;
            let mut direct_read_count = 0_u32;
            for index in 0..instructions.len() {
                let instruction = instructions[index].decoded().instruction();
                if local_operand(instruction.opcode(), instruction.operands()) != Some(local) {
                    continue;
                }
                if instruction.opcode() == FinalOpcode::GetLocCheck {
                    direct_read_count = direct_read_count.saturating_add(1);
                    continue;
                }
                if instruction.opcode() == FinalOpcode::SetLocUninitialized {
                    continue;
                }
                if instruction.opcode() == FinalOpcode::CloseLoc {
                    continue;
                }
                if !is_unchecked_local_put(instruction.opcode()) || index == 0 {
                    return Err(policy_error(
                        parent_id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::InvalidDeclarationPolicy,
                    ));
                }
                initialization_count = initialization_count.saturating_add(1);
                let prior = instructions[index - 1].decoded().instruction();
                if prior.opcode() != FinalOpcode::ToPropKey
                    || !parent_metadata.internal_stack.has_effective_successor(
                        instructions,
                        index - 1,
                        usize_to_u32(index),
                    )
                {
                    return Err(policy_error(
                        parent_id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
                initialization = Some(index);
            }
            if initialization_count != 1 {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Local(local),
                    initialization.and_then(|index| {
                        instructions
                            .get(index)
                            .map(|instruction| instruction.decoded().pc())
                    }),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            direct_reads[definition_index] = direct_read_count;
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
                if closure.policy().kind() != CompilerBindingKind::ClassFieldKey {
                    continue;
                }
                let CompilerClosureSource::ParentVariableReference(reference) = *source else {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                };
                let Some(CompilerCapturedBinding::ScopedLocal(local)) = parent
                    .control_flow()
                    .compiler_capture_layout()
                    .and_then(|layout| layout.binding_for_variable_reference(reference))
                else {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                };
                let Some(definition_index) =
                    arguments.checked_add(local as usize).filter(|&index| {
                        parent_metadata
                            .variables
                            .get(index)
                            .is_some_and(|definition| {
                                definition.policy.kind() == CompilerBindingKind::ClassFieldKey
                            })
                    })
                else {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                };
                if child_metadata.executable_kind
                    != CompilerExecutableKind::ClassInstanceInitializer
                {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                }
                captures[definition_index] = captures[definition_index].saturating_add(1);
            }
        }

        for (definition_index, definition) in parent_metadata.variables.iter().enumerate() {
            if definition.policy.kind() != CompilerBindingKind::ClassFieldKey {
                continue;
            }
            let local = definition_index
                .checked_sub(arguments)
                .and_then(|index| u32::try_from(index).ok())
                .ok_or_else(|| {
                    policy_error(
                        parent_id,
                        BindingSlot::Argument(usize_to_u32(definition_index)),
                        None,
                        BindingPolicyViolationReason::InvalidDeclarationPolicy,
                    )
                })?;
            let valid_use = if definition.variable_reference.is_some() {
                captures[definition_index] == 1 && direct_reads[definition_index] == 0
            } else {
                captures[definition_index] == 0 && direct_reads[definition_index] == 1
            };
            if !valid_use {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Local(local),
                    None,
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "typed class/method closure pairing, unique CFG entry, arity, and ownership form one definition certificate"
)]
fn verify_method_definitions(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let mut method_definition_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    let mut class_definition_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    let mut instance_initializer_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let instructions = parent.control_flow().instructions();
        let internal_stack = &metadata[parent_index].internal_stack;
        let mut predecessor_counts = try_filled_vec(
            parent_id,
            instructions.len(),
            0_u32,
            BytecodeGraphResource::SourceMappings,
        )?;
        for index in 0..instructions.len() {
            for edge in internal_stack.effective_successors(instructions, index) {
                let successor = edge.target;
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
                    internal_stack,
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
            if is_class_definition_opcode(instruction.opcode())
                && class_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::DefineClassTemplateMismatch { pc: decoded.pc() },
                ));
            }
            if instruction.opcode() == FinalOpcode::CheckCtor {
                let default_constructor_check = metadata[parent_index].executable_kind
                    == CompilerExecutableKind::ClassConstructor
                    && parent
                        .control_flow()
                        .function_header()
                        .flags()
                        .is_derived_class_constructor()
                    && index == 0
                    && derived_default_constructor_pair(
                        parent,
                        &metadata[parent_index],
                        &predecessor_counts,
                        internal_stack,
                    );
                let heritage_check = index.checked_add(6).is_some_and(|definition_index| {
                    matches!(
                        instructions.get(definition_index).map(|instruction| {
                            let instruction = instruction.decoded().instruction();
                            (instruction.opcode(), instruction.operands())
                        }),
                        Some((
                            FinalOpcode::DefineClass,
                            Operands::AtomU8 { value, .. },
                        )) if value & 1 != 0
                    ) && class_definition_pair(
                        graph,
                        parent,
                        metadata,
                        instructions,
                        &predecessor_counts,
                        internal_stack,
                        definition_index,
                    )
                    .is_some()
                });
                if !default_constructor_check && !heritage_check {
                    return Err(BytecodeVerificationError::function(
                        parent_id,
                        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                            pc: decoded.pc(),
                            opcode: instruction.opcode(),
                        },
                    ));
                }
            }
            if instruction.opcode() == FinalOpcode::InitCtor
                && !(metadata[parent_index].executable_kind
                    == CompilerExecutableKind::ClassConstructor
                    && parent
                        .control_flow()
                        .function_header()
                        .flags()
                        .is_derived_class_constructor()
                    && derived_default_constructor_pair(
                        parent,
                        &metadata[parent_index],
                        &predecessor_counts,
                        internal_stack,
                    ))
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                        pc: decoded.pc(),
                        opcode: instruction.opcode(),
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
            if child_metadata.executable_kind == CompilerExecutableKind::ClassInstanceInitializer {
                if class_instance_initializer_pair(
                    parent,
                    &metadata[parent_index],
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                ) != Some(*child)
                {
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
                instance_initializer_counts[child_index] =
                    instance_initializer_counts[child_index].saturating_add(1);
                continue;
            }
            if child_metadata.executable_kind == CompilerExecutableKind::ClassConstructor {
                let pair = index.checked_add(1).and_then(|definition_index| {
                    class_definition_pair(
                        graph,
                        parent,
                        metadata,
                        instructions,
                        &predecessor_counts,
                        internal_stack,
                        definition_index,
                    )
                });
                if pair != Some(*child) {
                    return Err(BytecodeVerificationError::function(
                        parent_id,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    ));
                }
                let child_index = usize::try_from(child.get()).map_err(|_| {
                    BytecodeVerificationError::function(
                        *child,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    )
                })?;
                let count = &mut class_definition_counts[child_index];
                *count = count.saturating_add(1);
                continue;
            }
            if !matches!(
                child_metadata.executable_kind,
                CompilerExecutableKind::OrdinaryMethod
                    | CompilerExecutableKind::GeneratorMethod
                    | CompilerExecutableKind::AsyncMethod
                    | CompilerExecutableKind::AsyncGeneratorMethod
            ) {
                continue;
            }
            let pair = index.checked_add(1).and_then(|definition_index| {
                method_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    definition_index,
                )
            });
            let private_method = [4_usize, 10].into_iter().find_map(|offset| {
                index.checked_add(offset).and_then(|set_name_index| {
                    private_method_name_pair(
                        parent,
                        metadata,
                        instructions,
                        &predecessor_counts,
                        internal_stack,
                        set_name_index,
                    )
                })
            });
            if pair.map(|(defined, _)| defined) != Some(*child) && private_method != Some(*child) {
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
            let count = &mut method_definition_counts[child_index];
            *count = count.saturating_add(1);
        }
        if instructions.iter().any(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::ArrayFrom
                    | FinalOpcode::DefineMethod
                    | FinalOpcode::DefineMethodComputed
                    | FinalOpcode::DefineClass
                    | FinalOpcode::CopyDataProperties
                    | FinalOpcode::DefineArrayEl
                    | FinalOpcode::Append
                    | FinalOpcode::Dup1
            )
        }) {
            verify_object_definition_provenance(
                parent_id,
                parent,
                &metadata[parent_index],
                internal_stack,
                limits,
                usage,
            )?;
        }
    }

    for (index, (metadata, &definitions)) in
        metadata.iter().zip(&method_definition_counts).enumerate()
    {
        if !matches!(
            metadata.executable_kind,
            CompilerExecutableKind::OrdinaryMethod
                | CompilerExecutableKind::GeneratorMethod
                | CompilerExecutableKind::AsyncMethod
                | CompilerExecutableKind::AsyncGeneratorMethod
        ) {
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
    for (index, (metadata, &definitions)) in
        metadata.iter().zip(&class_definition_counts).enumerate()
    {
        if metadata.executable_kind != CompilerExecutableKind::ClassConstructor {
            continue;
        }
        let child = function_id(index)?;
        if definitions != 1 {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::ClassConstructorTemplateOwnershipMismatch {
                    child,
                    definitions,
                },
            ));
        }
    }
    for (index, (metadata, &definitions)) in metadata
        .iter()
        .zip(&instance_initializer_counts)
        .enumerate()
    {
        if metadata.executable_kind != CompilerExecutableKind::ClassInstanceInitializer {
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

/// Certifies the compiler-owned named-evaluation primitives. `set_name` and
/// `set_name_computed` may rename only a fresh anonymous ordinary closure, or
/// a fresh base class immediately after a converted computed key and its typed
/// definition. Every form has exactly one effective incoming edge. This
/// prevents arbitrary function objects, methods, named templates, or
/// control-flow joins from acquiring the intrinsic mutation authority.
fn verify_inferred_function_names(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
) -> Result<(), BytecodeVerificationError> {
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let instructions = parent.control_flow().instructions();
        let internal_stack = &metadata[parent_index].internal_stack;
        let mut predecessor_counts = try_filled_vec(
            parent_id,
            instructions.len(),
            0_u32,
            BytecodeGraphResource::SourceMappings,
        )?;
        for index in 0..instructions.len() {
            for edge in internal_stack.effective_successors(instructions, index) {
                let successor = edge.target;
                predecessor_counts[successor.get() as usize] =
                    predecessor_counts[successor.get() as usize].saturating_add(1);
            }
        }

        for (index, verified) in instructions.iter().enumerate() {
            let decoded = verified.decoded();
            let opcode = decoded.instruction().opcode();
            if matches!(opcode, FinalOpcode::SetName | FinalOpcode::SetNameComputed)
                && inferred_function_name_pair(
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
                && inferred_computed_class_name_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
                && inferred_captured_computed_class_name_pair(
                    graph,
                    parent,
                    &metadata[parent_index],
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
                && private_method_name_pair(
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::SetNameTemplateMismatch { pc: decoded.pc() },
                ));
            }
            if opcode == FinalOpcode::SetHomeObject
                && !private_method_home_object_pair(
                    parent,
                    &metadata[parent_index],
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                        pc: decoded.pc(),
                        opcode,
                    },
                ));
            }
        }
    }
    Ok(())
}

/// Certifies one private instance method closure created during class
/// definition evaluation. The method name is set only on a fresh anonymous
/// method template, after the surrounding class prototype has become its home
/// object, and the function is immediately retained in a class-local cell.
fn private_instance_method_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetName, Operands::Atom(_))
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let closure_index = set_name_index.checked_sub(4)?;
    if !matches!(
        instructions
            .get(closure_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let expected = [
        (closure_index.checked_add(1)?, FinalOpcode::Swap),
        (closure_index.checked_add(2)?, FinalOpcode::SetHomeObject),
        (closure_index.checked_add(3)?, FinalOpcode::Swap),
    ];
    for (index, opcode) in expected {
        let instruction = instructions.get(index)?.decoded().instruction();
        if instruction.opcode() != opcode {
            return None;
        }
    }
    let store_index = set_name_index.checked_add(1)?;
    if !matches!(
        instructions
            .get(store_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    ) {
        return None;
    }
    for (from, to) in [
        (closure_index, closure_index.checked_add(1)?),
        (closure_index.checked_add(1)?, closure_index.checked_add(2)?),
        (closure_index.checked_add(2)?, closure_index.checked_add(3)?),
        (closure_index.checked_add(3)?, set_name_index),
        (set_name_index, store_index),
    ] {
        if !internal_stack.has_effective_successor(instructions, from, usize_to_u32(to)) {
            return None;
        }
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) && child_metadata.function_name.is_none())
    .then_some(*child)
}

/// Certifies one private static method closure created during class definition
/// evaluation. Its home object is the fresh constructor, and the closure is
/// retained in its class-local cell before that same constructor receives the
/// private method element.
fn private_static_method_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetName, Operands::Atom(_))
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let closure_index = set_name_index.checked_sub(10)?;
    if !matches!(
        instructions
            .get(closure_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let expected = [
        (closure_index.checked_add(1)?, FinalOpcode::Swap),
        (closure_index.checked_add(2)?, FinalOpcode::Perm3),
        (closure_index.checked_add(3)?, FinalOpcode::Swap),
        (closure_index.checked_add(4)?, FinalOpcode::Perm3),
        (closure_index.checked_add(5)?, FinalOpcode::SetHomeObject),
        (closure_index.checked_add(6)?, FinalOpcode::Perm3),
        (closure_index.checked_add(7)?, FinalOpcode::Swap),
        (closure_index.checked_add(8)?, FinalOpcode::Perm3),
        (closure_index.checked_add(9)?, FinalOpcode::Swap),
    ];
    for (index, opcode) in expected {
        if instructions.get(index)?.decoded().instruction().opcode() != opcode {
            return None;
        }
    }
    let store_index = set_name_index.checked_add(1)?;
    if !matches!(
        instructions
            .get(store_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    ) {
        return None;
    }
    for from in closure_index..store_index {
        if !internal_stack.has_effective_successor(
            instructions,
            from,
            usize_to_u32(from.checked_add(1)?),
        ) {
            return None;
        }
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) && child_metadata.function_name.is_none())
    .then_some(*child)
}

fn private_method_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    private_instance_method_name_pair(
        parent,
        metadata,
        instructions,
        predecessor_counts,
        internal_stack,
        set_name_index,
    )
    .or_else(|| {
        private_static_method_name_pair(
            parent,
            metadata,
            instructions,
            predecessor_counts,
            internal_stack,
            set_name_index,
        )
    })
}

/// Certifies the hidden instance-element initializer closure created
/// immediately after its class definition. The fresh class prototype becomes
/// the method's home object, and the closure is then published only into the
/// compiler-owned immutable initializer cell.
fn class_instance_initializer_pair(
    parent: &VerifiedCompilerFunction,
    parent_metadata: &VerifiedFunctionMetadata,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    closure_index: usize,
) -> Option<FunctionTemplateId> {
    let class_index = closure_index.checked_sub(1)?;
    let class_instruction = instructions.get(class_index)?.decoded().instruction();
    if !matches!(
        (class_instruction.opcode(), class_instruction.operands()),
        (FinalOpcode::DefineClass, Operands::AtomU8 { value, .. }) if value & 2 != 0
    ) {
        return None;
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    if !matches!(
        closure.opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let swap_home_index = closure_index.checked_add(1)?;
    let home_index = closure_index.checked_add(2)?;
    let swap_store_index = closure_index.checked_add(3)?;
    let store_index = closure_index.checked_add(4)?;
    for (index, opcode) in [
        (swap_home_index, FinalOpcode::Swap),
        (home_index, FinalOpcode::SetHomeObject),
        (swap_store_index, FinalOpcode::Swap),
    ] {
        if instructions.get(index)?.decoded().instruction().opcode() != opcode
            || predecessor_counts.get(index) != Some(&1)
        {
            return None;
        }
    }
    let store = instructions.get(store_index)?.decoded().instruction();
    if !is_unchecked_local_put(store.opcode()) {
        return None;
    }
    let local = local_operand(store.opcode(), store.operands())?;
    let arguments = parent.control_flow().domains().argument_count() as usize;
    if parent_metadata
        .variables
        .get(arguments.checked_add(local as usize)?)?
        .policy()
        .kind()
        != CompilerBindingKind::ClassInstanceInitializer
        || predecessor_counts.get(store_index) != Some(&1)
    {
        return None;
    }
    for from in class_index..store_index {
        if !internal_stack.has_effective_successor(
            instructions,
            from,
            usize_to_u32(from.checked_add(1)?),
        ) {
            return None;
        }
    }
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (child_metadata.executable_kind == CompilerExecutableKind::ClassInstanceInitializer
        && child_metadata.function_name.is_none())
    .then_some(*child)
}

fn private_method_home_object_pair(
    parent: &VerifiedCompilerFunction,
    parent_metadata: &VerifiedFunctionMetadata,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    home_object_index: usize,
) -> bool {
    let instance = home_object_index
        .checked_add(2)
        .and_then(|set_name_index| {
            private_instance_method_name_pair(
                parent,
                metadata,
                instructions,
                predecessor_counts,
                internal_stack,
                set_name_index,
            )
        })
        .is_some();
    let r#static = home_object_index
        .checked_add(5)
        .and_then(|set_name_index| {
            private_static_method_name_pair(
                parent,
                metadata,
                instructions,
                predecessor_counts,
                internal_stack,
                set_name_index,
            )
        })
        .is_some();
    let initializer = home_object_index
        .checked_sub(2)
        .is_some_and(|closure_index| {
            class_instance_initializer_pair(
                parent,
                parent_metadata,
                metadata,
                instructions,
                predecessor_counts,
                internal_stack,
                closure_index,
            )
            .is_some()
        });
    instance || r#static || initializer
}

fn inferred_function_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?;
    let set_name_instruction = set_name.decoded().instruction();
    if !matches!(
        (
            set_name_instruction.opcode(),
            set_name_instruction.operands(),
        ),
        (FinalOpcode::SetName, Operands::Atom(_)) | (FinalOpcode::SetNameComputed, Operands::None)
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    if set_name_instruction.opcode() == FinalOpcode::SetNameComputed {
        let definition_index = set_name_index.checked_add(1)?;
        if instructions
            .get(definition_index)?
            .decoded()
            .instruction()
            .opcode()
            != FinalOpcode::DefineArrayEl
            || !internal_stack.has_effective_successor(
                instructions,
                set_name_index,
                usize_to_u32(definition_index),
            )
        {
            return None;
        }
    }
    let closure_index = set_name_index.checked_sub(1)?;
    if !internal_stack.has_effective_successor(
        instructions,
        closure_index,
        usize_to_u32(set_name_index),
    ) {
        return None;
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryFunction
            | CompilerExecutableKind::OrdinaryArrow
            | CompilerExecutableKind::AsyncArrow
            | CompilerExecutableKind::GeneratorFunction
            | CompilerExecutableKind::AsyncFunction
            | CompilerExecutableKind::AsyncGeneratorFunction
    ) && child_metadata.function_name.is_none())
    .then_some(*child)
}

fn inferred_computed_class_name_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetNameComputed, Operands::None)
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let key_permutation_index = set_name_index.checked_sub(1)?;
    let constructor_swap_index = key_permutation_index.checked_sub(1)?;
    let definition_class_index = constructor_swap_index.checked_sub(1)?;
    let child = class_definition_pair(
        graph,
        parent,
        metadata,
        instructions,
        predecessor_counts,
        internal_stack,
        definition_class_index,
    )?;
    let closure_index = definition_class_index.checked_sub(1)?;
    let undefined_index = closure_index.checked_sub(1)?;
    let key_conversion_index = undefined_index.checked_sub(1)?;
    let expected_opcodes = [
        (key_conversion_index, FinalOpcode::ToPropKey),
        (undefined_index, FinalOpcode::Undefined),
        (definition_class_index, FinalOpcode::DefineClass),
        (constructor_swap_index, FinalOpcode::Swap),
        (key_permutation_index, FinalOpcode::Perm3),
    ];
    for (index, expected) in expected_opcodes {
        let actual = instructions.get(index)?.decoded().instruction().opcode();
        if actual != expected {
            return None;
        }
    }
    if !matches!(
        instructions
            .get(closure_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let sequence = [
        key_conversion_index,
        undefined_index,
        closure_index,
        definition_class_index,
        constructor_swap_index,
        key_permutation_index,
        set_name_index,
    ];
    for pair in sequence.windows(2) {
        if !internal_stack.has_effective_successor(instructions, pair[0], usize_to_u32(pair[1])) {
            return None;
        }
    }
    Some(child)
}

/// Certifies `NamedEvaluation` for an anonymous class in a computed public
/// field initializer. The key is evaluated once during
/// `ClassDefinitionEvaluation` and retained in a compiler-created immutable
/// `ClassFieldKey` cell: locally for a static field or through the constructor
/// capture for an instance field.
#[allow(
    clippy::too_many_arguments,
    reason = "the certificate validates one complete cross-function class-name sequence"
)]
fn inferred_captured_computed_class_name_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    parent_metadata: &VerifiedFunctionMetadata,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetNameComputed, Operands::None)
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let key_permutation_index = set_name_index.checked_sub(1)?;
    let constructor_swap_index = key_permutation_index.checked_sub(1)?;
    let definition_class_index = constructor_swap_index.checked_sub(1)?;
    let closure_index = definition_class_index.checked_sub(1)?;
    let undefined_index = closure_index.checked_sub(1)?;
    let key_read_index = undefined_index.checked_sub(1)?;
    let child = class_definition_pair(
        graph,
        parent,
        metadata,
        instructions,
        predecessor_counts,
        internal_stack,
        definition_class_index,
    )?;
    let key_read = instructions.get(key_read_index)?.decoded().instruction();
    let retained_key = if key_read.opcode() == FinalOpcode::GetVarRefCheck {
        closure_operand(key_read.opcode(), key_read.operands()).is_some_and(|slot| {
            parent_metadata
                .closures()
                .get(slot as usize)
                .is_some_and(|definition| {
                    definition.policy().kind() == CompilerBindingKind::ClassFieldKey
                })
        })
    } else if key_read.opcode() == FinalOpcode::GetLocCheck {
        local_operand(key_read.opcode(), key_read.operands()).is_some_and(|slot| {
            let arguments = parent.control_flow().domains().argument_count() as usize;
            parent_metadata
                .variables
                .get(arguments.saturating_add(slot as usize))
                .is_some_and(|definition| {
                    definition.policy().kind() == CompilerBindingKind::ClassFieldKey
                })
        })
    } else {
        false
    };
    if !retained_key {
        return None;
    }
    let expected_opcodes = [
        (undefined_index, FinalOpcode::Undefined),
        (closure_index, FinalOpcode::FClosure),
        (definition_class_index, FinalOpcode::DefineClass),
        (constructor_swap_index, FinalOpcode::Swap),
        (key_permutation_index, FinalOpcode::Perm3),
    ];
    for (index, expected) in expected_opcodes {
        let actual = instructions.get(index)?.decoded().instruction().opcode();
        if actual != expected
            && !(expected == FinalOpcode::FClosure
                && matches!(actual, FinalOpcode::FClosure | FinalOpcode::FClosure8))
        {
            return None;
        }
    }
    let sequence = [
        key_read_index,
        undefined_index,
        closure_index,
        definition_class_index,
        constructor_swap_index,
        key_permutation_index,
        set_name_index,
    ];
    for pair in sequence.windows(2) {
        if !internal_stack.has_effective_successor(instructions, pair[0], usize_to_u32(pair[1])) {
            return None;
        }
    }
    Some(child)
}

fn method_definition_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
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
    if !matches!(flags, 0..=2 | 4..=6) || predecessor_counts.get(definition_index) != Some(&1) {
        return None;
    }
    let closure_index = definition_index.checked_sub(1)?;
    let closure = instructions.get(closure_index)?;
    if !internal_stack.has_effective_successor(
        instructions,
        closure_index,
        usize_to_u32(definition_index),
    ) {
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
    if !matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) {
        return None;
    }
    // Accessor grammar constrains the complete formal-parameter list, not
    // the observable `length`. A setter with one defaulted parameter has one
    // argument slot while its ExpectedArgumentCount is zero.
    let arguments = graph
        .function(*child)?
        .control_flow()
        .domains()
        .argument_count();
    let kind = flags & 0b11;
    if (kind == 1 && arguments != 0) || (kind == 2 && arguments != 1) {
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

const fn is_class_definition_opcode(opcode: FinalOpcode) -> bool {
    matches!(opcode, FinalOpcode::DefineClass)
}

fn class_definition_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    definition_index: usize,
) -> Option<FunctionTemplateId> {
    let definition = instructions.get(definition_index)?;
    let definition_instruction = definition.decoded().instruction();
    let (FinalOpcode::DefineClass, Operands::AtomU8 { value: flags, .. }) = (
        definition_instruction.opcode(),
        definition_instruction.operands(),
    ) else {
        return None;
    };
    if flags > 3 || predecessor_counts.get(definition_index) != Some(&1) {
        return None;
    }
    let heritage = flags & 1;
    let closure_index = definition_index.checked_sub(1)?;
    if !internal_stack.has_effective_successor(
        instructions,
        closure_index,
        usize_to_u32(definition_index),
    ) {
        return None;
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_index = usize::try_from(child.get()).ok()?;
    let child_metadata = metadata.get(child_index)?;
    if child_metadata.executable_kind != CompilerExecutableKind::ClassConstructor {
        return None;
    }
    let child_function = graph.function(*child)?;
    let derived = child_function
        .control_flow()
        .function_header()
        .flags()
        .is_derived_class_constructor();
    if derived != (heritage == 1) {
        return None;
    }
    if heritage == 1
        && !derived_class_heritage_pair(
            parent,
            instructions,
            predecessor_counts,
            internal_stack,
            definition_index,
        )
    {
        return None;
    }
    Some(*child)
}

/// Proves that the derived `define_class` received the pair produced by
/// `ClassDefinitionEvaluation`: the one evaluated superclass (or `null`) and
/// the exactly-once observed `superclass.prototype` value.  The shape also
/// makes `check_ctor` admissible only at this semantic site.
fn derived_class_heritage_pair(
    parent: &VerifiedCompilerFunction,
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    definition_index: usize,
) -> bool {
    let Some(closure_index) = definition_index.checked_sub(1) else {
        return false;
    };
    let Some(null_index) = closure_index.checked_sub(1) else {
        return false;
    };
    let Some(goto_index) = null_index.checked_sub(1) else {
        return false;
    };
    let Some(get_prototype_index) = goto_index.checked_sub(1) else {
        return false;
    };
    let Some(duplicate_constructor_index) = get_prototype_index.checked_sub(1) else {
        return false;
    };
    let Some(check_constructor_index) = duplicate_constructor_index.checked_sub(1) else {
        return false;
    };
    let Some(if_null_index) = check_constructor_index.checked_sub(1) else {
        return false;
    };
    let Some(null_test_index) = if_null_index.checked_sub(1) else {
        return false;
    };
    let Some(duplicate_heritage_index) = null_test_index.checked_sub(1) else {
        return false;
    };

    let is_prototype_read = instructions
        .get(get_prototype_index)
        .map(|instruction| instruction.decoded().instruction())
        .is_some_and(
            |instruction| match (instruction.opcode(), instruction.operands()) {
                (FinalOpcode::GetField, Operands::Atom(atom)) => usize::try_from(atom.get())
                    .ok()
                    .and_then(|index| parent.atoms().get(index))
                    .is_some_and(|candidate| {
                        candidate.string().latin1_units() == Some(b"prototype")
                    }),
                _ => false,
            },
        );
    if !is_prototype_read {
        return false;
    }

    let expected_opcodes = [
        (duplicate_heritage_index, FinalOpcode::Dup),
        (null_test_index, FinalOpcode::IsNull),
        (check_constructor_index, FinalOpcode::CheckCtor),
        (duplicate_constructor_index, FinalOpcode::Dup),
        (null_index, FinalOpcode::Null),
    ];
    if expected_opcodes.into_iter().any(|(index, expected)| {
        instructions
            .get(index)
            .is_none_or(|instruction| instruction.decoded().instruction().opcode() != expected)
    }) {
        return false;
    }
    if !matches!(
        instructions
            .get(if_null_index)
            .map(|instruction| instruction.decoded().instruction().opcode()),
        Some(FinalOpcode::IfTrue | FinalOpcode::IfTrue8)
    ) || !matches!(
        instructions
            .get(goto_index)
            .map(|instruction| instruction.decoded().instruction().opcode()),
        Some(FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16)
    ) {
        return false;
    }
    let sequence = [
        (duplicate_heritage_index, null_test_index),
        (null_test_index, if_null_index),
        (if_null_index, check_constructor_index),
        (if_null_index, null_index),
        (check_constructor_index, duplicate_constructor_index),
        (duplicate_constructor_index, get_prototype_index),
        (get_prototype_index, goto_index),
        (goto_index, closure_index),
        (null_index, closure_index),
        (closure_index, definition_index),
    ];
    if sequence.into_iter().any(|(from, to)| {
        !internal_stack.has_effective_successor(instructions, from, usize_to_u32(to))
    }) {
        return false;
    }
    predecessor_counts.get(null_index) == Some(&1)
        && predecessor_counts.get(check_constructor_index) == Some(&1)
        && predecessor_counts.get(closure_index) == Some(&2)
}

/// Certifies the complete source-less derived-constructor body. Instance
/// elements are delegated to one compiler-owned hidden method; they are never
/// inlined into the constructor or duplicated at an arrow `super()` site.
fn derived_default_constructor_pair(
    function: &VerifiedCompilerFunction,
    metadata: &VerifiedFunctionMetadata,
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
) -> bool {
    let instructions = function.control_flow().instructions();
    let has_opcodes = |expected: &[FinalOpcode]| {
        instructions.len() == expected.len()
            && instructions
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.decoded().instruction().opcode() == *expected)
    };
    let no_initializer = has_opcodes(&[
        FinalOpcode::CheckCtor,
        FinalOpcode::InitCtor,
        FinalOpcode::Drop,
        FinalOpcode::ReturnUndef,
    ]);
    let initializer = has_opcodes(&[
        FinalOpcode::CheckCtor,
        FinalOpcode::InitCtor,
        FinalOpcode::GetVarRefCheck,
        FinalOpcode::PushThis,
        FinalOpcode::Swap,
        FinalOpcode::CallMethod,
        FinalOpcode::Drop,
        FinalOpcode::Drop,
        FinalOpcode::ReturnUndef,
    ]) && matches!(
        instructions
            .get(5)
            .map(|instruction| instruction.decoded().instruction().operands()),
        Some(Operands::NPop { argument_count: 0 })
    ) && instructions.get(2).is_some_and(|instruction| {
        let instruction = instruction.decoded().instruction();
        closure_operand(instruction.opcode(), instruction.operands()).is_some_and(|slot| {
            metadata
                .closures()
                .get(slot as usize)
                .is_some_and(|definition| {
                    definition.policy().kind() == CompilerBindingKind::ClassInstanceInitializer
                })
        })
    });
    if !no_initializer && !initializer {
        return false;
    }
    predecessor_counts.len() == instructions.len()
        && predecessor_counts.first() == Some(&0)
        && predecessor_counts.iter().skip(1).all(|count| *count == 1)
        && (0..instructions.len().saturating_sub(1)).all(|source| {
            has_only_effective_successor(
                internal_stack,
                instructions,
                source,
                usize_to_u32(source.saturating_add(1)),
            )
        })
}

fn has_only_effective_successor(
    internal_stack: &InternalStackCertificate,
    instructions: &[VerifiedInstruction],
    source: usize,
    target: u32,
) -> bool {
    let mut successors = internal_stack.effective_successors(instructions, source);
    successors
        .next()
        .is_some_and(|edge| edge.target.get() == target)
        && successors.next().is_none()
}

struct ParentClosureDefinition<'metadata> {
    name: Option<AtomPoolIndex>,
    binding: CompilerClosureBinding,
    arguments_object: bool,
    deletable_eval_variable: bool,
    atoms: &'metadata [crate::CompilerAtom],
}

fn parent_definition_for_reference<'metadata>(
    parent: &'metadata VerifiedCompilerFunction,
    metadata: &'metadata VerifiedFunctionMetadata,
    reference: u32,
) -> Option<ParentClosureDefinition<'metadata>> {
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
    (definition.variable_reference == Some(reference)).then_some(ParentClosureDefinition {
        name: definition.name,
        binding: CompilerClosureBinding::Captured(definition.policy),
        arguments_object: definition.arguments_object,
        deletable_eval_variable: false,
        atoms: parent.atoms(),
    })
}

fn atom_contents(
    atom: Option<AtomPoolIndex>,
    atoms: &[crate::CompilerAtom],
) -> Option<&crate::CompilerString> {
    let index = usize::try_from(atom?.get()).ok()?;
    atoms.get(index).map(crate::CompilerAtom::string)
}

pub(super) fn contextual_instance_initializer_sequence(
    flow: &VerifiedControlFlow,
    check_index: usize,
) -> bool {
    let instructions = flow.instructions();
    let expected = [
        (FinalOpcode::CheckCtorReturn, Operands::None),
        (FinalOpcode::SpecialObject, Operands::U8(6)),
        (FinalOpcode::PushThis, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (
            FinalOpcode::CallMethod,
            Operands::NPop { argument_count: 0 },
        ),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
    ];
    for (offset, (opcode, operands)) in expected.into_iter().enumerate() {
        let Some(index) = check_index.checked_add(offset) else {
            return false;
        };
        let Some(instruction) = instructions.get(index) else {
            return false;
        };
        let instruction = instruction.decoded().instruction();
        if instruction.opcode() != opcode || instruction.operands() != operands {
            return false;
        }
        if offset == 0 {
            continue;
        }
        let predecessor_count = instructions
            .iter()
            .filter(|candidate| {
                let successors = candidate.successors();
                successors.fallthrough().map(InstructionIndex::get) == Some(usize_to_u32(index))
                    || successors.branch_target().map(InstructionIndex::get)
                        == Some(usize_to_u32(index))
                    || successors.jump_target().map(InstructionIndex::get)
                        == Some(usize_to_u32(index))
            })
            .count();
        let Some(previous) = instructions.get(index - 1) else {
            return false;
        };
        if predecessor_count != 1
            || previous.successors().kind() != crate::VerifiedSuccessorKind::Fallthrough
            || previous
                .successors()
                .fallthrough()
                .map(InstructionIndex::get)
                != Some(usize_to_u32(index))
        {
            return false;
        }
    }
    true
}

#[allow(
    clippy::too_many_lines,
    reason = "opcode admission and function-kind restrictions share one auditable pass"
)]
fn verify_supported_opcodes(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    let executable_kind = metadata.executable_kind;
    let mut arguments_object_count = 0_u8;
    let mut arguments_object_initializer = None;
    let mut rest_parameter_count = 0_u8;
    let generator = matches!(
        executable_kind,
        CompilerExecutableKind::GeneratorFunction
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    );
    let asynchronous = matches!(
        executable_kind,
        CompilerExecutableKind::AsyncArrow
            | CompilerExecutableKind::AsyncFunction
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    );
    let async_generator = matches!(
        executable_kind,
        CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    );
    let static_field_super = metadata
        .variables
        .iter()
        .map(VariableDefinition::policy)
        .chain(
            metadata
                .closures
                .iter()
                .map(ClosureVariableDefinition::policy),
        )
        .any(|policy| policy.kind() == CompilerBindingKind::ClassStaticReceiver);
    let super_property_authorized = (executable_kind == CompilerExecutableKind::DirectEvalScript
        && (flow.function_header().flags().super_allowed()
            || flow.function_header().flags().super_call_allowed()))
        || matches!(
            executable_kind,
            CompilerExecutableKind::OrdinaryArrow
                | CompilerExecutableKind::AsyncArrow
                | CompilerExecutableKind::OrdinaryMethod
                | CompilerExecutableKind::ClassInstanceInitializer
                | CompilerExecutableKind::GeneratorMethod
                | CompilerExecutableKind::AsyncMethod
                | CompilerExecutableKind::AsyncGeneratorMethod
                | CompilerExecutableKind::ClassConstructor
        )
        || static_field_super;
    let mut initial_yield = None;
    let mapped_arguments_authority = flow
        .compiler_capture_layout()
        .and_then(CompilerCaptureLayout::mapped_arguments)
        .is_some();
    let simple_parameter_list = flow.function_header().flags().has_simple_parameter_list();
    for (instruction_index, instruction) in flow.instructions().iter().enumerate() {
        let decoded = instruction.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if matches!(
            (opcode, instruction.operands()),
            (FinalOpcode::SpecialObject, Operands::U8(0 | 1))
        ) {
            arguments_object_count = arguments_object_count.saturating_add(1);
            arguments_object_initializer = Some((instruction_index, decoded.pc()));
        } else if opcode == FinalOpcode::Rest {
            rest_parameter_count = rest_parameter_count.saturating_add(1);
        } else if opcode == FinalOpcode::InitialYield
            && initial_yield.replace(decoded.pc()).is_some()
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                    pc: decoded.pc(),
                    opcode,
                },
            ));
        }
        let throw_error_authorized = match instruction.operands() {
            Operands::AtomU8 { value: 3, .. } => super_property_authorized,
            Operands::AtomU8 { value: 4, .. } => generator,
            _ => false,
        };
        if !supported_compiler_opcode(opcode)
            || (matches!(
                opcode,
                FinalOpcode::InitialYield
                    | FinalOpcode::Yield
                    | FinalOpcode::YieldStar
                    | FinalOpcode::AsyncYieldStar
            ) && !generator)
            || (opcode == FinalOpcode::Await && !asynchronous)
            || (matches!(
                opcode,
                FinalOpcode::ForAwaitOfStart
                    | FinalOpcode::ForAwaitOfNext
                    | FinalOpcode::IteratorGetValueDone
            ) && !asynchronous)
            || (opcode == FinalOpcode::YieldStar && async_generator)
            || (opcode == FinalOpcode::AsyncYieldStar && !async_generator)
            || (opcode == FinalOpcode::Yield && async_generator && {
                let target = usize_to_u32(instruction_index);
                let immediately_awaited = instruction_index.checked_sub(1).is_some_and(|prior| {
                    let prior = &flow.instructions()[prior];
                    prior.decoded().instruction().opcode() == FinalOpcode::Await
                        && prior.successors().kind() == crate::VerifiedSuccessorKind::Fallthrough
                        && prior.successors().fallthrough().map(InstructionIndex::get)
                            == Some(target)
                });
                let predecessor_count = flow
                    .instructions()
                    .iter()
                    .filter(|candidate| {
                        let successors = candidate.successors();
                        successors.fallthrough().map(InstructionIndex::get) == Some(target)
                            || successors.branch_target().map(InstructionIndex::get) == Some(target)
                            || successors.jump_target().map(InstructionIndex::get) == Some(target)
                    })
                    .count();
                !immediately_awaited || predecessor_count != 1
            })
            || (opcode == FinalOpcode::ReturnAsync && !generator && !asynchronous)
            || (matches!(
                opcode,
                FinalOpcode::IteratorNext
                    | FinalOpcode::IteratorCall
                    | FinalOpcode::IteratorCheckObject
            ) && !generator)
            || (matches!(opcode, FinalOpcode::Return | FinalOpcode::ReturnUndef)
                && (generator || asynchronous))
            || (opcode == FinalOpcode::CheckCtorReturn
                && !(matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
                ) || (executable_kind == CompilerExecutableKind::DirectEvalScript
                    && flow.function_header().flags().super_call_allowed())
                    || (executable_kind == CompilerExecutableKind::ClassConstructor
                        && flow
                            .function_header()
                            .flags()
                            .is_derived_class_constructor())))
            || (opcode == FinalOpcode::CheckCtorReturn
                && executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow
                    .function_header()
                    .flags()
                    .direct_eval_has_instance_elements()
                && !contextual_instance_initializer_sequence(flow, instruction_index))
            || (matches!(
                opcode,
                FinalOpcode::GetSuper | FinalOpcode::GetSuperValue | FinalOpcode::PutSuperValue
            ) && !super_property_authorized)
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::SpecialObject, operands)
                    if !compiler_special_object_is_authorized(
                        operands,
                        flow,
                        executable_kind,
                        arguments_object_count,
                        rest_parameter_count,
                        mapped_arguments_authority,
                        simple_parameter_list,
                    )
            )
            || (matches!(instruction.operands(), Operands::U8(6))
                && opcode == FinalOpcode::SpecialObject
                && !instruction_index.checked_sub(1).is_some_and(|check_index| {
                    contextual_instance_initializer_sequence(flow, check_index)
                }))
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::Rest, Operands::U16(first_argument))
                    if u32::from(first_argument) != flow.domains().argument_count()
                        || simple_parameter_list
                        || !matches!(
                            executable_kind,
                            CompilerExecutableKind::OrdinaryFunction
                                | CompilerExecutableKind::OrdinaryArrow
                                | CompilerExecutableKind::AsyncArrow
                                | CompilerExecutableKind::OrdinaryMethod
                                | CompilerExecutableKind::ClassConstructor
                                | CompilerExecutableKind::GeneratorFunction
                                | CompilerExecutableKind::GeneratorMethod
                                | CompilerExecutableKind::AsyncFunction
                                | CompilerExecutableKind::AsyncMethod
                                | CompilerExecutableKind::AsyncGeneratorFunction
                                | CompilerExecutableKind::AsyncGeneratorMethod
                        )
                        || rest_parameter_count != 1
            )
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::DefineMethod, Operands::AtomU8 { value, .. })
                    | (FinalOpcode::DefineMethodComputed, Operands::U8(value))
                    if !matches!(value, 0..=2 | 4..=6)
            )
            || (opcode == FinalOpcode::ThrowError && !throw_error_authorized)
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
    let arguments_object_definition = metadata
        .variables
        .iter()
        .position(VariableDefinition::is_arguments_object)
        .map(usize_to_u32);
    let initialized_definition = arguments_object_initializer.and_then(|(index, _)| {
        flow.instructions()
            .get(index.checked_add(1)?)
            .and_then(|put| {
                let put = put.decoded().instruction();
                initializer_put_definition(
                    put.opcode(),
                    put.operands(),
                    flow.domains().argument_count() as usize,
                )
                .map(usize_to_u32)
            })
    });
    if arguments_object_definition != initialized_definition {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ArgumentsObjectMetadataMismatch {
                definition: arguments_object_definition,
                pc: arguments_object_initializer.map(|(_, pc)| pc),
            },
        ));
    }
    if arguments_object_count == 0 && mapped_arguments_authority {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                pc: BytecodePc::ZERO,
                opcode: FinalOpcode::SpecialObject,
            },
        ));
    }
    if generator && initial_yield.is_none() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                pc: BytecodePc::ZERO,
                opcode: FinalOpcode::InitialYield,
            },
        ));
    }
    Ok(())
}

fn compiler_special_object_is_authorized(
    operands: Operands,
    flow: &VerifiedControlFlow,
    executable_kind: CompilerExecutableKind,
    arguments_object_count: u8,
    rest_parameter_count: u8,
    mapped_arguments_authority: bool,
    simple_parameter_list: bool,
) -> bool {
    if !matches!(
        executable_kind,
        CompilerExecutableKind::DirectEvalScript
            | CompilerExecutableKind::OrdinaryFunction
            | CompilerExecutableKind::OrdinaryArrow
            | CompilerExecutableKind::AsyncArrow
            | CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::ClassInstanceInitializer
            | CompilerExecutableKind::ClassConstructor
            | CompilerExecutableKind::GeneratorFunction
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncFunction
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) {
        return false;
    }
    match operands {
        Operands::U8(0) => {
            !matches!(
                executable_kind,
                CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
            ) && (flow.function_header().mode().is_strict() || !simple_parameter_list)
                && arguments_object_count == 1
                && rest_parameter_count == 0
                && !mapped_arguments_authority
        }
        Operands::U8(1) => {
            !matches!(
                executable_kind,
                CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
            ) && !flow.function_header().mode().is_strict()
                && simple_parameter_list
                && arguments_object_count == 1
                && rest_parameter_count == 0
                && mapped_arguments_authority
        }
        Operands::U8(3) => flow.function_header().flags().new_target_allowed(),
        Operands::U8(4) => {
            matches!(
                executable_kind,
                CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
            ) || (executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow.function_header().flags().super_call_allowed())
                || (executable_kind == CompilerExecutableKind::ClassConstructor
                    && flow
                        .function_header()
                        .flags()
                        .is_derived_class_constructor())
        }
        Operands::U8(5) => {
            (executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow.function_header().flags().super_allowed())
                || matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryArrow
                        | CompilerExecutableKind::AsyncArrow
                        | CompilerExecutableKind::OrdinaryMethod
                        | CompilerExecutableKind::ClassInstanceInitializer
                        | CompilerExecutableKind::GeneratorMethod
                        | CompilerExecutableKind::AsyncMethod
                        | CompilerExecutableKind::AsyncGeneratorMethod
                        | CompilerExecutableKind::ClassConstructor
                )
        }
        Operands::U8(6) => {
            (executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow
                    .function_header()
                    .flags()
                    .direct_eval_has_instance_elements())
                || matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
                )
        }
        _ => false,
    }
}

#[allow(clippy::too_many_lines)]
const fn supported_compiler_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PushI32
            | FinalOpcode::PushConst
            | FinalOpcode::FClosure
            | FinalOpcode::SetName
            | FinalOpcode::SetNameComputed
            | FinalOpcode::SetHomeObject
            | FinalOpcode::PushAtomValue
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
            | FinalOpcode::RegExp
            | FinalOpcode::SpecialObject
            | FinalOpcode::Rest
            | FinalOpcode::Drop
            | FinalOpcode::Nip
            | FinalOpcode::Dup
            | FinalOpcode::Dup1
            | FinalOpcode::Dup2
            | FinalOpcode::Dup3
            | FinalOpcode::Insert2
            | FinalOpcode::Insert3
            | FinalOpcode::Insert4
            | FinalOpcode::Swap
            | FinalOpcode::Rot3l
            | FinalOpcode::Rot3r
            | FinalOpcode::Rot4l
            | FinalOpcode::CallConstructor
            | FinalOpcode::Call
            | FinalOpcode::CallMethod
            | FinalOpcode::Apply
            | FinalOpcode::Eval
            | FinalOpcode::ApplyEval
            | FinalOpcode::Import
            | FinalOpcode::WithGetVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef
            | FinalOpcode::PutRefValue
            | FinalOpcode::ArrayFrom
            | FinalOpcode::CheckCtorReturn
            | FinalOpcode::CheckCtor
            | FinalOpcode::InitCtor
            | FinalOpcode::GetSuper
            | FinalOpcode::GetSuperValue
            | FinalOpcode::PutSuperValue
            | FinalOpcode::Perm3
            | FinalOpcode::Perm4
            | FinalOpcode::Perm5
            | FinalOpcode::Return
            | FinalOpcode::ReturnUndef
            | FinalOpcode::ReturnAsync
            | FinalOpcode::Await
            | FinalOpcode::InitialYield
            | FinalOpcode::Yield
            | FinalOpcode::YieldStar
            | FinalOpcode::AsyncYieldStar
            | FinalOpcode::IteratorNext
            | FinalOpcode::IteratorCall
            | FinalOpcode::IteratorCheckObject
            | FinalOpcode::ThrowError
            | FinalOpcode::Throw
            | FinalOpcode::Catch
            | FinalOpcode::NipCatch
            | FinalOpcode::Gosub
            | FinalOpcode::Ret
            | FinalOpcode::GetVarUndef
            | FinalOpcode::GetVar
            | FinalOpcode::PutVar
            | FinalOpcode::PutVarInit
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
            | FinalOpcode::PutLocCheckInit
            | FinalOpcode::SetLocCheck
            | FinalOpcode::GetVarRefCheck
            | FinalOpcode::PutVarRefCheck
            | FinalOpcode::CloseLoc
            | FinalOpcode::PrivateSymbol
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::GetPrivateField
            | FinalOpcode::PrivateIn
            | FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::PutField
            | FinalOpcode::PutPrivateField
            | FinalOpcode::PutArrayEl
            | FinalOpcode::Delete
            | FinalOpcode::SetProto
            | FinalOpcode::ToObject
            | FinalOpcode::ToPropKey
            | FinalOpcode::CopyDataProperties
            | FinalOpcode::DefineField
            | FinalOpcode::DefinePrivateField
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::Append
            | FinalOpcode::DefineClass
            | FinalOpcode::DefineMethod
            | FinalOpcode::DefineMethodComputed
            | FinalOpcode::ForInStart
            | FinalOpcode::ForInNext
            | FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::ForAwaitOfNext
            | FinalOpcode::IteratorGetValueDone
            | FinalOpcode::IteratorClose
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
            | FinalOpcode::IsNull
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
enum InternalStackValue {
    Ordinary,
    DerivedActiveConstructor(BytecodePc),
    DerivedSuperConstructor(BytecodePc),
    DerivedSuperNewTarget(BytecodePc),
    DerivedSuperResult(BytecodePc),
    DerivedSuperCompletion(BytecodePc),
    ForInIterator(BytecodePc),
    ForInKey(BytecodePc),
    ForInDone(BytecodePc),
    ForInHeadKey(BytecodePc),
    ForOfIterator(BytecodePc),
    ForOfNextMethod(BytecodePc),
    ForOfCatch(BytecodePc),
    ForOfDisabledCatch(BytecodePc),
    ForOfAwaitResult(BytecodePc),
    ForOfAwaitedResult(BytecodePc),
    ForOfExhaustedIterator(BytecodePc),
    ForOfExhaustedNextMethod(BytecodePc),
    ForOfExhaustedCatch(BytecodePc),
    ForOfClosableIterator(BytecodePc),
    ForOfClosableNextMethod(BytecodePc),
    ForOfClosableCatch(BytecodePc),
    ForOfValue(BytecodePc),
    ForOfDone(BytecodePc),
    ForOfHeadValue(BytecodePc),
    ForOfReturnValue(BytecodePc),
    ForOfCloseIterator(BytecodePc),
    ForOfCloseNextMethod(BytecodePc),
    ForOfCloseDummy(BytecodePc),
    YieldStarIterator(BytecodePc),
    YieldStarNextMethod(BytecodePc),
    YieldStarDummy(BytecodePc),
    YieldStarIteratorResult(BytecodePc),
    YieldStarDone(BytecodePc),
    YieldStarYieldResult(BytecodePc),
    YieldStarYieldValue(BytecodePc),
    YieldStarFinalResult(BytecodePc),
    YieldStarResumeValue(BytecodePc),
    YieldStarResumeMode(BytecodePc),
    YieldStarResumeModeTest(BytecodePc),
    YieldStarIsThrow(BytecodePc),
    YieldStarCallValue(BytecodePc, YieldStarCallKind),
    YieldStarMethodMissing(BytecodePc, YieldStarCallKind),
    CatchMarker {
        site: BytecodePc,
        handler: InstructionIndex,
    },
    CatchException(BytecodePc),
    FinallyPending {
        target: InstructionIndex,
        original: JavaScriptStackValue,
    },
    FinallyReturn {
        target: InstructionIndex,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum YieldStarCallKind {
    Return,
    Throw,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JavaScriptStackValue {
    Ordinary,
    ForInKey(BytecodePc),
    ForInDone(BytecodePc),
    ForInHeadKey(BytecodePc),
    ForOfValue(BytecodePc),
    ForOfDone(BytecodePc),
    ForOfHeadValue(BytecodePc),
    ForOfReturnValue(BytecodePc),
    CatchException(BytecodePc),
}

impl JavaScriptStackValue {
    const fn from_internal(value: InternalStackValue) -> Option<Self> {
        match value {
            InternalStackValue::ForInKey(site) => Some(Self::ForInKey(site)),
            InternalStackValue::ForInDone(site) => Some(Self::ForInDone(site)),
            InternalStackValue::ForInHeadKey(site) => Some(Self::ForInHeadKey(site)),
            InternalStackValue::ForOfValue(site) => Some(Self::ForOfValue(site)),
            InternalStackValue::ForOfDone(site) => Some(Self::ForOfDone(site)),
            InternalStackValue::ForOfHeadValue(site) => Some(Self::ForOfHeadValue(site)),
            InternalStackValue::ForOfReturnValue(site) => Some(Self::ForOfReturnValue(site)),
            InternalStackValue::CatchException(site) => Some(Self::CatchException(site)),
            InternalStackValue::ForInIterator(_)
            | InternalStackValue::DerivedActiveConstructor(_)
            | InternalStackValue::DerivedSuperConstructor(_)
            | InternalStackValue::DerivedSuperNewTarget(_)
            | InternalStackValue::DerivedSuperCompletion(_)
            | InternalStackValue::ForOfIterator(_)
            | InternalStackValue::ForOfNextMethod(_)
            | InternalStackValue::ForOfCatch(_)
            | InternalStackValue::ForOfDisabledCatch(_)
            | InternalStackValue::ForOfExhaustedIterator(_)
            | InternalStackValue::ForOfExhaustedNextMethod(_)
            | InternalStackValue::ForOfExhaustedCatch(_)
            | InternalStackValue::ForOfClosableIterator(_)
            | InternalStackValue::ForOfClosableNextMethod(_)
            | InternalStackValue::ForOfClosableCatch(_)
            | InternalStackValue::ForOfCloseIterator(_)
            | InternalStackValue::ForOfCloseNextMethod(_)
            | InternalStackValue::ForOfCloseDummy(_)
            | InternalStackValue::YieldStarIterator(_)
            | InternalStackValue::YieldStarNextMethod(_)
            | InternalStackValue::YieldStarDummy(_)
            | InternalStackValue::CatchMarker { .. }
            | InternalStackValue::FinallyPending { .. }
            | InternalStackValue::FinallyReturn { .. } => None,
            InternalStackValue::Ordinary
            | InternalStackValue::DerivedSuperResult(_)
            | InternalStackValue::ForOfAwaitResult(_)
            | InternalStackValue::ForOfAwaitedResult(_)
            | InternalStackValue::YieldStarIteratorResult(_)
            | InternalStackValue::YieldStarDone(_)
            | InternalStackValue::YieldStarYieldResult(_)
            | InternalStackValue::YieldStarYieldValue(_)
            | InternalStackValue::YieldStarFinalResult(_)
            | InternalStackValue::YieldStarResumeValue(_)
            | InternalStackValue::YieldStarResumeMode(_)
            | InternalStackValue::YieldStarResumeModeTest(_)
            | InternalStackValue::YieldStarIsThrow(_)
            | InternalStackValue::YieldStarCallValue(_, _)
            | InternalStackValue::YieldStarMethodMissing(_, _) => Some(Self::Ordinary),
        }
    }

    const fn into_internal(self) -> InternalStackValue {
        match self {
            Self::Ordinary => InternalStackValue::Ordinary,
            Self::ForInKey(site) => InternalStackValue::ForInKey(site),
            Self::ForInDone(site) => InternalStackValue::ForInDone(site),
            Self::ForInHeadKey(site) => InternalStackValue::ForInHeadKey(site),
            Self::ForOfValue(site) => InternalStackValue::ForOfValue(site),
            Self::ForOfDone(site) => InternalStackValue::ForOfDone(site),
            Self::ForOfHeadValue(site) => InternalStackValue::ForOfHeadValue(site),
            Self::ForOfReturnValue(site) => InternalStackValue::ForOfReturnValue(site),
            Self::CatchException(site) => InternalStackValue::CatchException(site),
        }
    }
}

impl InternalStackValue {
    const fn is_javascript_value(self) -> bool {
        !matches!(
            self,
            Self::ForInIterator(_)
                | Self::DerivedActiveConstructor(_)
                | Self::DerivedSuperConstructor(_)
                | Self::DerivedSuperNewTarget(_)
                | Self::DerivedSuperCompletion(_)
                | Self::ForOfIterator(_)
                | Self::ForOfNextMethod(_)
                | Self::ForOfCatch(_)
                | Self::ForOfDisabledCatch(_)
                | Self::ForOfExhaustedIterator(_)
                | Self::ForOfExhaustedNextMethod(_)
                | Self::ForOfExhaustedCatch(_)
                | Self::ForOfClosableIterator(_)
                | Self::ForOfClosableNextMethod(_)
                | Self::ForOfClosableCatch(_)
                | Self::ForOfCloseIterator(_)
                | Self::ForOfCloseNextMethod(_)
                | Self::ForOfCloseDummy(_)
                | Self::YieldStarIterator(_)
                | Self::YieldStarNextMethod(_)
                | Self::YieldStarDummy(_)
                | Self::CatchMarker { .. }
                | Self::FinallyPending { .. }
                | Self::FinallyReturn { .. }
        )
    }

    const fn is_catch_value(self) -> bool {
        matches!(self, Self::CatchMarker { .. } | Self::CatchException(_))
    }

    const fn is_finally_value(self) -> bool {
        matches!(
            self,
            Self::FinallyPending { .. } | Self::FinallyReturn { .. }
        )
    }

    const fn is_for_of_value(self) -> bool {
        matches!(
            self,
            Self::ForOfIterator(_)
                | Self::ForOfNextMethod(_)
                | Self::ForOfCatch(_)
                | Self::ForOfDisabledCatch(_)
                | Self::ForOfAwaitResult(_)
                | Self::ForOfAwaitedResult(_)
                | Self::ForOfExhaustedIterator(_)
                | Self::ForOfExhaustedNextMethod(_)
                | Self::ForOfExhaustedCatch(_)
                | Self::ForOfClosableIterator(_)
                | Self::ForOfClosableNextMethod(_)
                | Self::ForOfClosableCatch(_)
                | Self::ForOfValue(_)
                | Self::ForOfDone(_)
                | Self::ForOfHeadValue(_)
                | Self::ForOfReturnValue(_)
                | Self::ForOfCloseIterator(_)
                | Self::ForOfCloseNextMethod(_)
                | Self::ForOfCloseDummy(_)
                | Self::YieldStarIterator(_)
                | Self::YieldStarNextMethod(_)
                | Self::YieldStarDummy(_)
                | Self::YieldStarIteratorResult(_)
                | Self::YieldStarDone(_)
                | Self::YieldStarYieldResult(_)
                | Self::YieldStarYieldValue(_)
                | Self::YieldStarFinalResult(_)
                | Self::YieldStarResumeValue(_)
                | Self::YieldStarResumeMode(_)
                | Self::YieldStarResumeModeTest(_)
                | Self::YieldStarIsThrow(_)
                | Self::YieldStarCallValue(_, _)
                | Self::YieldStarMethodMissing(_, _)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedIterationLocalPut {
    local: u32,
    cursor_site: BytecodePc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedCatchLocalPut {
    local: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedNipCatchTransform {
    input_depth: u32,
    retained_prefix: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct InternalStackCertificate {
    iteration_local_puts: Vec<Option<CertifiedIterationLocalPut>>,
    catch_local_puts: Vec<Option<CertifiedCatchLocalPut>>,
    nip_catch_transforms: Vec<Option<CertifiedNipCatchTransform>>,
    finally_continuations: Vec<Vec<InstructionIndex>>,
    ret_finalizers: Vec<Option<InstructionIndex>>,
}

impl InternalStackCertificate {
    fn certifies_iteration_local_put(&self, instruction: usize, local: u32) -> bool {
        self.iteration_local_puts
            .get(instruction)
            .copied()
            .flatten()
            .is_some_and(|certificate| certificate.local == local)
    }

    fn certifies_catch_local_put(&self, instruction: usize, local: u32) -> bool {
        self.catch_local_puts
            .get(instruction)
            .copied()
            .flatten()
            .is_some_and(|certificate| certificate.local == local)
    }

    fn nip_catch_transform(&self, instruction: usize) -> Option<CertifiedNipCatchTransform> {
        self.nip_catch_transforms
            .get(instruction)
            .copied()
            .flatten()
    }

    fn effective_successors<'a>(
        &'a self,
        instructions: &'a [VerifiedInstruction],
        instruction: usize,
    ) -> EffectiveSuccessors<'a> {
        let ret_finalizer = self.ret_finalizers.get(instruction).copied().flatten();
        effective_successors(
            instructions,
            instruction,
            &self.finally_continuations,
            ret_finalizer,
        )
    }

    fn has_effective_successor(
        &self,
        instructions: &[VerifiedInstruction],
        instruction: usize,
        target: u32,
    ) -> bool {
        self.effective_successors(instructions, instruction)
            .any(|edge| edge.target.get() == target)
    }

    fn is_finally_target(&self, instruction: usize) -> bool {
        self.finally_continuations
            .get(instruction)
            .is_some_and(|continuations| !continuations.is_empty())
    }
}

#[derive(Clone, Copy, Default)]
struct IterationLocalPutSummary {
    unchecked_puts: u32,
    certified_puts: u32,
    cursor_site: Option<BytecodePc>,
    first_certified_pc: Option<BytecodePc>,
    has_uncertified_put: bool,
    multiple_cursor_sites: bool,
    declarative_authority: bool,
}

#[derive(Clone, Copy)]
struct InternalStackTransfer {
    normal_completion: bool,
    iteration_branch_value: Option<IterationBranchValue>,
    ret_finalizer: Option<InstructionIndex>,
}

#[derive(Clone, Copy)]
enum IterationBranchValue {
    ForIn(BytecodePc),
    ForOf {
        site: BytecodePc,
        extras: usize,
    },
    YieldStarDone {
        site: BytecodePc,
        branch_when_true: bool,
    },
    YieldStarMethod {
        site: BytecodePc,
        kind: YieldStarCallKind,
    },
}

#[derive(Clone, Copy)]
struct EffectiveEdge {
    target: InstructionIndex,
    is_branch_target: bool,
    enters_finally: bool,
}

enum EffectiveSuccessors<'a> {
    Structural {
        edges: [Option<EffectiveEdge>; 3],
        next: usize,
    },
    One(Option<EffectiveEdge>),
    Ret(std::slice::Iter<'a, InstructionIndex>),
}

impl Iterator for EffectiveSuccessors<'_> {
    type Item = EffectiveEdge;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Structural { edges, next } => {
                while *next < edges.len() {
                    let edge = edges[*next].take();
                    *next += 1;
                    if edge.is_some() {
                        return edge;
                    }
                }
                None
            }
            Self::One(edge) => edge.take(),
            Self::Ret(continuations) => continuations.next().copied().map(|target| EffectiveEdge {
                target,
                is_branch_target: false,
                enters_finally: false,
            }),
        }
    }
}

fn effective_successors<'a>(
    instructions: &'a [VerifiedInstruction],
    instruction: usize,
    finally_continuations: &'a [Vec<InstructionIndex>],
    ret_finalizer: Option<InstructionIndex>,
) -> EffectiveSuccessors<'a> {
    let Some(verified) = instructions.get(instruction) else {
        return EffectiveSuccessors::Structural {
            edges: [None; 3],
            next: 0,
        };
    };
    let successors = verified.successors();
    match verified.decoded().instruction().opcode() {
        FinalOpcode::Gosub => {
            EffectiveSuccessors::One(successors.branch_target().map(|target| EffectiveEdge {
                target,
                is_branch_target: false,
                enters_finally: true,
            }))
        }
        FinalOpcode::Ret => {
            let continuations = ret_finalizer
                .and_then(|target| finally_continuations.get(target.get() as usize))
                .map_or([].as_slice(), Vec::as_slice);
            EffectiveSuccessors::Ret(continuations.iter())
        }
        _ => EffectiveSuccessors::Structural {
            edges: [
                successors.fallthrough().map(|target| EffectiveEdge {
                    target,
                    is_branch_target: false,
                    enters_finally: false,
                }),
                successors.branch_target().map(|target| EffectiveEdge {
                    target,
                    is_branch_target: true,
                    enters_finally: false,
                }),
                successors.jump_target().map(|target| EffectiveEdge {
                    target,
                    is_branch_target: false,
                    enters_finally: false,
                }),
            ],
            next: 0,
        },
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded CFG worklist and exact operand-stack transfer form one internal marker certificate"
)]
fn verify_internal_operand_stack(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<InternalStackCertificate, BytecodeVerificationError> {
    let instructions = function.control_flow().instructions();
    let gosub_sites = usize_to_u64(
        instructions
            .iter()
            .filter(|verified| verified.decoded().instruction().opcode() == FinalOpcode::Gosub)
            .count(),
    );
    if gosub_sites > u64::from(MAX_GOSUB_SITES_PER_FUNCTION) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::GosubSiteCountOutOfRange {
                sites: gosub_sites,
                maximum: MAX_GOSUB_SITES_PER_FUNCTION,
            },
        ));
    }
    if !instructions.iter().any(|verified| {
        matches!(
            verified.decoded().instruction().opcode(),
            FinalOpcode::ForInStart
                | FinalOpcode::ForInNext
                | FinalOpcode::ForOfStart
                | FinalOpcode::ForAwaitOfStart
                | FinalOpcode::ForOfNext
                | FinalOpcode::ForAwaitOfNext
                | FinalOpcode::IteratorGetValueDone
                | FinalOpcode::IteratorClose
                | FinalOpcode::Rot3r
                | FinalOpcode::Nip
                | FinalOpcode::Catch
                | FinalOpcode::NipCatch
                | FinalOpcode::Gosub
                | FinalOpcode::Ret
                | FinalOpcode::WithGetVar
                | FinalOpcode::WithDeleteVar
                | FinalOpcode::WithMakeRef
                | FinalOpcode::WithGetRef
        )
    }) {
        return Ok(InternalStackCertificate::default());
    }

    let mut entries = try_filled_vec(
        id,
        instructions.len(),
        None::<Vec<InternalStackValue>>,
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
    let mut iteration_local_puts = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedIterationLocalPut>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut catch_local_puts = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedCatchLocalPut>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut nip_catch_transforms = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedNipCatchTransform>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut catch_handler_targets = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut finally_targets = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut finally_continuations = try_filled_vec(
        id,
        instructions.len(),
        Vec::<InstructionIndex>::new(),
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut ret_finalizers = try_filled_vec(
        id,
        instructions.len(),
        None::<InstructionIndex>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    for verified in instructions {
        match verified.decoded().instruction().opcode() {
            FinalOpcode::Catch => {
                let handler = verified.successors().branch_target().ok_or_else(|| {
                    catch_stack_error(
                        id,
                        verified.decoded().pc(),
                        verified.decoded().instruction().opcode(),
                    )
                })?;
                let target = catch_handler_targets
                    .get_mut(handler.get() as usize)
                    .ok_or_else(|| {
                        catch_stack_error(
                            id,
                            verified.decoded().pc(),
                            verified.decoded().instruction().opcode(),
                        )
                    })?;
                *target = true;
            }
            FinalOpcode::Gosub => {
                let target = verified.successors().branch_target().ok_or_else(|| {
                    finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                })?;
                let continuation = verified.successors().fallthrough().ok_or_else(|| {
                    finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                })?;
                *finally_targets
                    .get_mut(target.get() as usize)
                    .ok_or_else(|| {
                        finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                    })? = true;
                let continuations = finally_continuations
                    .get_mut(target.get() as usize)
                    .ok_or_else(|| {
                        finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                    })?;
                continuations.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                continuations.push(continuation);
                charge_frame_state_entries(id, usage, 1, limits.max_frame_state_entries)?;
            }
            _ => {}
        }
    }
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
            while entries.get(next_seed).is_some_and(Option::is_some)
                || catch_handler_targets.get(next_seed) == Some(&true)
                || finally_targets.get(next_seed) == Some(&true)
            {
                next_seed = next_seed.saturating_add(1);
            }
            if next_seed == entries.len() {
                if let Some(protected) = entries.iter().enumerate().find_map(|(index, entry)| {
                    (entry.is_none() && (catch_handler_targets[index] || finally_targets[index]))
                        .then_some(index)
                }) {
                    let pc = instructions[protected].decoded().pc();
                    let error = if finally_targets[protected] {
                        BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
                            target: pc,
                            incoming_from: pc,
                        }
                    } else {
                        BytecodeVerificationErrorKind::CatchMarkerJoinMismatch {
                            target: pc,
                            incoming_from: pc,
                        }
                    };
                    return Err(BytecodeVerificationError::function(id, error));
                }
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
        let component = components[index].ok_or_else(|| {
            internal_stack_error(id, decoded.pc(), decoded.instruction().opcode(), &[])
        })?;
        let entry = entries[index].as_deref().ok_or_else(|| {
            internal_stack_error(id, decoded.pc(), decoded.instruction().opcode(), &[])
        })?;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(entry.len()).saturating_add(1),
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let mut state = try_copy_slice(id, entry, BytecodeGraphResource::FrameStateEntries)?;
        // Component zero starts at function entry. Later components only audit
        // structurally retained instructions absent from the effective graph,
        // such as a gosub continuation whose finalizer never executes `ret`.
        let effectively_reachable = component == 0;
        let transfer = transfer_internal_operand_stack(
            id,
            index,
            decoded,
            effectively_reachable,
            instructions[index].successors().branch_target(),
            instructions,
            &mut state,
            &mut iteration_local_puts,
            &mut catch_local_puts,
            &mut nip_catch_transforms,
            &mut ret_finalizers,
        )?;
        if !transfer.normal_completion {
            continue;
        }

        let mut has_successor = false;
        for edge in effective_successors(
            instructions,
            index,
            &finally_continuations,
            transfer.ret_finalizer,
        ) {
            has_successor = true;
            let successor = edge.target;
            let (target_pc, target_is_iterator_close) = instructions
                .get(successor.get() as usize)
                .map(|instruction| {
                    (
                        instruction.decoded().pc(),
                        instruction.decoded().instruction().opcode() == FinalOpcode::IteratorClose,
                    )
                })
                .ok_or_else(|| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ForInIteratorJoinMismatch {
                            target: BytecodePc::new(successor.get()),
                            incoming_from: decoded.pc(),
                        },
                    )
                })?;
            let finally_marker = if edge.enters_finally {
                let pending_index = state.len().checked_sub(1).ok_or_else(|| {
                    finally_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                })?;
                let original = JavaScriptStackValue::from_internal(state[pending_index])
                    .ok_or_else(|| {
                        finally_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                    })?;
                state[pending_index] = InternalStackValue::FinallyPending {
                    target: successor,
                    original,
                };
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::FinallyReturn { target: successor });
                Some((pending_index, original))
            } else {
                None
            };
            let branch_value = transfer
                .iteration_branch_value
                .map(|branch_value| {
                    let value_index = state.len().checked_sub(1).ok_or_else(|| {
                        internal_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                            &state,
                        )
                    })?;
                    let replacement = match branch_value {
                        IterationBranchValue::ForIn(site)
                            if state[value_index] == InternalStackValue::ForInKey(site) =>
                        {
                            if edge.is_branch_target {
                                InternalStackValue::ForInHeadKey(site)
                            } else {
                                InternalStackValue::Ordinary
                            }
                        }
                        IterationBranchValue::ForOf { site, extras }
                            if state[value_index] == InternalStackValue::ForOfValue(site) =>
                        {
                            let Some(record_index) =
                                value_index.checked_sub(3_usize.saturating_add(extras))
                            else {
                                return Err(for_of_stack_error(
                                    id,
                                    decoded.pc(),
                                    decoded.instruction().opcode(),
                                ));
                            };
                            if !matches!(
                                (
                                    state[record_index],
                                    state[record_index + 1],
                                    state[record_index + 2],
                                ),
                                (
                                    InternalStackValue::ForOfIterator(iterator),
                                    InternalStackValue::ForOfNextMethod(next),
                                    InternalStackValue::ForOfCatch(catch),
                                ) if iterator == site && next == site && catch == site
                            ) {
                                return Err(for_of_stack_error(
                                    id,
                                    decoded.pc(),
                                    decoded.instruction().opcode(),
                                ));
                            }
                            if edge.is_branch_target {
                                InternalStackValue::ForOfHeadValue(site)
                            } else {
                                state[record_index] =
                                    InternalStackValue::ForOfExhaustedIterator(site);
                                state[record_index + 1] =
                                    InternalStackValue::ForOfExhaustedNextMethod(site);
                                state[record_index + 2] =
                                    InternalStackValue::ForOfExhaustedCatch(site);
                                InternalStackValue::Ordinary
                            }
                        }
                        IterationBranchValue::YieldStarDone {
                            site,
                            branch_when_true,
                        } if state[value_index]
                            == InternalStackValue::YieldStarIteratorResult(site) =>
                        {
                            if edge.is_branch_target == branch_when_true {
                                InternalStackValue::YieldStarFinalResult(site)
                            } else {
                                InternalStackValue::YieldStarYieldResult(site)
                            }
                        }
                        IterationBranchValue::YieldStarMethod { site, kind }
                            if state[value_index]
                                == InternalStackValue::YieldStarCallValue(site, kind) =>
                        {
                            if kind == YieldStarCallKind::Close {
                                InternalStackValue::Ordinary
                            } else if edge.is_branch_target {
                                if kind == YieldStarCallKind::Throw {
                                    InternalStackValue::YieldStarResumeValue(site)
                                } else {
                                    InternalStackValue::Ordinary
                                }
                            } else {
                                InternalStackValue::YieldStarIteratorResult(site)
                            }
                        }
                        _ => {
                            return Err(internal_stack_error(
                                id,
                                decoded.pc(),
                                decoded.instruction().opcode(),
                                &state,
                            ));
                        }
                    };
                    state[value_index] = replacement;
                    Ok((value_index, branch_value))
                })
                .transpose()?;
            let catch_exception = if decoded.instruction().opcode() == FinalOpcode::Catch
                && edge.is_branch_target
            {
                let marker_index = state.len().checked_sub(1).ok_or_else(|| {
                    catch_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                })?;
                let InternalStackValue::CatchMarker { site, handler } = state[marker_index] else {
                    return Err(catch_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                };
                if handler != successor {
                    return Err(catch_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
                state[marker_index] = InternalStackValue::CatchException(site);
                Some((marker_index, site, handler))
            } else {
                None
            };
            let with_binding_results = if edge.is_branch_target {
                match decoded.instruction().opcode() {
                    FinalOpcode::WithGetVar | FinalOpcode::WithDeleteVar => 1,
                    FinalOpcode::WithMakeRef | FinalOpcode::WithGetRef => 2,
                    _ => 0,
                }
            } else {
                0
            };
            if with_binding_results != 0 {
                state.try_reserve(with_binding_results).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: usize_to_u64(with_binding_results),
                        },
                    )
                })?;
                state.extend(std::iter::repeat_n(
                    InternalStackValue::Ordinary,
                    with_binding_results,
                ));
            }
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(state.len()).saturating_add(1),
                usage.policy_transfers,
                limits.max_policy_transfers,
            )?;
            propagate_internal_operand_stack(
                id,
                decoded.pc(),
                successor,
                target_pc,
                component,
                InternalStackTarget {
                    catch_handler: catch_handler_targets[successor.get() as usize],
                    finally_entry: finally_targets[successor.get() as usize],
                    iterator_close: target_is_iterator_close,
                },
                edge.enters_finally,
                &state,
                &mut entries,
                &mut components,
                &mut queued,
                &mut work,
                limits.max_frame_state_entries,
                usage,
            )?;
            if let Some((value_index, branch_value)) = branch_value {
                state[value_index] = match branch_value {
                    IterationBranchValue::ForIn(site) => InternalStackValue::ForInKey(site),
                    IterationBranchValue::ForOf { site, extras } => {
                        let Some(record_index) =
                            value_index.checked_sub(3_usize.saturating_add(extras))
                        else {
                            return Err(for_of_stack_error(
                                id,
                                decoded.pc(),
                                decoded.instruction().opcode(),
                            ));
                        };
                        state[record_index] = InternalStackValue::ForOfIterator(site);
                        state[record_index + 1] = InternalStackValue::ForOfNextMethod(site);
                        state[record_index + 2] = InternalStackValue::ForOfCatch(site);
                        InternalStackValue::ForOfValue(site)
                    }
                    IterationBranchValue::YieldStarDone { site, .. } => {
                        InternalStackValue::YieldStarIteratorResult(site)
                    }
                    IterationBranchValue::YieldStarMethod { site, kind } => {
                        InternalStackValue::YieldStarCallValue(site, kind)
                    }
                };
            }
            if let Some((marker_index, site, handler)) = catch_exception {
                state[marker_index] = InternalStackValue::CatchMarker { site, handler };
            }
            if with_binding_results != 0 {
                state.truncate(state.len() - with_binding_results);
            }
            if let Some((pending_index, original)) = finally_marker {
                match state.pop() {
                    Some(InternalStackValue::FinallyReturn { target }) if target == successor => {}
                    _ => {
                        return Err(finally_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                }
                if !matches!(
                    state.get(pending_index),
                    Some(InternalStackValue::FinallyPending { target, .. })
                        if *target == successor
                ) {
                    return Err(finally_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
                state[pending_index] = original.into_internal();
            }
        }
        if !has_successor {
            verify_internal_stack_exit(
                id,
                decoded,
                &state,
                finally_continuations
                    .iter()
                    .any(|continuations| !continuations.is_empty()),
            )?;
        }
    }

    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    Ok(InternalStackCertificate {
        iteration_local_puts,
        catch_local_puts,
        nip_catch_transforms,
        finally_continuations,
        ret_finalizers,
    })
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the classifier shares the graph resource limits and usage ledger with the typed stack pass"
)]
fn classify_iteration_declarative_local_puts(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
    certificate: &mut InternalStackCertificate,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    if certificate.iteration_local_puts.iter().all(Option::is_none) {
        return Ok(());
    }

    let argument_count = flow.domains().argument_count() as usize;
    let local_count = variables.len() - argument_count;
    charge_frame_state_entries(id, usage, local_count, limits.max_frame_state_entries)?;
    let mut summaries = try_filled_vec(
        id,
        local_count,
        IterationLocalPutSummary::default(),
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

        let certified = certificate
            .iteration_local_puts
            .get(index)
            .copied()
            .flatten();
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
        // Mutable lexical writes are normally proven safe by the binding-state
        // pass once their scope is active. A captured per-iteration `let`
        // declaration is different: its unchecked iterator-head put must be
        // certified so the binding-state pass can require the previous cell
        // to be closed before the backedge reinitializes it. Ordinary
        // iterator-backed assignments use checked puts; mixed or uncertified
        // unchecked puts therefore cannot claim declaration authority.
        let captured_mutable_iteration_declaration = definition.policy.writes
            == CompilerWritePolicy::Mutable
            && definition.policy.temporal_dead_zone
            && definition.has_scope
            && definition.variable_reference.is_some()
            && !summary.has_uncertified_put
            && summary.unchecked_puts == summary.certified_puts;
        if definition.policy.writes == CompilerWritePolicy::Mutable
            && !captured_mutable_iteration_declaration
        {
            continue;
        }
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

    for certified in &mut certificate.iteration_local_puts {
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
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "marker isolation, edge-specific handler and iteration provenance, and ordinary stack transfer form one typed opcode boundary"
)]
fn transfer_internal_operand_stack(
    id: FunctionTemplateId,
    instruction_index: usize,
    decoded: crate::DecodedInstruction,
    effectively_reachable: bool,
    catch_handler: Option<InstructionIndex>,
    instructions: &[VerifiedInstruction],
    state: &mut Vec<InternalStackValue>,
    iteration_local_puts: &mut [Option<CertifiedIterationLocalPut>],
    catch_local_puts: &mut [Option<CertifiedCatchLocalPut>],
    nip_catch_transforms: &mut [Option<CertifiedNipCatchTransform>],
    ret_finalizers: &mut [Option<InstructionIndex>],
) -> Result<InternalStackTransfer, BytecodeVerificationError> {
    let instruction = decoded.instruction();
    let opcode = instruction.opcode();
    match opcode {
        FinalOpcode::SpecialObject => {
            let Operands::U8(selector) = instruction.operands() else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            match selector {
                // The only admitted producer of an active derived-constructor
                // capability. `get_super` must consume it immediately in the
                // typed stack transfer below.
                4 => {
                    state.try_reserve(1).map_err(|_| {
                        BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::AllocationFailed {
                                resource: BytecodeGraphResource::FrameStateEntries,
                                requested: 1,
                            },
                        )
                    })?;
                    state.push(InternalStackValue::DerivedActiveConstructor(decoded.pc()));
                    return Ok(InternalStackTransfer {
                        normal_completion: true,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                // `new.target` becomes a derived-super capability only when
                // it immediately follows the typed superclass constructor.
                // Other source-level uses retain ordinary JavaScript-value
                // treatment through the generic stack transfer.
                3 if matches!(
                    state.last(),
                    Some(InternalStackValue::DerivedSuperConstructor(_))
                ) =>
                {
                    let Some(InternalStackValue::DerivedSuperConstructor(site)) =
                        state.last().copied()
                    else {
                        unreachable!("the derived superclass guard established the value")
                    };
                    state.try_reserve(1).map_err(|_| {
                        BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::AllocationFailed {
                                resource: BytecodeGraphResource::FrameStateEntries,
                                requested: 1,
                            },
                        )
                    })?;
                    state.push(InternalStackValue::DerivedSuperNewTarget(site));
                    return Ok(InternalStackTransfer {
                        normal_completion: true,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                _ => {}
            }
        }
        FinalOpcode::GetSuper
            if matches!(
                state.last(),
                Some(InternalStackValue::DerivedActiveConstructor(_))
            ) =>
        {
            // In a derived constructor `get_super` consumes the typed
            // superclass-constructor capability. In a method (or a static
            // field initializer) it instead consumes an ordinary home object
            // and follows the generic JavaScript-value transfer below.
            *state
                .last_mut()
                .expect("derived active constructor is present") =
                InternalStackValue::DerivedSuperConstructor(decoded.pc());
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::CallConstructor => {
            let Some(argument_count) = instruction.operands().dynamic_argument_count() else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            let required = usize::from(argument_count).saturating_add(2);
            let Some(base) = state.len().checked_sub(required) else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if let (
                InternalStackValue::DerivedSuperConstructor(super_site),
                InternalStackValue::DerivedSuperNewTarget(target_site),
            ) = (state[base], state[base + 1])
            {
                if super_site != target_site
                    || state[base + 2..]
                        .iter()
                        .any(|value| !value.is_javascript_value())
                {
                    return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                }
                state.truncate(base);
                state.push(InternalStackValue::DerivedSuperResult(decoded.pc()));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::Apply if matches!(instruction.operands(), Operands::U16(2)) => {
            let Some(base) = state.len().checked_sub(3) else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if let (
                InternalStackValue::DerivedSuperConstructor(super_site),
                InternalStackValue::DerivedSuperNewTarget(target_site),
            ) = (state[base], state[base + 1])
            {
                if super_site != target_site || !state[base + 2].is_javascript_value() {
                    return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                }
                state.truncate(base);
                state.push(InternalStackValue::DerivedSuperResult(decoded.pc()));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            // Operand mode two is reserved for the proven derived-super
            // transaction. Do not let hand-authored bytecode fall through to
            // ordinary `apply` transfer and reach the runtime construction
            // path without those capabilities.
            return Err(internal_stack_error(id, decoded.pc(), opcode, state));
        }
        FinalOpcode::CheckCtorReturn => {
            let Some(InternalStackValue::DerivedSuperResult(site)) = state.last().copied() else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::DerivedSuperCompletion(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Drop
            if matches!(
                state.last(),
                Some(InternalStackValue::DerivedSuperCompletion(_))
            ) =>
        {
            let Some(base) = state.len().checked_sub(2) else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            let (
                InternalStackValue::DerivedSuperResult(result_site),
                InternalStackValue::DerivedSuperCompletion(completion_site),
            ) = (state[base], state[base + 1])
            else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if result_site != completion_site {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            }
            state[base] = InternalStackValue::Ordinary;
            state.truncate(base + 1);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Catch => {
            invalidate_internal_value_provenance(state);
            let Some(handler) = catch_handler else {
                return Err(catch_stack_error(id, decoded.pc(), opcode));
            };
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::CatchMarker {
                site: decoded.pc(),
                handler,
            });
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForInStart => {
            invalidate_internal_value_provenance(state);
            let Some(input) = state.last_mut() else {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            };
            if *input != InternalStackValue::Ordinary {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            }
            *input = InternalStackValue::ForInIterator(decoded.pc());
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForInNext => {
            invalidate_internal_value_provenance(state);
            let Some(InternalStackValue::ForInIterator(site)) = state.last().copied() else {
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
            state.push(InternalStackValue::ForInKey(site));
            state.push(InternalStackValue::ForInDone(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForOfStart | FinalOpcode::ForAwaitOfStart => {
            invalidate_internal_value_provenance(state);
            let Some(input) = state.last_mut() else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if *input != InternalStackValue::Ordinary {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            let site = decoded.pc();
            *input = InternalStackValue::ForOfIterator(site);
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfNextMethod(site));
            state.push(InternalStackValue::ForOfCatch(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForOfNext => {
            let Operands::U8(offset) = instruction.operands() else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            invalidate_internal_value_provenance(state);
            let Some(base) = state
                .len()
                .checked_sub(3_usize.saturating_add(usize::from(offset)))
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfCatch(catch),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next
                || next != catch
                || for_of_start_is_async(instructions, iterator) != Some(false)
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfValue(iterator));
            state.push(InternalStackValue::ForOfDone(iterator));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForAwaitOfNext => {
            invalidate_internal_value_provenance(state);
            let Some(base) = state.len().checked_sub(3) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfCatch(catch),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next
                || next != catch
                || for_of_start_is_async(instructions, iterator) != Some(true)
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state[base + 2] = InternalStackValue::ForOfDisabledCatch(iterator);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfAwaitResult(iterator));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Await
            if matches!(state.last(), Some(InternalStackValue::ForOfAwaitResult(_))) =>
        {
            let Some(InternalStackValue::ForOfAwaitResult(site)) = state.pop() else {
                unreachable!("the for-await result guard established the top value")
            };
            state.push(InternalStackValue::ForOfAwaitedResult(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IteratorGetValueDone => {
            let Some(base) = state.len().checked_sub(4) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfDisabledCatch(catch),
                InternalStackValue::ForOfAwaitedResult(result),
            ) = (
                state[base],
                state[base + 1],
                state[base + 2],
                state[base + 3],
            )
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next
                || next != catch
                || catch != result
                || for_of_start_is_async(instructions, iterator) != Some(true)
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state[base + 2] = InternalStackValue::ForOfCatch(iterator);
            state[base + 3] = InternalStackValue::ForOfValue(iterator);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfDone(iterator));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IteratorNext => {
            let Some(base) = state.len().checked_sub(4) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::YieldStarIterator(iterator),
                InternalStackValue::YieldStarNextMethod(next),
                InternalStackValue::YieldStarDummy(dummy),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next || next != dummy || !state[base + 3].is_javascript_value() {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state[base + 3] = InternalStackValue::YieldStarIteratorResult(iterator);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IteratorCheckObject
            if matches!(
                state.last(),
                Some(
                    InternalStackValue::YieldStarIteratorResult(_)
                        | InternalStackValue::YieldStarYieldResult(_)
                        | InternalStackValue::YieldStarFinalResult(_)
                )
            ) =>
        {
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::GetField2
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarIteratorResult(_))
            ) =>
        {
            let Some(InternalStackValue::YieldStarIteratorResult(site)) = state.last().copied()
            else {
                unreachable!("delegated iterator result guard established the value")
            };
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::YieldStarDone(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::GetField
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarYieldResult(_))
            ) =>
        {
            let Some(InternalStackValue::YieldStarYieldResult(site)) = state.last().copied() else {
                unreachable!("delegated yield result guard established the value")
            };
            *state.last_mut().expect("matched delegated yield result") =
                InternalStackValue::YieldStarYieldValue(site);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::GetField
            if matches!(
                state.last(),
                Some(
                    InternalStackValue::YieldStarIteratorResult(_)
                        | InternalStackValue::YieldStarFinalResult(_)
                )
            ) =>
        {
            *state.last_mut().expect("matched delegated result") = InternalStackValue::Ordinary;
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::YieldStar | FinalOpcode::AsyncYieldStar => {
            let expected = match opcode {
                FinalOpcode::YieldStar => state.last().copied().and_then(|value| match value {
                    InternalStackValue::YieldStarYieldResult(site) => Some(site),
                    _ => None,
                }),
                FinalOpcode::AsyncYieldStar => {
                    state.last().copied().and_then(|value| match value {
                        InternalStackValue::YieldStarYieldValue(site) => Some(site),
                        _ => None,
                    })
                }
                _ => unreachable!("matched delegated yield opcode"),
            };
            let Some(site) = expected else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            *state.last_mut().expect("matched delegated result") =
                InternalStackValue::YieldStarResumeValue(site);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::YieldStarResumeMode(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Await
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarIteratorResult(_))
            ) =>
        {
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Dup => {
            if let Some(InternalStackValue::YieldStarResumeMode(site)) = state.last().copied() {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::YieldStarResumeModeTest(site));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::Push2 => {
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarResumeMode(_))
            ) {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::Ordinary);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::StrictEq => {
            if let Some(base) = state.len().checked_sub(2)
                && let InternalStackValue::YieldStarResumeMode(site) = state[base]
                && state[base + 1] == InternalStackValue::Ordinary
            {
                state[base] = InternalStackValue::YieldStarIsThrow(site);
                state.truncate(base + 1);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::IteratorCall => {
            let Operands::U8(flags) = instruction.operands() else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let Some(base) = state.len().checked_sub(4) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::YieldStarIterator(iterator),
                InternalStackValue::YieldStarNextMethod(next),
                InternalStackValue::YieldStarDummy(dummy),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let InternalStackValue::YieldStarResumeValue(value_site) = state[base + 3] else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next || next != dummy || dummy != value_site {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            let kind = match flags {
                0 => YieldStarCallKind::Return,
                1 => YieldStarCallKind::Throw,
                2 => YieldStarCallKind::Close,
                _ => return Err(for_of_stack_error(id, decoded.pc(), opcode)),
            };
            state[base + 3] = InternalStackValue::YieldStarCallValue(iterator, kind);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::YieldStarMethodMissing(iterator, kind));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => {
            if let Some(InternalStackValue::YieldStarDone(site)) = state.last().copied()
                && matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::YieldStarIteratorResult(result_site))
                        if *result_site == site
                )
            {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::YieldStarDone {
                        site,
                        branch_when_true: false,
                    }),
                    ret_finalizer: None,
                });
            }
            if let Some((record_index, site)) = for_of_branch_record(state) {
                let value_index = state.len().saturating_sub(2);
                let extras = value_index
                    .saturating_sub(1)
                    .saturating_sub(record_index.saturating_add(2));
                state.pop();
                invalidate_internal_value_provenance(&mut state[..record_index]);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::ForOf { site, extras }),
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    InternalStackValue::ForInIterator(iterator),
                    InternalStackValue::ForInKey(key),
                    InternalStackValue::ForInDone(done),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == key
                && key == done
            {
                state.pop();
                invalidate_internal_value_provenance(&mut state[..base]);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::ForIn(key)),
                    ret_finalizer: None,
                });
            }
            if matches!(state.last(), Some(InternalStackValue::ForOfDone(_))) {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
        }
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8 => {
            if let Some(InternalStackValue::YieldStarDone(site)) = state.last().copied()
                && matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::YieldStarIteratorResult(result_site))
                        if *result_site == site
                )
            {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::YieldStarDone {
                        site,
                        branch_when_true: true,
                    }),
                    ret_finalizer: None,
                });
            }
            if matches!(
                state.last(),
                Some(
                    InternalStackValue::YieldStarResumeModeTest(_)
                        | InternalStackValue::YieldStarIsThrow(_)
                )
            ) {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(InternalStackValue::YieldStarMethodMissing(site, kind)) =
                state.last().copied()
                && matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::YieldStarCallValue(value_site, value_kind))
                        if *value_site == site && *value_kind == kind
                )
            {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::YieldStarMethod {
                        site,
                        kind,
                    }),
                    ret_finalizer: None,
                });
            }
            if matches!(state.last(), Some(InternalStackValue::ForOfDone(_))) {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
        }
        opcode if is_unchecked_local_put(opcode) => {
            if let Some(InternalStackValue::CatchException(_catch_site)) = state.last().copied() {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(catch_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = catch_local_puts.get_mut(instruction_index) else {
                    return Err(catch_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedCatchLocalPut { local });
                state.pop();
                invalidate_internal_value_provenance(state);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(marker) = state.len().checked_sub(2)
                && let (
                    InternalStackValue::ForInIterator(iterator),
                    InternalStackValue::ForInHeadKey(key),
                ) = (state[marker], state[marker + 1])
                && iterator == key
            {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(for_in_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = iteration_local_puts.get_mut(instruction_index) else {
                    return Err(for_in_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedIterationLocalPut {
                    local,
                    cursor_site: key,
                });
                state.pop();
                invalidate_internal_value_provenance(state);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(marker) = state.len().checked_sub(4)
                && let (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch),
                    InternalStackValue::ForOfHeadValue(value),
                ) = (
                    state[marker],
                    state[marker + 1],
                    state[marker + 2],
                    state[marker + 3],
                )
                && iterator == next
                && next == catch
                && catch == value
            {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(for_of_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = iteration_local_puts.get_mut(instruction_index) else {
                    return Err(for_of_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedIterationLocalPut {
                    local,
                    cursor_site: value,
                });
                state.pop();
                invalidate_internal_value_provenance(state);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::Drop => {
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == next
                && next == catch
            {
                state[base] = InternalStackValue::YieldStarIterator(iterator);
                state[base + 1] = InternalStackValue::YieldStarNextMethod(iterator);
                state.truncate(base + 2);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(2)
                && let (
                    InternalStackValue::YieldStarResumeValue(value),
                    InternalStackValue::YieldStarResumeMode(mode),
                ) = (state[base], state[base + 1])
                && value == mode
            {
                state[base] = InternalStackValue::Ordinary;
                state.truncate(base + 1);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            // A `for (const [value] of iterable)` head starts an inner
            // iterator for the pattern. Dropping that iterator's done flag
            // leaves the element value above its record. Preserve a distinct
            // head value only when an enclosing complete for-of record proves
            // that this nested iterator belongs to an iteration head. The
            // following lexical store can then be certified as the permitted
            // fresh initialization of a captured per-iteration binding.
            if let Some(base) = state.len().checked_sub(5)
                && let (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch),
                    InternalStackValue::ForOfValue(value),
                    InternalStackValue::ForOfDone(done),
                ) = (
                    state[base],
                    state[base + 1],
                    state[base + 2],
                    state[base + 3],
                    state[base + 4],
                )
                && iterator == next
                && next == catch
                && catch == value
                && value == done
                && has_enclosing_for_of_record(&state[..base])
            {
                state.truncate(base + 4);
                state[base + 3] = InternalStackValue::ForOfHeadValue(value);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if state.is_empty() {
                if effectively_reachable {
                    return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                }
                return Ok(InternalStackTransfer {
                    normal_completion: false,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if state
                .last()
                .is_some_and(|value| value.is_for_of_value() && !value.is_javascript_value())
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            if let Some(InternalStackValue::FinallyReturn { target }) = state.last().copied()
                && !matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::FinallyPending {
                        target: pending_target,
                        ..
                    }) if *pending_target == target
                )
            {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            }
            state.pop();
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Nip => {
            let Some(value_index) = state.len().checked_sub(1) else {
                if !effectively_reachable {
                    return Ok(InternalStackTransfer {
                        normal_completion: false,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            let Some(marker_index) = value_index.checked_sub(1) else {
                if !effectively_reachable {
                    return Ok(InternalStackTransfer {
                        normal_completion: false,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if !state[value_index].is_javascript_value() {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            }
            if matches!(
                state[marker_index],
                InternalStackValue::YieldStarIterator(_)
                    | InternalStackValue::YieldStarNextMethod(_)
                    | InternalStackValue::YieldStarDummy(_)
            ) {
                state[marker_index] = state[value_index];
                state.truncate(value_index);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            match state[marker_index] {
                InternalStackValue::ForInIterator(_)
                | InternalStackValue::FinallyPending { .. } => {}
                InternalStackValue::FinallyReturn { target } => {
                    if !matches!(
                        marker_index
                            .checked_sub(1)
                            .and_then(|pending| state.get(pending)),
                        Some(InternalStackValue::FinallyPending {
                            target: pending_target,
                            ..
                        }) if *pending_target == target
                    ) {
                        return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                    }
                }
                _ => return Err(internal_stack_error(id, decoded.pc(), opcode, state)),
            }
            state[marker_index] = state[value_index];
            state.truncate(value_index);
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::NipCatch => {
            let Some(value_index) = state.len().checked_sub(1) else {
                if !effectively_reachable {
                    return Ok(InternalStackTransfer {
                        normal_completion: false,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                return Err(catch_stack_error(id, decoded.pc(), opcode));
            };
            if !state[value_index].is_javascript_value() {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            }
            let Some(marker_index) = state[..value_index].iter().rposition(|value| {
                matches!(
                    value,
                    InternalStackValue::CatchMarker { .. }
                        | InternalStackValue::ForOfCatch(_)
                        | InternalStackValue::ForOfExhaustedCatch(_)
                        | InternalStackValue::ForOfClosableCatch(_)
                )
            }) else {
                if state.iter().any(|value| value.is_for_of_value()) {
                    return Err(for_of_stack_error(id, decoded.pc(), opcode));
                }
                return Err(catch_stack_error(id, decoded.pc(), opcode));
            };
            let for_of_site = match state[marker_index] {
                InternalStackValue::CatchMarker { .. } => None,
                InternalStackValue::ForOfCatch(site) => {
                    let Some(record_start) = marker_index.checked_sub(2) else {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    };
                    if !matches!(
                        (state[record_start], state[record_start + 1]),
                        (
                            InternalStackValue::ForOfIterator(iterator),
                            InternalStackValue::ForOfNextMethod(next)
                        ) if iterator == site && next == site
                    ) {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    }
                    Some(site)
                }
                InternalStackValue::ForOfExhaustedCatch(site) => {
                    let Some(record_start) = marker_index.checked_sub(2) else {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    };
                    if !matches!(
                        (state[record_start], state[record_start + 1]),
                        (
                            InternalStackValue::ForOfExhaustedIterator(iterator),
                            InternalStackValue::ForOfExhaustedNextMethod(next)
                        ) if iterator == site && next == site
                    ) {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    }
                    Some(site)
                }
                InternalStackValue::ForOfClosableCatch(site) => {
                    let Some(record_start) = marker_index.checked_sub(2) else {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    };
                    if !matches!(
                        (state[record_start], state[record_start + 1]),
                        (
                            InternalStackValue::ForOfClosableIterator(iterator),
                            InternalStackValue::ForOfClosableNextMethod(next)
                        ) if iterator == site && next == site
                    ) {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    }
                    Some(site)
                }
                _ => return Err(internal_stack_error(id, decoded.pc(), opcode, state)),
            };
            let marker_is_for_of = for_of_site.is_some();
            let mut cursor = marker_index + 1;
            while cursor < value_index {
                match state[cursor] {
                    value if value.is_javascript_value() => {
                        cursor += 1;
                    }
                    InternalStackValue::FinallyPending { target, .. } => {
                        if !matches!(
                            state.get(cursor + 1),
                            Some(InternalStackValue::FinallyReturn {
                                target: return_target
                            }) if *return_target == target && cursor + 1 < value_index
                        ) {
                            return Err(finally_stack_error(id, decoded.pc(), opcode));
                        }
                        cursor += 2;
                    }
                    InternalStackValue::FinallyReturn { .. } => {
                        return Err(finally_stack_error(id, decoded.pc(), opcode));
                    }
                    _ => {
                        return Err(if marker_is_for_of {
                            for_of_stack_error(id, decoded.pc(), opcode)
                        } else {
                            catch_stack_error(id, decoded.pc(), opcode)
                        });
                    }
                }
            }
            let transform = CertifiedNipCatchTransform {
                input_depth: usize_to_u32(state.len()),
                retained_prefix: usize_to_u32(marker_index),
            };
            let Some(certificate) = nip_catch_transforms.get_mut(instruction_index) else {
                return Err(if marker_is_for_of {
                    for_of_stack_error(id, decoded.pc(), opcode)
                } else {
                    catch_stack_error(id, decoded.pc(), opcode)
                });
            };
            match *certificate {
                Some(established) if established != transform => {
                    return Err(if marker_is_for_of {
                        for_of_stack_error(id, decoded.pc(), opcode)
                    } else {
                        catch_stack_error(id, decoded.pc(), opcode)
                    });
                }
                Some(_) => {}
                None => *certificate = Some(transform),
            }
            state.truncate(marker_index);
            invalidate_internal_value_provenance(state);
            state.push(for_of_site.map_or(
                InternalStackValue::Ordinary,
                InternalStackValue::ForOfReturnValue,
            ));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Rot3r => {
            let Some(base) = state.len().checked_sub(3) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let iterator = match (state[base], state[base + 1], state[base + 2]) {
                (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfReturnValue(completion),
                )
                | (
                    InternalStackValue::ForOfExhaustedIterator(iterator),
                    InternalStackValue::ForOfExhaustedNextMethod(next),
                    InternalStackValue::ForOfReturnValue(completion),
                )
                | (
                    InternalStackValue::ForOfClosableIterator(iterator),
                    InternalStackValue::ForOfClosableNextMethod(next),
                    InternalStackValue::ForOfReturnValue(completion),
                ) if iterator == next && next == completion => iterator,
                _ => return Err(for_of_stack_error(id, decoded.pc(), opcode)),
            };
            state[base] = InternalStackValue::Ordinary;
            state[base + 1] = InternalStackValue::ForOfCloseIterator(iterator);
            state[base + 2] = InternalStackValue::ForOfCloseNextMethod(iterator);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Undefined => {
            if let Some(base) = state.len().checked_sub(2)
                && let (
                    InternalStackValue::YieldStarIterator(iterator),
                    InternalStackValue::YieldStarNextMethod(next),
                ) = (state[base], state[base + 1])
                && iterator == next
            {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::YieldStarDummy(iterator));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    InternalStackValue::YieldStarIterator(iterator),
                    InternalStackValue::YieldStarNextMethod(next),
                    InternalStackValue::YieldStarDummy(dummy),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == next
                && next == dummy
            {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::Ordinary);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(3)
                && state[base].is_javascript_value()
                && let (
                    InternalStackValue::ForOfCloseIterator(iterator),
                    InternalStackValue::ForOfCloseNextMethod(next),
                ) = (state[base + 1], state[base + 2])
                && iterator == next
            {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::ForOfCloseDummy(iterator));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::IteratorClose => {
            let Some(base) = state.len().checked_sub(3) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let valid = matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch)
                ) if iterator == next && next == catch
            ) || matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfExhaustedIterator(iterator),
                    InternalStackValue::ForOfExhaustedNextMethod(next),
                    InternalStackValue::ForOfExhaustedCatch(catch)
                ) if iterator == next && next == catch
            ) || matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfClosableIterator(iterator),
                    InternalStackValue::ForOfClosableNextMethod(next),
                    InternalStackValue::ForOfClosableCatch(catch)
                ) if iterator == next && next == catch
            ) || matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfCloseIterator(iterator),
                    InternalStackValue::ForOfCloseNextMethod(next),
                    InternalStackValue::ForOfCloseDummy(dummy)
                ) if iterator == next && next == dummy
            );
            if !valid {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state.truncate(base);
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Ret => {
            if !effectively_reachable && state.is_empty() {
                return Ok(InternalStackTransfer {
                    normal_completion: false,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            let Some(pair_start) = state.len().checked_sub(2) else {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::FinallyPending {
                    target: pending_target,
                    original,
                },
                InternalStackValue::FinallyReturn {
                    target: return_target,
                },
            ) = (state[pair_start], state[pair_start + 1])
            else {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            };
            if pending_target != return_target {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            }
            let target = return_target;
            state.truncate(pair_start);
            state.push(original.into_internal());
            let Some(certificate) = ret_finalizers.get_mut(instruction_index) else {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            };
            match *certificate {
                Some(established) if established != target => {
                    return Err(finally_stack_error(id, decoded.pc(), opcode));
                }
                Some(_) => {}
                None => *certificate = Some(target),
            }
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: Some(target),
            });
        }
        _ => {}
    }

    if state.iter().any(|value| {
        matches!(
            value,
            InternalStackValue::ForOfCloseIterator(_)
                | InternalStackValue::ForOfCloseNextMethod(_)
                | InternalStackValue::ForOfCloseDummy(_)
        )
    }) {
        return Err(for_of_stack_error(id, decoded.pc(), opcode));
    }

    invalidate_internal_value_provenance(state);
    let effect = instruction
        .stack_effect()
        .map_err(|_| internal_stack_error(id, decoded.pc(), opcode, state))?;
    let pops = effect.pops() as usize;
    let pushes = effect.pushes() as usize;
    let Some(input_start) = state.len().checked_sub(pops) else {
        if !effectively_reachable {
            return Ok(InternalStackTransfer {
                normal_completion: false,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        return Err(internal_stack_error(id, decoded.pc(), opcode, state));
    };
    if state[input_start..]
        .iter()
        .any(|value| !value.is_javascript_value())
    {
        return Err(internal_stack_error(
            id,
            decoded.pc(),
            opcode,
            &state[input_start..],
        ));
    }
    let output_len = input_start
        .checked_add(pushes)
        .ok_or_else(|| internal_stack_error(id, decoded.pc(), opcode, state))?;
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
    state.resize(output_len, InternalStackValue::Ordinary);
    Ok(InternalStackTransfer {
        normal_completion: true,
        iteration_branch_value: None,
        ret_finalizer: None,
    })
}

fn for_of_start_is_async(instructions: &[VerifiedInstruction], site: BytecodePc) -> Option<bool> {
    let index = instructions
        .binary_search_by_key(&site, |verified| verified.decoded().pc())
        .ok()?;
    match instructions[index].decoded().instruction().opcode() {
        FinalOpcode::ForOfStart => Some(false),
        FinalOpcode::ForAwaitOfStart => Some(true),
        _ => None,
    }
}

fn has_enclosing_for_of_record(state: &[InternalStackValue]) -> bool {
    state.windows(3).any(|record| {
        matches!(
            record,
            [
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfCatch(catch),
            ] if iterator == next && next == catch
        )
    })
}

fn invalidate_internal_value_provenance(state: &mut [InternalStackValue]) {
    for value in state {
        if matches!(
            value,
            InternalStackValue::ForInKey(_)
                | InternalStackValue::ForInDone(_)
                | InternalStackValue::ForInHeadKey(_)
                | InternalStackValue::ForOfValue(_)
                | InternalStackValue::ForOfDone(_)
                | InternalStackValue::ForOfHeadValue(_)
                | InternalStackValue::ForOfReturnValue(_)
                | InternalStackValue::CatchException(_)
        ) {
            *value = InternalStackValue::Ordinary;
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ForOfRecordState {
    Active,
    Exhausted,
    Closable,
}

#[derive(Clone, Copy)]
struct InternalStackTarget {
    catch_handler: bool,
    finally_entry: bool,
    iterator_close: bool,
}

fn trailing_for_of_record(
    state: &[InternalStackValue],
) -> Option<(usize, BytecodePc, ForOfRecordState)> {
    let base = state.len().checked_sub(3)?;
    let (site, record_state) = match (state[base], state[base + 1], state[base + 2]) {
        (
            InternalStackValue::ForOfIterator(iterator),
            InternalStackValue::ForOfNextMethod(next),
            InternalStackValue::ForOfCatch(catch),
        ) if iterator == next && next == catch => (iterator, ForOfRecordState::Active),
        (
            InternalStackValue::ForOfExhaustedIterator(iterator),
            InternalStackValue::ForOfExhaustedNextMethod(next),
            InternalStackValue::ForOfExhaustedCatch(catch),
        ) if iterator == next && next == catch => (iterator, ForOfRecordState::Exhausted),
        (
            InternalStackValue::ForOfClosableIterator(iterator),
            InternalStackValue::ForOfClosableNextMethod(next),
            InternalStackValue::ForOfClosableCatch(catch),
        ) if iterator == next && next == catch => (iterator, ForOfRecordState::Closable),
        _ => return None,
    };
    Some((base, site, record_state))
}

fn merge_trailing_for_of_close_record(
    established: &mut [InternalStackValue],
    incoming: &[InternalStackValue],
) -> Option<bool> {
    if established.len() != incoming.len() {
        return None;
    }
    let (established_base, established_site, established_state) =
        trailing_for_of_record(established)?;
    let (incoming_base, incoming_site, incoming_state) = trailing_for_of_record(incoming)?;
    if established_base != incoming_base
        || established_site != incoming_site
        || established[..established_base] != incoming[..incoming_base]
    {
        return None;
    }
    let mergeable = established_state == ForOfRecordState::Closable
        || incoming_state == ForOfRecordState::Closable
        || established_state != incoming_state;
    if !mergeable {
        return None;
    }
    let changed = established_state != ForOfRecordState::Closable;
    established[established_base] = InternalStackValue::ForOfClosableIterator(established_site);
    established[established_base + 1] =
        InternalStackValue::ForOfClosableNextMethod(established_site);
    established[established_base + 2] = InternalStackValue::ForOfClosableCatch(established_site);
    Some(changed)
}

#[allow(clippy::too_many_arguments)]
fn propagate_internal_operand_stack(
    id: FunctionTemplateId,
    source_pc: BytecodePc,
    successor: InstructionIndex,
    target_pc: BytecodePc,
    component: u32,
    target: InternalStackTarget,
    enters_finally: bool,
    output: &[InternalStackValue],
    entries: &mut [Option<Vec<InternalStackValue>>],
    components: &mut [Option<u32>],
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
    state_limit: u64,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let index = successor.get() as usize;
    if target.finally_entry {
        if !enters_finally
            || !matches!(
                output.get(output.len().saturating_sub(2)..),
                Some([
                    InternalStackValue::FinallyPending {
                        target: pending_target,
                        ..
                    },
                    InternalStackValue::FinallyReturn {
                        target: return_target
                    }
                ]) if *pending_target == successor && *return_target == successor
            )
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
                    target: target_pc,
                    incoming_from: source_pc,
                },
            ));
        }
    } else if enters_finally {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
                target: target_pc,
                incoming_from: source_pc,
            },
        ));
    }
    let component_slot = components
        .get_mut(index)
        .ok_or_else(|| internal_join_error(id, target_pc, source_pc, output, &[]))?;
    match *component_slot {
        Some(established) if established != component => {
            let existing = entries
                .get(index)
                .and_then(Option::as_deref)
                .ok_or_else(|| internal_join_error(id, target_pc, source_pc, output, &[]))?;
            // A Gosub necessarily contributes its own certified pending/return
            // suffix. Ignore only that suffix when deciding whether a later,
            // disconnected compiler component is trying to smuggle an
            // independently forged marker into an already verified finalizer.
            let component_prefix = if target.finally_entry && enters_finally {
                &output[..output.len() - 2]
            } else {
                output
            };
            let carries_internal_value = component_prefix
                .iter()
                .any(|value| *value != InternalStackValue::Ordinary);
            if existing != output && (target.catch_handler || carries_internal_value) {
                return Err(internal_join_error(
                    id, target_pc, source_pc, existing, output,
                ));
            }
            return Ok(());
        }
        Some(_) => {}
        None => *component_slot = Some(component),
    }
    let entry = entries
        .get_mut(index)
        .ok_or_else(|| internal_join_error(id, target_pc, source_pc, output, &[]))?;
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
        Some(existing) => {
            let merged = target.iterator_close
                && merge_trailing_for_of_close_record(existing, output).is_some_and(|changed| {
                    if changed && !queued[index] {
                        queued[index] = true;
                        work.push_back(index);
                    }
                    true
                });
            if !merged {
                return Err(internal_join_error(
                    id, target_pc, source_pc, existing, output,
                ));
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "terminal cleanup validates nested finally, catch, for-in, and synchronous for-of marker grammars together"
)]
fn verify_internal_stack_exit(
    id: FunctionTemplateId,
    decoded: crate::DecodedInstruction,
    state: &[InternalStackValue],
    has_finally: bool,
) -> Result<(), BytecodeVerificationError> {
    let is_throw = matches!(
        decoded.instruction().opcode(),
        FinalOpcode::Throw | FinalOpcode::ThrowError
    );
    let mut prefix_len = state.len();
    while matches!(
        state.get(prefix_len.saturating_sub(1)),
        Some(InternalStackValue::FinallyReturn { .. })
    ) {
        let Some(pair_start) = prefix_len.checked_sub(2) else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { pc: decoded.pc() },
            ));
        };
        if !matches!(
            (state[pair_start], state[pair_start + 1]),
            (
                InternalStackValue::FinallyPending {
                    target: pending_target,
                    ..
                },
                InternalStackValue::FinallyReturn {
                    target: return_target
                }
            ) if pending_target == return_target
        ) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { pc: decoded.pc() },
            ));
        }
        prefix_len = pair_start;
    }
    let state = &state[..prefix_len];
    if !is_throw && state.iter().any(|value| value.is_finally_value()) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if is_throw {
        let mut cursor = 0;
        while cursor < state.len() {
            match state[cursor] {
                value if value.is_javascript_value() => cursor += 1,
                InternalStackValue::CatchMarker { .. } => cursor += 1,
                InternalStackValue::FinallyPending { target, .. } => {
                    if !matches!(
                        state.get(cursor.saturating_add(1)),
                        Some(InternalStackValue::FinallyReturn {
                            target: return_target
                        }) if *return_target == target
                    ) {
                        return Err(BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit {
                                pc: decoded.pc(),
                            },
                        ));
                    }
                    cursor += 2;
                }
                InternalStackValue::FinallyReturn { .. } => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit {
                            pc: decoded.pc(),
                        },
                    ));
                }
                InternalStackValue::ForOfIterator(site) => {
                    if !matches!(
                        state.get(cursor..cursor.saturating_add(3)),
                        Some([
                            InternalStackValue::ForOfIterator(iterator),
                            InternalStackValue::ForOfNextMethod(next),
                            InternalStackValue::ForOfCatch(catch),
                        ]) if *iterator == site && *next == site && *catch == site
                    ) {
                        return Err(for_of_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                    cursor += 3;
                }
                value if value.is_for_of_value() => {
                    return Err(for_of_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
                InternalStackValue::ForInIterator(_) => {
                    // A catch or active for-of handler nested inside the
                    // for-in region owns the next unwind step and may retain
                    // the enumeration marker beneath it. An uncaught throw,
                    // or a throw to an outer handler, must instead have removed
                    // every crossed for-in marker before this terminal.
                    let retained_by_inner_handler =
                        state[cursor.saturating_add(1)..].iter().any(|value| {
                            matches!(
                                value,
                                InternalStackValue::CatchMarker { .. }
                                    | InternalStackValue::ForOfCatch(_)
                            )
                        });
                    if !retained_by_inner_handler {
                        return Err(for_in_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                    cursor += 1;
                }
                _ => {
                    return Err(catch_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
            }
        }
        return Ok(());
    }
    if state.iter().any(|value| value.is_for_of_value()) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ForOfIteratorMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if state
        .iter()
        .any(|value| matches!(value, InternalStackValue::CatchMarker { .. }))
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::CatchMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if state
        .iter()
        .any(|value| matches!(value, InternalStackValue::ForInIterator(_)))
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ForInIteratorMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if has_finally && !state.is_empty() {
        return Err(finally_stack_error(
            id,
            decoded.pc(),
            decoded.instruction().opcode(),
        ));
    }
    Ok(())
}

fn internal_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
    state: &[InternalStackValue],
) -> BytecodeVerificationError {
    if opcode == FinalOpcode::Gosub
        || opcode == FinalOpcode::Ret
        || state.iter().any(|value| value.is_finally_value())
    {
        finally_stack_error(id, pc, opcode)
    } else if matches!(
        opcode,
        FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::ForAwaitOfNext
            | FinalOpcode::IteratorGetValueDone
            | FinalOpcode::IteratorClose
            | FinalOpcode::Rot3r
    ) || state.iter().any(|value| value.is_for_of_value())
    {
        for_of_stack_error(id, pc, opcode)
    } else if opcode == FinalOpcode::Catch
        || opcode == FinalOpcode::NipCatch
        || state.iter().any(|value| value.is_catch_value())
    {
        catch_stack_error(id, pc, opcode)
    } else {
        for_in_stack_error(id, pc, opcode)
    }
}

fn finally_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::FinallyReturnStackMismatch { pc, opcode },
    )
}

fn catch_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::CatchMarkerStackMismatch { pc, opcode },
    )
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

fn for_of_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ForOfIteratorStackMismatch { pc, opcode },
    )
}

/// Locates the certified for-of record beneath a `value`/`done` pair about
/// to be branched on.
///
/// The `done` flag must sit at the top with the `value` directly below it.
/// Any number of ordinary JavaScript values may sit between the value and
/// the record: the array-destructuring rest collector keeps its fresh array
/// and cursor there. The three-slot record must share the value's exact
/// `for_of_start` site, and no other certified internal value may intervene.
/// Returns the record start and the shared site.
fn for_of_branch_record(state: &[InternalStackValue]) -> Option<(usize, BytecodePc)> {
    let done_index = state.len().checked_sub(1)?;
    let InternalStackValue::ForOfDone(site) = state[done_index] else {
        return None;
    };
    let value_index = done_index.checked_sub(1)?;
    let InternalStackValue::ForOfValue(value_site) = state[value_index] else {
        return None;
    };
    if value_site != site {
        return None;
    }
    let mut cursor = value_index;
    while cursor > 0 {
        cursor -= 1;
        match state[cursor] {
            InternalStackValue::Ordinary => {}
            InternalStackValue::ForOfCatch(catch) => {
                let iterator_index = cursor.checked_sub(2)?;
                if matches!(
                    (state[iterator_index], state[iterator_index + 1]),
                    (
                        InternalStackValue::ForOfIterator(iterator),
                        InternalStackValue::ForOfNextMethod(next)
                    ) if iterator == next && next == catch && catch == site
                ) {
                    return Some((iterator_index, site));
                }
                return None;
            }
            _ => return None,
        }
    }
    None
}

fn internal_join_error(
    id: FunctionTemplateId,
    target: BytecodePc,
    incoming_from: BytecodePc,
    established: &[InternalStackValue],
    incoming: &[InternalStackValue],
) -> BytecodeVerificationError {
    let kind = if established
        .iter()
        .chain(incoming)
        .any(|value| value.is_finally_value())
    {
        BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
            target,
            incoming_from,
        }
    } else if established
        .iter()
        .chain(incoming)
        .any(|value| value.is_for_of_value())
    {
        BytecodeVerificationErrorKind::ForOfIteratorJoinMismatch {
            target,
            incoming_from,
        }
    } else if established
        .iter()
        .chain(incoming)
        .any(|value| value.is_catch_value())
    {
        BytecodeVerificationErrorKind::CatchMarkerJoinMismatch {
            target,
            incoming_from,
        }
    } else {
        BytecodeVerificationErrorKind::ForInIteratorJoinMismatch {
            target,
            incoming_from,
        }
    };
    BytecodeVerificationError::function(id, kind)
}

#[allow(
    clippy::too_many_lines,
    reason = "binding opcode authority and exact initializer counts are checked in one pass"
)]
fn verify_binding_opcodes(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
    closures: &[ClosureVariableDefinition],
    internal_stack: &InternalStackCertificate,
) -> Result<(), BytecodeVerificationError> {
    let argument_count = flow.domains().argument_count() as usize;
    // A for-in/of loop rotation re-arms the head's non-captured TDZ cells at
    // the loop back edge (the `rotate` label targets exactly that
    // instruction), so a second scope activation is admitted only at a
    // backward jump target; straight-line repeated initialization stays
    // rejected.
    let instructions = flow.instructions();
    let mut back_edge_targets = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    for (index, verified) in instructions.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::VariableDefinitions,
                    limit: u64::from(u32::MAX),
                    observed: u64::from(u32::MAX),
                },
            )
        })?;
        for target in [
            verified.successors().branch_target(),
            verified.successors().jump_target(),
        ] {
            if let Some(target) = target
                && target.get() < index
            {
                back_edge_targets[target.get() as usize] = true;
            }
        }
    }
    // A rotation emits one activation per cell, and the loop label targets
    // only the first, so extend the back-edge set over the contiguous
    // activation run that starts at the target.
    for index in 0..instructions.len().saturating_sub(1) {
        if back_edge_targets[index]
            && instructions[index + 1].decoded().instruction().opcode()
                == FinalOpcode::SetLocUninitialized
        {
            back_edge_targets[index + 1] = true;
        }
    }
    let mut scope_activations = try_filled_vec(
        id,
        variables.len() - argument_count,
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut catch_initializations = try_filled_vec(
        id,
        variables.len() - argument_count,
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    for (index, verified) in flow.instructions().iter().enumerate() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if opcode == FinalOpcode::DeleteVar {
            let Operands::Atom(atom) = instruction.operands() else {
                continue;
            };
            let has_binding = closures.iter().any(|definition| {
                definition.name == Some(atom)
                    && (matches!(
                        definition.binding,
                        CompilerClosureBinding::RealmGlobal(policy)
                            if matches!(
                                policy.kind(),
                                CompilerBindingKind::GlobalReference
                                    | CompilerBindingKind::Var
                                    | CompilerBindingKind::Function
                            )
                    ) || definition.deletable_eval_variable)
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
            if internal_stack.certifies_catch_local_put(index, local)
                && !definition.policy.temporal_dead_zone
            {
                if definition.policy.initialization != CompilerInitializationPolicy::Catch {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(decoded.pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
                let count = &mut catch_initializations[local as usize];
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
            if matches!(opcode, FinalOpcode::SetLocUninitialized) {
                let count = &mut scope_activations[local as usize];
                *count = count.saturating_add(1);
                // The iteration rotation is the single legitimate second
                // activation, and it must sit at the loop back-edge target.
                if *count > 2 || (*count == 2 && !back_edge_targets[index]) {
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
    for (local, ((definition, activations), catch_initializations)) in variables[argument_count..]
        .iter()
        .zip(scope_activations)
        .zip(catch_initializations)
        .enumerate()
    {
        let requires_scope_activation = definition.policy.temporal_dead_zone
            || definition.policy.initialization
                == CompilerInitializationPolicy::FunctionAtScopeEntry;
        // A for-in/of loop rotation adds exactly one back-edge re-arm to the
        // entry activation, so both one and two activations are admitted.
        if requires_scope_activation && !(activations == 1 || activations == 2) {
            return Err(policy_error(
                id,
                BindingSlot::Local(usize_to_u32(local)),
                None,
                BindingPolicyViolationReason::MissingLexicalScopeInitialization,
            ));
        }
        let expected_catch_initializations = u8::from(
            definition.policy.initialization == CompilerInitializationPolicy::Catch
                && !definition.policy.temporal_dead_zone,
        );
        if definition.policy.initialization == CompilerInitializationPolicy::Catch
            && catch_initializations != expected_catch_initializations
        {
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
    let runtime_checked_immutable_write = definition.policy.writes != CompilerWritePolicy::Mutable
        && ((tdz
            && matches!(
                opcode,
                FinalOpcode::PutLocCheck | FinalOpcode::PutLocCheckInit | FinalOpcode::SetLocCheck
            ))
            || (!tdz && definition.policy.kind == CompilerBindingKind::FunctionName));
    if is_local_write(opcode)
        && !matches!(opcode, FinalOpcode::SetLocUninitialized)
        && definition.policy.writes != CompilerWritePolicy::Mutable
        && !(tdz && is_unchecked_local_put(opcode))
        && !runtime_checked_immutable_write
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
            if opcode == FinalOpcode::GetVarUndef && definition.deletable_eval_variable {
                return Ok(());
            }
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
                CompilerBindingKind::Let | CompilerBindingKind::Const => matches!(
                    opcode,
                    FinalOpcode::GetVarUndef
                        | FinalOpcode::GetVar
                        | FinalOpcode::PutVar
                        | FinalOpcode::PutVarInit
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
    // Captured writes retain their declaration policy in the authority. The
    // VM uses it to throw for immutable bindings or ignore a sloppy write to
    // an ImmutableInStrictCode binding; these opcodes never grant an
    // unchecked mutation capability.
    Ok(())
}

const fn is_realm_global_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetVarUndef
            | FinalOpcode::GetVar
            | FinalOpcode::PutVar
            | FinalOpcode::PutVarInit
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
    internal_stack: &InternalStackCertificate,
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
                internal_stack.certifies_iteration_local_put(index, local),
                internal_stack.certifies_catch_local_put(index, local)
                    && !tracked[position].1.policy.temporal_dead_zone,
                &mut state[position],
            )?;
        }
        if !normal_completion_possible {
            continue;
        }
        for edge in internal_stack.effective_successors(instructions, index) {
            let successor = edge.target;
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
        || definition.policy.initialization == CompilerInitializationPolicy::Catch
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
    clippy::too_many_lines,
    reason = "binding identity, declaration policy, initializer authority, and iteration-head authority are checked together"
)]
fn transfer_local_state(
    id: FunctionTemplateId,
    pc: BytecodePc,
    local: u32,
    opcode: FinalOpcode,
    definition: &VariableDefinition,
    is_function_initializer: bool,
    is_iteration_head_put: bool,
    is_catch_initialization: bool,
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
            let valid = if is_catch_initialization {
                definition.policy.initialization == CompilerInitializationPolicy::Catch
                    && BindingState::only(
                        *state,
                        BindingState::INACTIVE | BindingState::INITIALIZED_CLOSED,
                    )
            } else if is_function_initializer {
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
            } else if is_iteration_head_put {
                BindingState::only(
                    *state,
                    BindingState::UNINITIALIZED | BindingState::INITIALIZED_CLOSED,
                )
            } else if definition.policy.kind == CompilerBindingKind::FunctionName {
                BindingState::only(*state, BindingState::INITIALIZED)
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
            *state = if is_catch_initialization
                && definition.has_scope
                && definition.variable_reference.is_some()
            {
                BindingState::INITIALIZED_ACTIVE
            } else {
                BindingState::with_initialized_value(*state)
            };
        }
        FinalOpcode::PutLocCheckInit => {
            if !BindingState::only(*state, BindingState::UNINITIALIZED) {
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
        FinalOpcode::GetLocCheck
            | FinalOpcode::PutLocCheck
            | FinalOpcode::PutLocCheckInit
            | FinalOpcode::SetLocCheck
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
            | FinalOpcode::PutLocCheckInit
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
                crate::CompilerConstant::Value(
                    crate::CompilerConstantValue::String(_)
                        | crate::CompilerConstantValue::TemplateObject(_)
                )
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
    if function.constants().iter().any(|constant| {
        matches!(
            constant,
            crate::CompilerConstant::Value(crate::CompilerConstantValue::BigInt(_))
        )
    }) {
        push_requirement(requirements, ExecutionRequirement::BigInts);
    }
    if function.constants().iter().any(|constant| {
        matches!(
            constant,
            crate::CompilerConstant::Value(crate::CompilerConstantValue::TemplateObject(_))
        )
    }) {
        push_requirement(requirements, ExecutionRequirement::Arrays);
        push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
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
        let instruction = instruction.decoded().instruction();
        match instruction.opcode() {
            FinalOpcode::CallConstructor
            | FinalOpcode::Call
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::CallMethod
            | FinalOpcode::Apply
            | FinalOpcode::Eval
            | FinalOpcode::ApplyEval
            | FinalOpcode::InitCtor
            | FinalOpcode::GetSuper
            | FinalOpcode::GetSuperValue
            | FinalOpcode::PutSuperValue
            | FinalOpcode::PushThis => {
                push_requirement(requirements, ExecutionRequirement::Calls);
            }
            FinalOpcode::ArrayFrom | FinalOpcode::Rest => {
                push_requirement(requirements, ExecutionRequirement::Arrays);
            }
            FinalOpcode::Append => {
                push_requirement(requirements, ExecutionRequirement::Arrays);
                push_requirement(requirements, ExecutionRequirement::Iterators);
            }
            FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::IteratorClose => {
                push_requirement(requirements, ExecutionRequirement::Iterators);
            }
            FinalOpcode::Object
            | FinalOpcode::SetName
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::PutField
            | FinalOpcode::DefineField
            | FinalOpcode::DefineClass
            | FinalOpcode::DefineMethod
            | FinalOpcode::ForInStart => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
            }
            FinalOpcode::WithGetVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef
            | FinalOpcode::PutRefValue => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::Calls);
            }
            FinalOpcode::SpecialObject => match instruction.operands() {
                Operands::U8(3..=6) => {
                    push_requirement(requirements, ExecutionRequirement::Calls);
                }
                Operands::U8(0 | 1) => {
                    push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                }
                _ => unreachable!("verified compiler special-object selector"),
            },
            FinalOpcode::ForInNext => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::Strings);
            }
            FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::PutArrayEl
            | FinalOpcode::ToPropKey
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::DefineMethodComputed
            | FinalOpcode::SetNameComputed => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::DynamicPropertyKeys);
            }
            FinalOpcode::Throw
            | FinalOpcode::Catch
            | FinalOpcode::NipCatch
            | FinalOpcode::Gosub
            | FinalOpcode::Ret => {
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
