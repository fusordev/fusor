use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    sync::Arc,
};

use oxc_ast::{
    AstKind,
    ast::{
        BindingPattern, ClassElement, ExportDefaultDeclarationKind, Expression, FunctionType,
        MethodDefinitionKind, PropertyKind, Statement, VariableDeclarationKind,
    },
};
use oxc_semantic::{AstNodes, NodeId, ReferenceId, ScopeId, SymbolFlags, SymbolId};
use oxc_span::GetSpan;
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, ModuleExportLocalName, ParsedUnit, Span,
};

/// Dense plan-local compiler identity of one executable body.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExecutableId(u32);

impl ExecutableId {
    /// Returns the dense zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Dense plan-local compiler identity of one source binding.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BindingId(u32);

impl BindingId {
    /// Returns the dense zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Dense zero-based capture slot local to one executable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CaptureSlot(u32);

impl CaptureSlot {
    /// Returns the dense zero-based index within the capturing executable.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Dense plan-local compiler identity of one unresolved global reference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnresolvedGlobalId(u32);

impl UnresolvedGlobalId {
    /// Returns the dense zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Dense plan-local compiler identity of one resolved binding reference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResolvedReferenceId(u32);

impl ResolvedReferenceId {
    /// Returns the dense zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Source-unit kind accepted by this storage-planning slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilationUnitKind {
    /// A Script root, including host-loaded global Script and the internal
    /// synchronous dynamic-function wrapper Script.
    Script,
    /// An ECMAScript Module.
    Module,
}

/// Kind of executable body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutableKind {
    /// The Script root body.
    Script {
        /// Whether the host requested asynchronous global Script evaluation,
        /// which admits top-level `await` and produces a Promise when executed.
        asynchronous: bool,
    },
    /// The Module root body.
    Module,
    /// A non-arrow function.
    Function {
        /// Whether the function is asynchronous.
        asynchronous: bool,
        /// Whether the function is a generator.
        generator: bool,
    },
    /// An arrow function.
    Arrow {
        /// Whether the arrow is asynchronous.
        asynchronous: bool,
    },
    /// A compiler-synthesized base-class constructor for a class without a
    /// source-written constructor.
    ClassDefaultConstructor,
}

/// Compiler-owned metadata for one executable body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Executable {
    id: ExecutableId,
    parent: Option<ExecutableId>,
    kind: ExecutableKind,
    span: Span,
    name: Option<Arc<str>>,
    name_span: Option<Span>,
    strict: bool,
    parameter_count: u32,
    defined_parameter_count: u32,
    simple_parameter_list: bool,
    parameter_expressions: bool,
    parameter_binding_indices: Arc<[u32]>,
    mapped_parameter_indices: Arc<[u32]>,
    binding_start: u32,
    binding_end: u32,
    resolved_start: u32,
    resolved_end: u32,
    unresolved_start: u32,
    unresolved_end: u32,
    capture_start: u32,
    capture_end: u32,
}

impl Executable {
    /// Returns this executable's dense identity.
    #[must_use]
    pub const fn id(&self) -> ExecutableId {
        self.id
    }

    /// Returns the nearest enclosing executable.
    #[must_use]
    pub const fn parent(&self) -> Option<ExecutableId> {
        self.parent
    }

    /// Returns this executable's kind.
    #[must_use]
    pub const fn kind(&self) -> ExecutableKind {
        self.kind
    }

    /// Returns the complete source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns the source-written function name, when present.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the source-written function-name span, when present.
    #[must_use]
    pub const fn name_span(&self) -> Option<Span> {
        self.name_span
    }

    /// Returns whether this executable is strict code.
    #[must_use]
    pub const fn is_strict(&self) -> bool {
        self.strict
    }

    /// Returns the number of source parameter positions.
    #[must_use]
    pub const fn parameter_count(&self) -> u32 {
        self.parameter_count
    }

    /// Returns the number of leading formal parameters before the first
    /// top-level initializer. Rest parameters never contribute.
    #[must_use]
    pub const fn defined_parameter_count(&self) -> u32 {
        self.defined_parameter_count
    }

    /// Returns whether every formal parameter is a plain binding identifier.
    #[must_use]
    pub const fn has_simple_parameter_list(&self) -> bool {
        self.simple_parameter_list
    }

    /// Returns whether the formal parameter list contains a default
    /// initializer or computed binding-pattern key.
    #[must_use]
    pub const fn has_parameter_expressions(&self) -> bool {
        self.parameter_expressions
    }

    /// Returns, for each simple formal position, the last position that owns
    /// the source binding for that parameter name. Non-simple lists return an
    /// empty slice because their raw argument positions are not all bindings.
    #[must_use]
    pub fn parameter_binding_indices(&self) -> &[u32] {
        &self.parameter_binding_indices
    }

    /// Returns the last source position for each distinct simple parameter
    /// name, in ascending position order.
    #[must_use]
    pub fn mapped_parameter_indices(&self) -> &[u32] {
        &self.mapped_parameter_indices
    }
}

/// Storage category selected for a source binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoragePlacement {
    /// A named argument position.
    Argument {
        /// Zero-based source parameter position.
        parameter_index: u32,
    },
    /// Executable-local storage, including nested lexical blocks and
    /// evaluation-local dynamic-Function Script lexicals.
    Local,
    /// A property-backed global Script declaration.
    GlobalObject,
    /// A host global Script declarative-environment binding.
    GlobalLexical,
    /// A module-owned declaration cell.
    ModuleLocal,
    /// A named/default live import cell.
    ModuleImport,
}

/// Source declaration category after legal redeclarations are merged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// A formal parameter.
    Parameter,
    /// A `var` declaration.
    Var,
    /// A `let` declaration.
    Let,
    /// A `const` declaration.
    Const,
    /// A class declaration. Classes are mutable lexical bindings with a TDZ.
    Class,
    /// The compiler-created immutable inner binding for a named class.
    ClassName,
    /// The compiler-created immutable cell that retains one evaluated
    /// computed public instance-field key for its class definition.
    ClassFieldKey,
    /// The compiler-created immutable class-scope cell that holds a class
    /// constructor while its static field initializers execute.
    ClassStaticReceiver,
    /// A function declaration.
    Function,
    /// A named function-expression binding.
    FunctionName,
    /// A catch parameter.
    Catch,
    /// A named or default import.
    Import,
    /// A namespace import.
    NamespaceImport,
    /// `QuickJS`'s internal module `*default*` cell.
    SyntheticDefault,
}

/// When a binding receives its initial value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationPolicy {
    /// Copied from a source argument position.
    Argument,
    /// Initialized to `undefined` during executable instantiation.
    UndefinedAtInstantiation,
    /// Initialized when its declaration executes.
    AtDeclaration,
    /// Initialized with a function during executable instantiation.
    FunctionAtInstantiation,
    /// Initialized with a function when a lexical block is entered.
    FunctionAtScopeEntry,
    /// Initialized when a named function object is created.
    FunctionName,
    /// Initialized on catch-clause entry.
    Catch,
    /// Connected to another module's named/default export during linking.
    ModuleImport,
    /// Initialized with a requested module namespace during linking.
    ModuleNamespace,
}

/// Assignment behavior after initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritePolicy {
    /// Ordinary writes are allowed.
    Mutable,
    /// Every source write is rejected.
    Immutable,
    /// Sloppy writes are ignored and strict writes are rejected.
    ImmutableInStrictCode,
    /// The cell is compiler-internal and cannot be named by source code.
    Internal,
}

/// Declaration policy required by later bytecode lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationPolicy {
    kind: DeclarationKind,
    initialization: InitializationPolicy,
    writes: WritePolicy,
    temporal_dead_zone: bool,
}

impl DeclarationPolicy {
    /// Returns the effective declaration category.
    #[must_use]
    pub const fn kind(self) -> DeclarationKind {
        self.kind
    }

    /// Returns when the binding receives its initial value.
    #[must_use]
    pub const fn initialization(self) -> InitializationPolicy {
        self.initialization
    }

    /// Returns the binding's post-initialization write behavior.
    #[must_use]
    pub const fn writes(self) -> WritePolicy {
        self.writes
    }

    /// Returns whether reads require an uninitialized-binding check.
    #[must_use]
    pub const fn has_temporal_dead_zone(self) -> bool {
        self.temporal_dead_zone
    }
}

/// One arena-independent binding-storage decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingStorage {
    id: BindingId,
    executable: ExecutableId,
    name: Arc<str>,
    declaration_spans: Arc<[Span]>,
    placement: StoragePlacement,
    policy: DeclarationPolicy,
    frame_captured: bool,
    arguments_object: bool,
}

impl BindingStorage {
    /// Returns this binding's dense identity.
    #[must_use]
    pub const fn id(&self) -> BindingId {
        self.id
    }

    /// Returns the executable that owns the storage.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Returns the exact binding name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns every source declaration span merged into this binding.
    #[must_use]
    pub fn declaration_spans(&self) -> &[Span] {
        &self.declaration_spans
    }

    /// Returns the selected storage category.
    #[must_use]
    pub const fn placement(&self) -> StoragePlacement {
        self.placement
    }

    /// Returns initialization, write, and TDZ policy.
    #[must_use]
    pub const fn policy(&self) -> DeclarationPolicy {
        self.policy
    }

    /// Returns whether a descendant executable captures this frame binding.
    ///
    /// Only argument and local placements can be frame-captured. Global and
    /// module cells remain reachable through their own storage domains.
    #[must_use]
    pub const fn is_frame_captured(&self) -> bool {
        self.frame_captured
    }

    /// Returns whether this is the compiler-synthesized `arguments` binding.
    #[must_use]
    pub const fn is_arguments_object(&self) -> bool {
        self.arguments_object
    }
}

/// Read/write role of an unresolved global reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceAccess {
    read: bool,
    write: bool,
}

impl ReferenceAccess {
    /// Returns whether the reference reads its resolved value.
    #[must_use]
    pub const fn reads(self) -> bool {
        self.read
    }

    /// Returns whether the reference writes its resolved value.
    #[must_use]
    pub const fn writes(self) -> bool {
        self.write
    }
}

/// One source reference that remained unresolved after Oxc semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedGlobal {
    id: UnresolvedGlobalId,
    executable: ExecutableId,
    name: Arc<str>,
    span: Span,
    access: ReferenceAccess,
}

impl UnresolvedGlobal {
    /// Returns this reference's dense identity.
    #[must_use]
    pub const fn id(&self) -> UnresolvedGlobalId {
        self.id
    }

    /// Returns the executable containing the reference.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Returns the exact identifier text.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact identifier span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns whether the source reads and/or writes the reference.
    #[must_use]
    pub const fn access(&self) -> ReferenceAccess {
        self.access
    }
}

/// One source reference resolved to a compiler-owned binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedReference {
    id: ResolvedReferenceId,
    executable: ExecutableId,
    binding: BindingId,
    span: Span,
    access: ReferenceAccess,
}

impl ResolvedReference {
    /// Returns this reference's dense identity.
    #[must_use]
    pub const fn id(&self) -> ResolvedReferenceId {
        self.id
    }

    /// Returns the executable containing the reference.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Returns the compiler-owned binding targeted by this reference.
    #[must_use]
    pub const fn binding(&self) -> BindingId {
        self.binding
    }

    /// Returns the exact identifier span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns whether the source reads and/or writes the binding.
    #[must_use]
    pub const fn access(&self) -> ReferenceAccess {
        self.access
    }
}

/// Where one executable obtains the value cell for a capture slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureSource {
    /// The immediate parent owns the argument or local binding.
    ParentBinding(BindingId),
    /// The immediate parent forwards one of its own capture slots.
    ParentCapture(CaptureSlot),
}

/// One binding cell captured or forwarded by an executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCapture {
    executable: ExecutableId,
    binding: BindingId,
    slot: CaptureSlot,
    source: CaptureSource,
}

impl FrameCapture {
    /// Returns the executable that owns this capture slot.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Returns the original frame binding represented by the slot.
    #[must_use]
    pub const fn binding(&self) -> BindingId {
        self.binding
    }

    /// Returns the dense slot local to the capturing executable.
    #[must_use]
    pub const fn slot(&self) -> CaptureSlot {
        self.slot
    }

    /// Returns whether the immediate parent owns or forwards the cell.
    #[must_use]
    pub const fn source(&self) -> CaptureSource {
        self.source
    }
}

/// Fully owned storage metadata for one accepted source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoragePlan {
    kind: CompilationUnitKind,
    executables: Arc<[Executable]>,
    bindings: Arc<[BindingStorage]>,
    resolved_references: Arc<[ResolvedReference]>,
    unresolved_globals: Arc<[UnresolvedGlobal]>,
    frame_captures: Arc<[FrameCapture]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeReferenceId {
    Resolved(ResolvedReferenceId),
    Unresolved(UnresolvedGlobalId),
}

pub(crate) struct OxcIdentityMap {
    pub(crate) executable_by_node: Box<[Option<ExecutableId>]>,
    pub(crate) node_by_executable: Box<[NodeId]>,
    pub(crate) binding_by_symbol: Box<[Option<BindingId>]>,
    pub(crate) binding_by_declaration: HashMap<(SymbolId, u32, u32), BindingId>,
    /// Synthesized default constructor template for each eligible named base
    /// class that lacks a source-written constructor.
    pub(crate) default_class_constructors: HashMap<NodeId, ExecutableId>,
    /// The synthetic immutable class-name cell for each named class.  Oxc
    /// resolves member references to the declaration symbol, but ECMAScript
    /// gives a class body a distinct inner binding; storage redirects those
    /// references before capture planning.
    pub(crate) class_name_bindings: HashMap<NodeId, BindingId>,
    /// The immutable class-scope key cell for each computed public instance
    /// field. Constructors capture these cells so field construction never
    /// re-evaluates an observable key expression.
    pub(crate) class_field_key_bindings: HashMap<NodeId, BindingId>,
    /// The immutable class-scope receiver cell for each class whose static
    /// field initializer lexically observes `this`.
    pub(crate) class_static_receiver_bindings: HashMap<NodeId, BindingId>,
    pub(crate) scope_by_binding: Box<[Option<ScopeId>]>,
    pub(crate) reference_by_id: Box<[Option<NativeReferenceId>]>,
}

pub(crate) struct PlannedStorage {
    pub(crate) plan: Arc<StoragePlan>,
    pub(crate) identities: OxcIdentityMap,
}

impl StoragePlan {
    /// Returns whether the root is a Script or Module.
    #[must_use]
    pub const fn kind(&self) -> CompilationUnitKind {
        self.kind
    }

    /// Returns executables in stable dense preorder.
    #[must_use]
    pub fn executables(&self) -> &[Executable] {
        &self.executables
    }

    /// Resolves a plan-local executable identity, returning `None` when its
    /// dense index is out of range.
    #[must_use]
    pub fn executable(&self, id: ExecutableId) -> Option<&Executable> {
        self.executables.get(id.index())
    }

    /// Returns all source bindings grouped by owning executable.
    #[must_use]
    pub fn bindings(&self) -> &[BindingStorage] {
        &self.bindings
    }

    /// Resolves a plan-local binding identity, returning `None` when its dense
    /// index is out of range.
    #[must_use]
    pub fn binding(&self, id: BindingId) -> Option<&BindingStorage> {
        self.bindings.get(id.index())
    }

    /// Returns bindings owned by one executable, or `None` if its dense index
    /// is out of range.
    ///
    /// Bindings are ordered by source declaration span within each executable,
    /// with placement and name as deterministic tie-breakers.
    #[must_use]
    pub fn bindings_for(&self, executable: ExecutableId) -> Option<&[BindingStorage]> {
        let executable = self.executables.get(executable.index())?;
        self.bindings
            .get(executable.binding_start as usize..executable.binding_end as usize)
    }

    /// Returns all resolved source references grouped by using executable.
    #[must_use]
    pub fn resolved_references(&self) -> &[ResolvedReference] {
        &self.resolved_references
    }

    /// Resolves a plan-local reference identity, returning `None` when its dense
    /// index is out of range.
    #[must_use]
    pub fn resolved_reference(&self, id: ResolvedReferenceId) -> Option<&ResolvedReference> {
        self.resolved_references.get(id.index())
    }

    /// Returns resolved references used by one executable, or `None` if its
    /// dense index is out of range.
    ///
    /// References are in source-span order within each executable.
    #[must_use]
    pub fn resolved_references_for(
        &self,
        executable: ExecutableId,
    ) -> Option<&[ResolvedReference]> {
        let executable = self.executables.get(executable.index())?;
        self.resolved_references
            .get(executable.resolved_start as usize..executable.resolved_end as usize)
    }

    /// Returns all unresolved global references grouped by owning executable.
    #[must_use]
    pub fn unresolved_globals(&self) -> &[UnresolvedGlobal] {
        &self.unresolved_globals
    }

    /// Returns unresolved globals used by one executable, or `None` if its
    /// dense index is out of range.
    ///
    /// References are in source-span order within each executable.
    #[must_use]
    pub fn unresolved_globals_for(&self, executable: ExecutableId) -> Option<&[UnresolvedGlobal]> {
        let executable = self.executables.get(executable.index())?;
        self.unresolved_globals
            .get(executable.unresolved_start as usize..executable.unresolved_end as usize)
    }

    /// Returns every frame capture grouped by capturing executable.
    #[must_use]
    pub fn frame_captures(&self) -> &[FrameCapture] {
        &self.frame_captures
    }

    /// Returns capture slots owned by one executable, or `None` if its dense
    /// index is out of range.
    ///
    /// Slots are dense and ordered by the original binding identity.
    #[must_use]
    pub fn frame_captures_for(&self, executable: ExecutableId) -> Option<&[FrameCapture]> {
        let executable = self.executables.get(executable.index())?;
        self.frame_captures
            .get(executable.capture_start as usize..executable.capture_end as usize)
    }
}

/// Semantic cases intentionally rejected by this first complete slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedFeature {
    /// Any eval compilation goal.
    EvalCompilationGoal,
    /// An unsupported dynamic-function constructor family.
    DynamicFunctionKind(DynamicFunctionKind),
    /// A syntactic bare `eval(...)` call.
    DirectEval,
    /// A `with` statement.
    WithStatement,
    /// Annex B's paired block-lexical and var-like function binding.
    AnnexBBlockFunction,
    /// An anonymous `export default class` needs the module execution layer's
    /// synthetic default binding and class environment.
    AnonymousDefaultClassExport,
    /// A synthesized `this`, `new.target`, or `super` binding.
    FunctionSyntheticBinding,
}

/// Storage-planning failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerError {
    /// The accepted AST requires semantics outside this total slice.
    Unsupported {
        /// The unsupported semantic case.
        feature: UnsupportedFeature,
        /// Exact source span that requires it.
        span: Span,
    },
    /// Oxc's retained semantic graph violated an expected post-build invariant.
    SemanticInvariant {
        /// Stable invariant label.
        invariant: &'static str,
        /// Related source span, when available.
        span: Option<Span>,
    },
    /// A dense compiler-owned domain exceeded `u32`.
    CapacityExceeded {
        /// Stable capacity-domain label.
        domain: &'static str,
    },
}

impl fmt::Display for CompilerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { feature, span } => {
                write!(
                    formatter,
                    "unsupported compiler feature {feature:?} at {span:?}"
                )
            }
            Self::SemanticInvariant { invariant, span } => {
                write!(formatter, "semantic invariant `{invariant}` failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                Ok(())
            }
            Self::CapacityExceeded { domain } => {
                write!(formatter, "compiler capacity exceeded for {domain}")
            }
        }
    }
}

impl Error for CompilerError {}

/// Builds an arena-independent binding-storage plan.
///
/// # Errors
///
/// Returns a typed error for semantic cases not yet modeled by this total
/// slice or if the retained Oxc model violates a required invariant.
pub fn build_storage_plan(unit: &ParsedUnit<'_, '_>) -> Result<StoragePlan, CompilerError> {
    build_planned_storage(unit).map(|planned| (*planned.plan).clone())
}

pub(crate) fn build_planned_storage(
    unit: &ParsedUnit<'_, '_>,
) -> Result<PlannedStorage, CompilerError> {
    Planner::new(unit)?.build()
}

#[derive(Clone, Copy)]
struct ParameterStorage {
    executable: ExecutableId,
    parameter_index: u32,
}

#[derive(Default)]
struct ParameterLayout {
    count: u32,
    defined_count: u32,
    simple: bool,
    expressions: bool,
    binding_indices: Arc<[u32]>,
    mapped_indices: Arc<[u32]>,
}

struct ExecutableDraft {
    executable: Executable,
    node_id: NodeId,
    scope_id: ScopeId,
}

struct BindingDraft {
    symbol_id: Option<SymbolId>,
    primary_symbol_binding: bool,
    class_node: Option<NodeId>,
    class_field_node: Option<NodeId>,
    class_static_receiver_node: Option<NodeId>,
    executable: ExecutableId,
    name: Arc<str>,
    declaration_spans: Arc<[Span]>,
    placement: StoragePlacement,
    policy: DeclarationPolicy,
    arguments_object: bool,
}

struct FrozenBindings {
    bindings: Vec<BindingStorage>,
    primary_by_symbol: Vec<Option<BindingId>>,
    source_symbols: Vec<Option<SymbolId>>,
    by_declaration: HashMap<(SymbolId, u32, u32), BindingId>,
    class_name_bindings: HashMap<NodeId, BindingId>,
    class_field_key_bindings: HashMap<NodeId, BindingId>,
    class_static_receiver_bindings: HashMap<NodeId, BindingId>,
}

#[derive(Default)]
struct ImplicitArgumentsPlan {
    reference_owners: HashMap<ReferenceId, ExecutableId>,
    first_references: HashMap<ExecutableId, Span>,
}

struct ResolvedDraft {
    reference_id: ReferenceId,
    executable: ExecutableId,
    binding: BindingId,
    span: Span,
    access: ReferenceAccess,
}

#[derive(Clone, Copy)]
struct CaptureRequest {
    executable: ExecutableId,
    binding: BindingId,
    span: Span,
}

struct UnresolvedDraft {
    reference_id: ReferenceId,
    executable: ExecutableId,
    name: Arc<str>,
    span: Span,
    access: ReferenceAccess,
}

#[derive(Default)]
struct DeclarationFacts {
    bits: u16,
    function_scope_entry: bool,
}

impl DeclarationFacts {
    const PARAMETER: u16 = 1 << 0;
    const VAR: u16 = 1 << 1;
    const LET: u16 = 1 << 2;
    const CONST: u16 = 1 << 3;
    const FUNCTION: u16 = 1 << 4;
    const FUNCTION_NAME: u16 = 1 << 5;
    const CATCH: u16 = 1 << 6;
    const IMPORT: u16 = 1 << 7;
    const NAMESPACE_IMPORT: u16 = 1 << 8;
    const CLASS: u16 = 1 << 9;

    fn insert(&mut self, fact: u16) {
        self.bits |= fact;
    }

    const fn contains(&self, fact: u16) -> bool {
        self.bits & fact != 0
    }

    fn effective_kind(&self) -> Option<DeclarationKind> {
        if self.contains(Self::FUNCTION_NAME) {
            Some(DeclarationKind::FunctionName)
        } else if self.contains(Self::NAMESPACE_IMPORT) {
            Some(DeclarationKind::NamespaceImport)
        } else if self.contains(Self::IMPORT) {
            Some(DeclarationKind::Import)
        } else if self.contains(Self::CATCH) {
            Some(DeclarationKind::Catch)
        } else if self.contains(Self::FUNCTION) {
            Some(DeclarationKind::Function)
        } else if self.contains(Self::CLASS) {
            Some(DeclarationKind::Class)
        } else if self.contains(Self::CONST) {
            Some(DeclarationKind::Const)
        } else if self.contains(Self::LET) {
            Some(DeclarationKind::Let)
        } else if self.contains(Self::PARAMETER) {
            Some(DeclarationKind::Parameter)
        } else if self.contains(Self::VAR) {
            Some(DeclarationKind::Var)
        } else {
            None
        }
    }
}

struct Planner<'unit, 'arena, 'scope> {
    unit: &'unit ParsedUnit<'arena, 'scope>,
    kind: CompilationUnitKind,
    root_executable_kind: ExecutableKind,
    root_span: Span,
    executable_drafts: Vec<ExecutableDraft>,
    node_executables: Vec<Option<ExecutableId>>,
    exact_scope_executables: Vec<Option<ExecutableId>>,
    scope_executables: Vec<Option<ExecutableId>>,
    parameter_storage: HashMap<SymbolId, ParameterStorage>,
    default_class_constructors: HashMap<NodeId, ExecutableId>,
}

fn is_derived_class_constructor(nodes: &AstNodes<'_>, function_node: NodeId) -> bool {
    let AstKind::Function(function) = nodes.kind(function_node) else {
        return false;
    };
    let AstKind::MethodDefinition(method) = nodes.parent_kind(function.node_id.get()) else {
        return false;
    };
    if method.kind != MethodDefinitionKind::Constructor
        || method.value.node_id.get() != function_node
    {
        return false;
    }
    let AstKind::ClassBody(body) = nodes.parent_kind(method.node_id.get()) else {
        return false;
    };
    let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
        return false;
    };
    class.super_class.is_some()
}

fn is_home_object_method(nodes: &AstNodes<'_>, function_node: NodeId) -> bool {
    let AstKind::Function(function) = nodes.kind(function_node) else {
        return false;
    };
    match nodes.parent_kind(function.node_id.get()) {
        AstKind::MethodDefinition(method) => method.value.node_id.get() == function_node,
        AstKind::ObjectProperty(property) => {
            matches!(
                &property.value,
                Expression::FunctionExpression(value) if value.node_id.get() == function_node
            ) && (property.method || property.kind != PropertyKind::Init)
        }
        _ => false,
    }
}

impl<'unit, 'arena, 'scope> Planner<'unit, 'arena, 'scope> {
    fn new(unit: &'unit ParsedUnit<'arena, 'scope>) -> Result<Self, CompilerError> {
        let root_span = unit.program().span;
        let (kind, root_executable_kind) = match unit.goal() {
            CompilationGoal::GlobalScript(goal) => (
                CompilationUnitKind::Script,
                ExecutableKind::Script {
                    asynchronous: goal.allows_top_level_await(),
                },
            ),
            CompilationGoal::Module => (CompilationUnitKind::Module, ExecutableKind::Module),
            CompilationGoal::IndirectEval(_) | CompilationGoal::DirectEval(_) => {
                return Err(CompilerError::Unsupported {
                    feature: UnsupportedFeature::EvalCompilationGoal,
                    span: root_span,
                });
            }
            CompilationGoal::DynamicFunction(
                DynamicFunctionKind::Function
                | DynamicFunctionKind::GeneratorFunction
                | DynamicFunctionKind::AsyncFunction
                | DynamicFunctionKind::AsyncGeneratorFunction,
            ) => (
                CompilationUnitKind::Script,
                ExecutableKind::Script {
                    asynchronous: false,
                },
            ),
        };
        let semantic = unit.semantic();
        Ok(Self {
            unit,
            kind,
            root_executable_kind,
            root_span,
            executable_drafts: Vec::new(),
            node_executables: vec![None; semantic.nodes().len()],
            exact_scope_executables: vec![None; semantic.scoping().scopes_len()],
            scope_executables: vec![None; semantic.scoping().scopes_len()],
            parameter_storage: HashMap::new(),
            default_class_constructors: HashMap::new(),
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "planning freezes bindings, references, captures, and their identity maps together"
    )]
    fn build(mut self) -> Result<PlannedStorage, CompilerError> {
        self.reject_preflight_features()?;
        self.inventory_executables()?;
        self.assign_scope_owners()?;
        self.reject_synthetic_binding_uses()?;

        let mut binding_drafts = self.binding_drafts()?;
        let implicit_arguments_references = self.add_arguments_bindings(&mut binding_drafts)?;
        self.add_synthetic_default_binding(&mut binding_drafts)?;
        self.add_class_name_bindings(&mut binding_drafts)?;
        self.add_class_field_key_bindings(&mut binding_drafts)?;
        self.add_class_static_receiver_bindings(&mut binding_drafts)?;
        binding_drafts.sort_by_key(|binding| {
            let first = binding
                .declaration_spans
                .first()
                .copied()
                .unwrap_or(Span::new(u32::MAX, u32::MAX));
            (
                binding.executable.index(),
                first.start,
                first.end,
                placement_order(binding.placement),
                binding.name.clone(),
            )
        });
        let FrozenBindings {
            mut bindings,
            primary_by_symbol: symbol_bindings,
            source_symbols,
            by_declaration: declaration_bindings,
            class_name_bindings,
            class_field_key_bindings,
            class_static_receiver_bindings,
        } = self.freeze_binding_drafts(binding_drafts)?;
        let scope_by_binding = self.binding_scope_map(
            &symbol_bindings,
            &source_symbols,
            &class_name_bindings,
            &class_field_key_bindings,
            &class_static_receiver_bindings,
            &bindings,
        )?;

        let mut resolved_drafts = self.resolved_drafts(
            &symbol_bindings,
            &source_symbols,
            &class_name_bindings,
            &bindings,
            &implicit_arguments_references,
        )?;
        let unresolved_drafts = self.unresolved_drafts()?;
        let (arguments_references, mut unresolved_drafts) = Self::resolve_arguments_references(
            unresolved_drafts,
            &bindings,
            &implicit_arguments_references,
        )?;
        resolved_drafts.extend(arguments_references);
        resolved_drafts.sort_by_key(|reference| {
            (
                reference.executable.index(),
                reference.span.start,
                reference.span.end,
                reference.binding.index(),
                reference.reference_id.index(),
            )
        });
        let class_field_key_captures =
            self.class_field_key_capture_requests(&class_field_key_bindings, &bindings)?;
        let class_static_receiver_captures = self
            .class_static_receiver_capture_requests(&class_static_receiver_bindings, &bindings)?;
        let mut synthetic_captures = class_field_key_captures;
        synthetic_captures.extend(class_static_receiver_captures);
        synthetic_captures.sort_unstable_by_key(|request| {
            (
                request.executable.index(),
                request.binding.index(),
                request.span.start,
                request.span.end,
            )
        });
        let frame_captures = plan_frame_captures(
            &self.executable_drafts,
            &mut bindings,
            &resolved_drafts,
            &synthetic_captures,
        )?;

        unresolved_drafts.sort_by_key(|reference| {
            (
                reference.executable.index(),
                reference.span.start,
                reference.span.end,
                reference.name.clone(),
                reference.reference_id.index(),
            )
        });
        let mut reference_by_id = vec![None; self.unit.semantic().scoping().references_len()];
        let resolved_references = freeze_resolved(resolved_drafts, &mut reference_by_id)?;
        let unresolved_globals = freeze_unresolved(unresolved_drafts, &mut reference_by_id)?;
        if reference_by_id.iter().any(Option::is_none) {
            return Err(CompilerError::SemanticInvariant {
                invariant: "every semantic reference has compiler identity",
                span: None,
            });
        }

        let node_by_executable = self
            .executable_drafts
            .iter()
            .map(|draft| draft.node_id)
            .collect::<Vec<_>>();
        let mut executables = self
            .executable_drafts
            .into_iter()
            .map(|draft| draft.executable)
            .collect::<Vec<_>>();
        assign_ranges(
            &mut executables,
            &bindings,
            &resolved_references,
            &unresolved_globals,
            &frame_captures,
        )?;

        let plan = StoragePlan {
            kind: self.kind,
            executables: executables.into(),
            bindings: bindings.into(),
            resolved_references: resolved_references.into(),
            unresolved_globals: unresolved_globals.into(),
            frame_captures: frame_captures.into(),
        };
        Ok(PlannedStorage {
            plan: Arc::new(plan),
            identities: OxcIdentityMap {
                executable_by_node: self.node_executables.into_boxed_slice(),
                node_by_executable: node_by_executable.into_boxed_slice(),
                binding_by_symbol: symbol_bindings.into_boxed_slice(),
                binding_by_declaration: declaration_bindings,
                default_class_constructors: self.default_class_constructors,
                class_name_bindings,
                class_field_key_bindings,
                class_static_receiver_bindings,
                scope_by_binding: scope_by_binding.into_boxed_slice(),
                reference_by_id: reference_by_id.into_boxed_slice(),
            },
        })
    }

    fn freeze_binding_drafts(
        &self,
        drafts: Vec<BindingDraft>,
    ) -> Result<FrozenBindings, CompilerError> {
        freeze_bindings(drafts, self.unit.semantic().scoping().symbols_len())
    }

    fn binding_scope_map(
        &self,
        symbol_bindings: &[Option<BindingId>],
        source_symbols: &[Option<SymbolId>],
        class_name_bindings: &HashMap<NodeId, BindingId>,
        class_field_key_bindings: &HashMap<NodeId, BindingId>,
        class_static_receiver_bindings: &HashMap<NodeId, BindingId>,
        bindings: &[BindingStorage],
    ) -> Result<Vec<Option<ScopeId>>, CompilerError> {
        let scoping = self.unit.semantic().scoping();
        let mut scopes = reverse_binding_scopes(
            symbol_bindings,
            bindings.len(),
            scoping.scopes_len(),
            scoping.symbol_ids().map(|symbol| {
                (
                    symbol,
                    scoping.symbol_scope_id(symbol),
                    scoping.symbol_span(symbol),
                )
            }),
        )?;
        for (binding, source_symbol) in source_symbols.iter().copied().enumerate() {
            let Some(symbol) = source_symbol else {
                continue;
            };
            let scope = scoping.symbol_scope_id(symbol);
            let span = scoping.symbol_span(symbol);
            let target = scopes
                .get_mut(binding)
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "source-backed compiler binding scope index is in range",
                    span: Some(span),
                })?;
            match *target {
                None => *target = Some(scope),
                Some(existing) if existing == scope => {}
                Some(_) => {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: "split compiler bindings share their semantic scope",
                        span: Some(span),
                    });
                }
            }
        }
        for binding in bindings.iter().filter(|binding| binding.arguments_object) {
            let scope = self
                .executable_drafts
                .get(binding.executable.index())
                .map(|draft| draft.scope_id)
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "arguments binding executable has a scope",
                    span: binding.declaration_spans.first().copied(),
                })?;
            let target =
                scopes
                    .get_mut(binding.id.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "arguments binding scope index is in range",
                        span: binding.declaration_spans.first().copied(),
                    })?;
            match *target {
                None => *target = Some(scope),
                Some(existing) if existing == scope => {}
                Some(_) => {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: "arguments binding has its function compiler scope",
                        span: binding.declaration_spans.first().copied(),
                    });
                }
            }
        }
        for (&node_id, &binding) in class_name_bindings {
            let AstKind::Class(class) = self.unit.semantic().nodes().kind(node_id) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "synthetic class-name binding belongs to a class node",
                    span: None,
                });
            };
            let scope = class.scope_id();
            let target =
                scopes
                    .get_mut(binding.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "class-name binding scope index is in range",
                        span: Some(class.span),
                    })?;
            if target.replace(scope).is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "synthetic class-name binding has one class scope",
                    span: Some(class.span),
                });
            }
        }
        self.bind_class_field_key_scopes(&mut scopes, class_field_key_bindings)?;
        self.bind_class_static_receiver_scopes(&mut scopes, class_static_receiver_bindings)?;
        Ok(scopes)
    }

    fn bind_class_field_key_scopes(
        &self,
        scopes: &mut [Option<ScopeId>],
        class_field_key_bindings: &HashMap<NodeId, BindingId>,
    ) -> Result<(), CompilerError> {
        for (&node_id, &binding) in class_field_key_bindings {
            let AstKind::PropertyDefinition(field) = self.unit.semantic().nodes().kind(node_id)
            else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "synthetic class-field key binding belongs to a property definition",
                    span: None,
                });
            };
            if field.r#static || !field.computed {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "synthetic class-field key binding belongs to a computed instance field",
                    span: Some(field.span),
                });
            }
            let nodes = self.unit.semantic().nodes();
            let AstKind::ClassBody(body) = nodes.parent_kind(node_id) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "computed instance field belongs to a class body",
                    span: Some(field.span),
                });
            };
            let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "computed instance field class body belongs to a class",
                    span: Some(body.span),
                });
            };
            let target =
                scopes
                    .get_mut(binding.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "class-field key binding scope index is in range",
                        span: Some(field.span),
                    })?;
            if target.replace(class.scope_id()).is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "synthetic class-field key binding has one class scope",
                    span: Some(field.span),
                });
            }
        }
        Ok(())
    }

    fn bind_class_static_receiver_scopes(
        &self,
        scopes: &mut [Option<ScopeId>],
        class_static_receiver_bindings: &HashMap<NodeId, BindingId>,
    ) -> Result<(), CompilerError> {
        for (&node_id, &binding) in class_static_receiver_bindings {
            let AstKind::Class(class) = self.unit.semantic().nodes().kind(node_id) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class static-receiver binding belongs to a class node",
                    span: None,
                });
            };
            let target =
                scopes
                    .get_mut(binding.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "class static-receiver binding scope index is in range",
                        span: Some(class.span),
                    })?;
            if target.replace(class.scope_id()).is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class static-receiver binding has one class scope",
                    span: Some(class.span),
                });
            }
        }
        Ok(())
    }

    fn reject_preflight_features(&self) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        let nodes = semantic.nodes();
        for (node_id, node) in nodes.iter_enumerated() {
            match node.kind() {
                AstKind::CallExpression(call)
                    if !call.optional && call.callee.is_specific_id("eval") =>
                {
                    return unsupported(UnsupportedFeature::DirectEval, call.span);
                }
                AstKind::WithStatement(statement) => {
                    return unsupported(UnsupportedFeature::WithStatement, statement.span);
                }
                AstKind::Function(function)
                    if function.r#type == FunctionType::FunctionDeclaration
                        && !function.r#async
                        && !function.generator =>
                {
                    let declaration_scope = node.scope_id();
                    let flags = semantic.scoping().scope_flags(declaration_scope);
                    let single_statement_parent =
                        is_single_statement_parent(nodes.parent_kind(node_id));
                    if single_statement_parent || (!flags.is_var() && !flags.is_strict_mode()) {
                        return unsupported(UnsupportedFeature::AnnexBBlockFunction, function.span);
                    }
                }
                _ => {}
            }
        }

        if semantic
            .scoping()
            .scope_descendants_from_root()
            .any(|scope_id| {
                semantic
                    .scoping()
                    .scope_flags(scope_id)
                    .contains_direct_eval()
            })
        {
            return unsupported(UnsupportedFeature::DirectEval, self.root_span);
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "executable inventory keeps AST form, scope identity, and complete parameter metadata in one checked pass"
    )]
    fn inventory_executables(&mut self) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        let nodes = semantic.nodes();
        for (node_id, node) in nodes.iter_enumerated() {
            if let AstKind::Class(class) = node.kind()
                && class.decorators.is_empty()
                && let Some(constructor) =
                    class.body.body.iter().find_map(|element| match element {
                        ClassElement::MethodDefinition(method)
                            if method.kind == MethodDefinitionKind::Constructor =>
                        {
                            Some(method)
                        }
                        _ => None,
                    })
            {
                // Class elements are visited in source order, so a field initializer can
                // precede a written constructor. Reserve the constructor executable at
                // the class boundary: field-created closures must close over the
                // constructor environment, not the surrounding source function.
                self.inventory_source_function(
                    constructor.value.node_id.get(),
                    &constructor.value,
                )?;
            }

            match node.kind() {
                AstKind::Program(program) => self.inventory_executable(
                    node_id,
                    self.root_executable_kind,
                    program.scope_id(),
                    program.span,
                    None,
                    None,
                    ParameterLayout::default(),
                    true,
                )?,
                AstKind::Function(function) => {
                    if self.node_executables[node_id.index()].is_none() {
                        self.inventory_source_function(node_id, function)?;
                    }
                }
                AstKind::ArrowFunctionExpression(arrow) => {
                    let parameters = self.validate_parameters(arrow.params.as_ref())?;
                    self.inventory_executable(
                        node_id,
                        ExecutableKind::Arrow {
                            asynchronous: arrow.r#async,
                        },
                        arrow.scope_id(),
                        arrow.span,
                        None,
                        None,
                        parameters,
                        true,
                    )?;
                }
                AstKind::Class(class)
                    if class.decorators.is_empty()
                        && !class.body.body.iter().any(|element| {
                            matches!(
                                element,
                                ClassElement::MethodDefinition(method)
                                    if method.kind == MethodDefinitionKind::Constructor
                            )
                        }) =>
                {
                    self.inventory_executable(
                        node_id,
                        ExecutableKind::ClassDefaultConstructor,
                        class.scope_id(),
                        class.span,
                        None,
                        None,
                        ParameterLayout {
                            simple: true,
                            ..ParameterLayout::default()
                        },
                        false,
                    )?;
                }
                _ => {}
            }
        }

        self.validate_root_executable()
    }

    fn inventory_source_function(
        &mut self,
        node_id: NodeId,
        function: &oxc_ast::ast::Function<'arena>,
    ) -> Result<(), CompilerError> {
        let parameters = self.validate_parameters(function.params.as_ref())?;
        self.inventory_executable(
            node_id,
            ExecutableKind::Function {
                asynchronous: function.r#async,
                generator: function.generator,
            },
            function.scope_id(),
            function.span,
            function
                .id
                .as_ref()
                .map(|identifier| Arc::<str>::from(identifier.name.as_str())),
            function.id.as_ref().map(|identifier| identifier.span),
            parameters,
            true,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an executable inventory record carries all validated source metadata"
    )]
    fn inventory_executable(
        &mut self,
        node_id: NodeId,
        kind: ExecutableKind,
        scope_id: ScopeId,
        span: Span,
        name: Option<Arc<str>>,
        name_span: Option<Span>,
        parameters: ParameterLayout,
        source_executable: bool,
    ) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        let nodes = semantic.nodes();
        let id = executable_id(self.executable_drafts.len())?;
        let parent = if matches!(kind, ExecutableKind::Script { .. } | ExecutableKind::Module) {
            None
        } else if let Some(owner) = self.instance_field_initializer_owner(node_id)? {
            Some(owner)
        } else {
            Some(
                nodes
                    .ancestor_ids(node_id)
                    .find_map(|ancestor| self.node_executables[ancestor.index()])
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "executable parent",
                        span: Some(span),
                    })?,
            )
        };
        if source_executable {
            if semantic.scoping().get_node_id(scope_id) != node_id {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "scope creator matches executable node",
                    span: Some(span),
                });
            }
            if self.exact_scope_executables[scope_id.index()].is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "one executable per created scope",
                    span: Some(span),
                });
            }
        }

        let strict = semantic.scoping().scope_flags(scope_id).is_strict_mode();
        let executable = Executable {
            id,
            parent,
            kind,
            span,
            name,
            name_span,
            strict,
            parameter_count: parameters.count,
            defined_parameter_count: parameters.defined_count,
            simple_parameter_list: parameters.simple,
            parameter_expressions: parameters.expressions,
            parameter_binding_indices: parameters.binding_indices,
            mapped_parameter_indices: parameters.mapped_indices,
            binding_start: 0,
            binding_end: 0,
            resolved_start: 0,
            resolved_end: 0,
            unresolved_start: 0,
            unresolved_end: 0,
            capture_start: 0,
            capture_end: 0,
        };
        if source_executable {
            self.node_executables[node_id.index()] = Some(id);
            self.exact_scope_executables[scope_id.index()] = Some(id);
        } else if self
            .default_class_constructors
            .insert(node_id, id)
            .is_some()
        {
            return Err(CompilerError::SemanticInvariant {
                invariant: "one synthesized default constructor per class",
                span: Some(span),
            });
        }
        self.executable_drafts.push(ExecutableDraft {
            executable,
            node_id,
            scope_id,
        });
        Ok(())
    }

    fn validate_root_executable(&self) -> Result<(), CompilerError> {
        let Some(root) = self.executable_drafts.first() else {
            return Err(CompilerError::SemanticInvariant {
                invariant: "root executable exists",
                span: Some(self.root_span),
            });
        };
        if root.executable.id == ExecutableId(0)
            && root.executable.parent.is_none()
            && root.node_id == NodeId::ROOT
        {
            Ok(())
        } else {
            Err(CompilerError::SemanticInvariant {
                invariant: "root executable identity",
                span: Some(self.root_span),
            })
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "formal layout validates raw, defined, mapped, and expression-bearing parameter domains together"
    )]
    fn validate_parameters(
        &mut self,
        parameters: &oxc_ast::ast::FormalParameters<'arena>,
    ) -> Result<ParameterLayout, CompilerError> {
        let executable = executable_id(self.executable_drafts.len())?;
        let expressions = parameters.items.iter().any(|parameter| {
            parameter.initializer.is_some()
                || binding_pattern_expression_span(&parameter.pattern).is_some()
        }) || parameters
            .rest
            .as_ref()
            .is_some_and(|rest| binding_pattern_expression_span(&rest.rest.argument).is_some());
        let simple = parameters.rest.is_none()
            && parameters.items.iter().all(|parameter| {
                parameter.initializer.is_none()
                    && matches!(parameter.pattern, BindingPattern::BindingIdentifier(_))
            });
        for (index, parameter) in parameters.items.iter().enumerate() {
            if expressions {
                continue;
            }
            let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                continue;
            };
            let parameter_index =
                u32::try_from(index).map_err(|_| CompilerError::CapacityExceeded {
                    domain: "function parameters",
                })?;
            if let Some(previous) = self.parameter_storage.insert(
                identifier.symbol_id(),
                ParameterStorage {
                    executable,
                    parameter_index,
                },
            ) && previous.executable != executable
            {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "parameter symbol belongs to one executable",
                    span: Some(identifier.span),
                });
            }
        }
        let count =
            u32::try_from(parameters.items.len()).map_err(|_| CompilerError::CapacityExceeded {
                domain: "function parameters",
            })?;
        let defined_count = parameters
            .items
            .iter()
            .position(|parameter| parameter.initializer.is_some())
            .map_or(parameters.items.len(), |index| index);
        let defined_count =
            u32::try_from(defined_count).map_err(|_| CompilerError::CapacityExceeded {
                domain: "defined function parameters",
            })?;
        if !simple {
            return Ok(ParameterLayout {
                count,
                defined_count,
                simple,
                expressions,
                binding_indices: Arc::from([]),
                mapped_indices: Arc::from([]),
            });
        }
        let mut binding_index_by_name = HashMap::with_capacity(parameters.items.len());
        let mut mapped_indices = Vec::new();
        mapped_indices
            .try_reserve_exact(parameters.items.len())
            .map_err(|_| CompilerError::CapacityExceeded {
                domain: "mapped function parameters",
            })?;
        for (index, parameter) in parameters.items.iter().enumerate().rev() {
            let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "validated simple parameter remains an identifier",
                    span: Some(parameter.span),
                });
            };
            if !binding_index_by_name.contains_key(identifier.name.as_str()) {
                let index = u32::try_from(index).map_err(|_| CompilerError::CapacityExceeded {
                    domain: "mapped function parameters",
                })?;
                binding_index_by_name.insert(identifier.name.as_str(), index);
                mapped_indices.push(index);
            }
        }
        mapped_indices.reverse();
        let mut binding_indices = Vec::new();
        binding_indices
            .try_reserve_exact(parameters.items.len())
            .map_err(|_| CompilerError::CapacityExceeded {
                domain: "function parameter bindings",
            })?;
        for parameter in &parameters.items {
            let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "validated simple parameter remains an identifier",
                    span: Some(parameter.span),
                });
            };
            binding_indices.push(*binding_index_by_name.get(identifier.name.as_str()).ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "validated parameter name has a binding position",
                    span: Some(identifier.span),
                },
            )?);
        }
        Ok(ParameterLayout {
            count,
            defined_count,
            simple,
            expressions,
            binding_indices: binding_indices.into(),
            mapped_indices: mapped_indices.into(),
        })
    }

    fn assign_scope_owners(&mut self) -> Result<(), CompilerError> {
        let scoping = self.unit.semantic().scoping();
        for scope_id in scoping.scope_descendants_from_root() {
            let owner = scoping
                .scope_ancestors(scope_id)
                .find_map(|ancestor| self.exact_scope_executables[ancestor.index()])
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "scope has executable owner",
                    span: None,
                })?;
            self.scope_executables[scope_id.index()] = Some(owner);
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the synthetic new.target and super validator keeps its owner exceptions in one audited path"
    )]
    fn reject_synthetic_binding_uses(&self) -> Result<(), CompilerError> {
        let nodes = self.unit.semantic().nodes();
        for (node_id, node) in nodes.iter_enumerated() {
            let (span, new_target) = match node.kind() {
                AstKind::NewTarget(expression) => (expression.span, true),
                AstKind::Super(expression) => (expression.span, false),
                _ => continue,
            };
            let static_field_owner = self.static_field_initializer_class_for_node(node_id)?;
            if new_target && static_field_owner.is_some() {
                // ClassDefinitionEvaluation supplies `undefined`, rather than
                // the enclosing function's new.target, for this lexical site.
                continue;
            }
            if !new_target && static_field_owner.is_some() {
                // Static field `super` property access is lowered through the
                // same immutable class receiver cell as lexical `this`.
                continue;
            }
            let instance_field_owner = self.instance_field_initializer_owner(node_id)?;
            let owner =
                instance_field_owner.unwrap_or(self.scope_owner(node.scope_id(), Some(span))?);
            if new_target {
                let mut current = owner;
                loop {
                    let candidate = self.executable_drafts.get(current.index()).ok_or(
                        CompilerError::SemanticInvariant {
                            invariant: "new.target owner executable exists",
                            span: Some(span),
                        },
                    )?;
                    match candidate.executable.kind {
                        ExecutableKind::Function { .. } => break,
                        ExecutableKind::Arrow { .. } => {
                            let Some(parent) = candidate.executable.parent else {
                                return Err(CompilerError::SemanticInvariant {
                                    invariant: "arrow new.target reference has an executable parent",
                                    span: Some(span),
                                });
                            };
                            current = parent;
                        }
                        ExecutableKind::ClassDefaultConstructor
                            if instance_field_owner.is_some() =>
                        {
                            break;
                        }
                        ExecutableKind::ClassDefaultConstructor
                        | ExecutableKind::Script { .. }
                        | ExecutableKind::Module => {
                            return unsupported(UnsupportedFeature::FunctionSyntheticBinding, span);
                        }
                    }
                }
                continue;
            }
            let direct_super_call = matches!(
                nodes.parent_kind(node_id),
                AstKind::CallExpression(call)
                    if matches!(
                        &call.callee,
                        Expression::Super(expression)
                            if expression.node_id.get() == node_id
                    )
            );
            if direct_super_call && instance_field_owner.is_some() {
                return unsupported(UnsupportedFeature::FunctionSyntheticBinding, span);
            }
            let derived_constructor =
                self.executable_drafts
                    .get(owner.index())
                    .is_some_and(|candidate| {
                        matches!(candidate.executable.kind, ExecutableKind::Function { .. })
                            && is_derived_class_constructor(nodes, candidate.node_id)
                    });
            if direct_super_call && derived_constructor {
                continue;
            }
            let direct_super_property = match nodes.parent_kind(node_id) {
                AstKind::StaticMemberExpression(member) => {
                    !member.optional
                        && matches!(
                            &member.object,
                            Expression::Super(expression)
                                if expression.node_id.get() == node_id
                        )
                }
                AstKind::ComputedMemberExpression(member) => {
                    !member.optional
                        && matches!(
                            &member.object,
                            Expression::Super(expression)
                                if expression.node_id.get() == node_id
                        )
                }
                _ => false,
            };
            let home_object_method =
                self.executable_drafts
                    .get(owner.index())
                    .is_some_and(|candidate| {
                        matches!(candidate.executable.kind, ExecutableKind::Function { .. })
                            && is_home_object_method(nodes, candidate.node_id)
                    });
            if direct_super_property && home_object_method {
                continue;
            }
            return unsupported(UnsupportedFeature::FunctionSyntheticBinding, span);
        }
        Ok(())
    }

    fn binding_drafts(&self) -> Result<Vec<BindingDraft>, CompilerError> {
        let semantic = self.unit.semantic();
        let scoping = semantic.scoping();
        let mut drafts = Vec::with_capacity(scoping.symbols_len());
        for symbol_id in scoping.symbol_ids() {
            let flags = scoping.symbol_flags(symbol_id);
            if !flags.is_value() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "JavaScript semantic symbol is a value",
                    span: Some(scoping.symbol_span(symbol_id)),
                });
            }
            let facts = self.declaration_facts(symbol_id, flags)?;
            let owner = self.scope_owner(
                scoping.symbol_scope_id(symbol_id),
                Some(scoping.symbol_span(symbol_id)),
            )?;
            let name = Arc::<str>::from(scoping.symbol_name(symbol_id));
            let declaration_spans = declaration_spans(scoping, symbol_id);
            let split_parameter_environment = self.executable_drafts[owner.index()]
                .executable
                .has_parameter_expressions()
                && facts.contains(DeclarationFacts::PARAMETER)
                && (facts.contains(DeclarationFacts::VAR)
                    || facts.contains(DeclarationFacts::FUNCTION));
            if split_parameter_environment {
                let parameter_span = self.parameter_list_span(owner).ok_or(
                    CompilerError::SemanticInvariant {
                        invariant: "parameter/body collision belongs to a function parameter list",
                        span: Some(scoping.symbol_span(symbol_id)),
                    },
                )?;
                let (parameter_spans, body_spans): (Vec<_>, Vec<_>) = declaration_spans
                    .iter()
                    .copied()
                    .partition(|span| span_within(*span, parameter_span));
                if parameter_spans.is_empty() || body_spans.is_empty() {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: "split parameter and body bindings retain exact declarations",
                        span: Some(scoping.symbol_span(symbol_id)),
                    });
                }
                drafts.push(BindingDraft {
                    symbol_id: Some(symbol_id),
                    primary_symbol_binding: false,
                    class_node: None,
                    class_field_node: None,
                    class_static_receiver_node: None,
                    executable: owner,
                    name: Arc::clone(&name),
                    declaration_spans: parameter_spans.into(),
                    placement: StoragePlacement::Local,
                    policy: self.declaration_policy(owner, DeclarationKind::Parameter, false),
                    arguments_object: false,
                });
                let body_kind = if facts.contains(DeclarationFacts::FUNCTION) {
                    DeclarationKind::Function
                } else {
                    DeclarationKind::Var
                };
                drafts.push(BindingDraft {
                    symbol_id: Some(symbol_id),
                    primary_symbol_binding: true,
                    class_node: None,
                    class_field_node: None,
                    class_static_receiver_node: None,
                    executable: owner,
                    name,
                    declaration_spans: body_spans.into(),
                    placement: StoragePlacement::Local,
                    policy: self.declaration_policy(owner, body_kind, facts.function_scope_entry),
                    arguments_object: false,
                });
                continue;
            }
            let kind = facts
                .effective_kind()
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "known JavaScript declaration kind",
                    span: Some(scoping.symbol_span(symbol_id)),
                })?;
            let placement = self.placement(symbol_id, owner, kind)?;
            let policy = self.declaration_policy(owner, kind, facts.function_scope_entry);
            drafts.push(BindingDraft {
                symbol_id: Some(symbol_id),
                primary_symbol_binding: true,
                class_node: None,
                class_field_node: None,
                class_static_receiver_node: None,
                executable: owner,
                name,
                declaration_spans,
                placement,
                policy,
                arguments_object: false,
            });
        }
        Ok(drafts)
    }

    fn parameter_list_span(&self, executable: ExecutableId) -> Option<Span> {
        let node = self.executable_drafts.get(executable.index())?.node_id;
        match self.unit.semantic().nodes().kind(node) {
            AstKind::Function(function) => Some(function.params.span),
            AstKind::ArrowFunctionExpression(arrow) => Some(arrow.params.span),
            _ => None,
        }
    }

    fn declaration_facts(
        &self,
        symbol_id: SymbolId,
        flags: SymbolFlags,
    ) -> Result<DeclarationFacts, CompilerError> {
        let semantic = self.unit.semantic();
        let scoping = semantic.scoping();
        let mut facts = DeclarationFacts::default();
        if flags.contains(SymbolFlags::FunctionExpression) {
            facts.insert(DeclarationFacts::FUNCTION_NAME);
        }
        for declaration in scoping.symbol_declarations(symbol_id) {
            match semantic.nodes().kind(declaration) {
                AstKind::FormalParameter(_) | AstKind::FormalParameterRest(_) => {
                    facts.insert(DeclarationFacts::PARAMETER);
                }
                AstKind::VariableDeclarator(declarator) => match declarator.kind {
                    VariableDeclarationKind::Var => facts.insert(DeclarationFacts::VAR),
                    VariableDeclarationKind::Let => facts.insert(DeclarationFacts::LET),
                    VariableDeclarationKind::Const => facts.insert(DeclarationFacts::CONST),
                    VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => {
                        return Err(CompilerError::SemanticInvariant {
                            invariant: "frontend rejected using declaration",
                            span: Some(declarator.span),
                        });
                    }
                },
                AstKind::Function(_) => {
                    facts.insert(DeclarationFacts::FUNCTION);
                    if !facts.contains(DeclarationFacts::FUNCTION_NAME) {
                        let declaration_scope = semantic.nodes().get_node(declaration).scope_id();
                        facts.function_scope_entry |=
                            !scoping.scope_flags(declaration_scope).is_var();
                    }
                }
                AstKind::Class(_) => facts.insert(DeclarationFacts::CLASS),
                AstKind::CatchParameter(_) => facts.insert(DeclarationFacts::CATCH),
                AstKind::ImportSpecifier(_) | AstKind::ImportDefaultSpecifier(_) => {
                    facts.insert(DeclarationFacts::IMPORT);
                }
                AstKind::ImportNamespaceSpecifier(_) => {
                    facts.insert(DeclarationFacts::NAMESPACE_IMPORT);
                }
                other => {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: declaration_kind_invariant(other),
                        span: Some(scoping.symbol_span(symbol_id)),
                    });
                }
            }
        }
        Ok(facts)
    }

    fn placement(
        &self,
        symbol_id: SymbolId,
        owner: ExecutableId,
        kind: DeclarationKind,
    ) -> Result<StoragePlacement, CompilerError> {
        if let Some(parameter) = self.parameter_storage.get(&symbol_id) {
            if parameter.executable != owner {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "parameter symbol belongs to parameter executable",
                    span: Some(self.unit.semantic().scoping().symbol_span(symbol_id)),
                });
            }
            return Ok(StoragePlacement::Argument {
                parameter_index: parameter.parameter_index,
            });
        }

        if owner != ExecutableId(0) {
            return Ok(StoragePlacement::Local);
        }
        let scoping = self.unit.semantic().scoping();
        let root_scope = scoping.root_scope_id();
        let symbol_scope = scoping.symbol_scope_id(symbol_id);
        if symbol_scope != root_scope {
            return Ok(StoragePlacement::Local);
        }
        match self.kind {
            CompilationUnitKind::Script => match kind {
                DeclarationKind::Let | DeclarationKind::Const | DeclarationKind::Class
                    if crate::is_supported_dynamic_function_goal(self.unit.goal()) =>
                {
                    Ok(StoragePlacement::Local)
                }
                DeclarationKind::Let | DeclarationKind::Const | DeclarationKind::Class => {
                    Ok(StoragePlacement::GlobalLexical)
                }
                DeclarationKind::Var | DeclarationKind::Function => {
                    Ok(StoragePlacement::GlobalObject)
                }
                DeclarationKind::FunctionName
                | DeclarationKind::ClassName
                | DeclarationKind::ClassFieldKey
                | DeclarationKind::ClassStaticReceiver
                | DeclarationKind::Parameter
                | DeclarationKind::Catch
                | DeclarationKind::Import
                | DeclarationKind::NamespaceImport
                | DeclarationKind::SyntheticDefault => Err(CompilerError::SemanticInvariant {
                    invariant: "valid root Script binding category",
                    span: Some(scoping.symbol_span(symbol_id)),
                }),
            },
            CompilationUnitKind::Module => match kind {
                DeclarationKind::Import => Ok(StoragePlacement::ModuleImport),
                DeclarationKind::NamespaceImport
                | DeclarationKind::Var
                | DeclarationKind::Let
                | DeclarationKind::Const
                | DeclarationKind::Class
                | DeclarationKind::Function => Ok(StoragePlacement::ModuleLocal),
                DeclarationKind::FunctionName
                | DeclarationKind::ClassName
                | DeclarationKind::ClassFieldKey
                | DeclarationKind::ClassStaticReceiver
                | DeclarationKind::Parameter
                | DeclarationKind::Catch
                | DeclarationKind::SyntheticDefault => Err(CompilerError::SemanticInvariant {
                    invariant: "valid root Module binding category",
                    span: Some(scoping.symbol_span(symbol_id)),
                }),
            },
        }
    }

    fn declaration_policy(
        &self,
        owner: ExecutableId,
        kind: DeclarationKind,
        function_scope_entry: bool,
    ) -> DeclarationPolicy {
        let (initialization, writes, temporal_dead_zone) = match kind {
            DeclarationKind::Parameter => (
                InitializationPolicy::Argument,
                WritePolicy::Mutable,
                self.executable_drafts[owner.index()]
                    .executable
                    .has_parameter_expressions(),
            ),
            DeclarationKind::Var => (
                InitializationPolicy::UndefinedAtInstantiation,
                WritePolicy::Mutable,
                false,
            ),
            DeclarationKind::Let | DeclarationKind::Class => (
                InitializationPolicy::AtDeclaration,
                WritePolicy::Mutable,
                true,
            ),
            DeclarationKind::Const
            | DeclarationKind::ClassName
            | DeclarationKind::ClassFieldKey
            | DeclarationKind::ClassStaticReceiver => (
                InitializationPolicy::AtDeclaration,
                WritePolicy::Immutable,
                true,
            ),
            DeclarationKind::Function => (
                if function_scope_entry
                    || (matches!(
                        self.executable_drafts[owner.index()].executable.kind,
                        ExecutableKind::Function { .. }
                    ) && !self.executable_drafts[owner.index()]
                        .executable
                        .simple_parameter_list)
                {
                    InitializationPolicy::FunctionAtScopeEntry
                } else {
                    InitializationPolicy::FunctionAtInstantiation
                },
                WritePolicy::Mutable,
                false,
            ),
            DeclarationKind::FunctionName => (
                InitializationPolicy::FunctionName,
                if self.executable_drafts[owner.index()].executable.strict {
                    WritePolicy::Immutable
                } else {
                    WritePolicy::ImmutableInStrictCode
                },
                false,
            ),
            DeclarationKind::Catch => (InitializationPolicy::Catch, WritePolicy::Mutable, false),
            DeclarationKind::Import => (
                InitializationPolicy::ModuleImport,
                WritePolicy::Immutable,
                true,
            ),
            DeclarationKind::NamespaceImport => (
                InitializationPolicy::ModuleNamespace,
                WritePolicy::Immutable,
                true,
            ),
            DeclarationKind::SyntheticDefault => (
                InitializationPolicy::AtDeclaration,
                WritePolicy::Internal,
                true,
            ),
        };
        DeclarationPolicy {
            kind,
            initialization,
            writes,
            temporal_dead_zone,
        }
    }

    fn add_synthetic_default_binding(
        &self,
        bindings: &mut Vec<BindingDraft>,
    ) -> Result<(), CompilerError> {
        if self.kind != CompilationUnitKind::Module {
            return Ok(());
        }
        let mut synthetic_spans = self
            .unit
            .module_syntax()
            .export_entries()
            .iter()
            .filter_map(|entry| {
                matches!(entry.local_name(), ModuleExportLocalName::SyntheticDefault)
                    .then_some(entry.span())
            })
            .collect::<Vec<_>>();
        if synthetic_spans.is_empty() {
            return Ok(());
        }
        synthetic_spans.sort_by_key(|span| (span.start, span.end));
        synthetic_spans.dedup();
        let policy = self.synthetic_default_policy()?;
        bindings.push(BindingDraft {
            symbol_id: None,
            primary_symbol_binding: false,
            class_node: None,
            class_field_node: None,
            class_static_receiver_node: None,
            executable: ExecutableId(0),
            name: Arc::from("*default*"),
            declaration_spans: synthetic_spans.into(),
            placement: StoragePlacement::ModuleLocal,
            policy,
            arguments_object: false,
        });
        Ok(())
    }

    /// Adds the lexical class-name environment required by
    /// `ClassDefinitionEvaluation`.  It deliberately has no Oxc symbol: Oxc
    /// represents the declaration name once, while ECMAScript gives member
    /// closures a second, immutable binding whose lifetime ends only after
    /// those closures have captured it.
    fn add_class_name_bindings(
        &self,
        bindings: &mut Vec<BindingDraft>,
    ) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        for (node_id, node) in semantic.nodes().iter_enumerated() {
            let AstKind::Class(class) = node.kind() else {
                continue;
            };
            let Some(identifier) = class.id.as_ref() else {
                continue;
            };
            let owner = self.scope_owner(class.scope_id(), Some(class.span))?;
            bindings.push(BindingDraft {
                symbol_id: None,
                primary_symbol_binding: false,
                class_node: Some(node_id),
                class_field_node: None,
                class_static_receiver_node: None,
                executable: owner,
                name: Arc::from(identifier.name.as_str()),
                declaration_spans: Arc::from([identifier.span]),
                placement: StoragePlacement::Local,
                policy: self.declaration_policy(owner, DeclarationKind::ClassName, false),
                arguments_object: false,
            });
        }
        Ok(())
    }

    /// Adds one fresh class-scope cell for every computed public instance
    /// field. The constructor captures the cell; class definition evaluation
    /// stores the once-converted property key before the constructor can run.
    fn add_class_field_key_bindings(
        &self,
        bindings: &mut Vec<BindingDraft>,
    ) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        for (_class_node, node) in semantic.nodes().iter_enumerated() {
            let AstKind::Class(class) = node.kind() else {
                continue;
            };
            let owner = self.scope_owner(class.scope_id(), Some(class.span))?;
            for element in &class.body.body {
                let ClassElement::PropertyDefinition(field) = element else {
                    continue;
                };
                if field.r#static || !field.computed {
                    continue;
                }
                let field_node = field.node_id.get();
                bindings.push(BindingDraft {
                    symbol_id: None,
                    primary_symbol_binding: false,
                    class_node: None,
                    class_field_node: Some(field_node),
                    class_static_receiver_node: None,
                    executable: owner,
                    name: Arc::from(format!("[[class-field-key:{}]]", field_node.index())),
                    declaration_spans: Arc::from([field.key.span()]),
                    placement: StoragePlacement::Local,
                    policy: self.declaration_policy(owner, DeclarationKind::ClassFieldKey, false),
                    arguments_object: false,
                });
            }
        }
        Ok(())
    }

    fn class_field_key_capture_requests(
        &self,
        class_field_key_bindings: &HashMap<NodeId, BindingId>,
        bindings: &[BindingStorage],
    ) -> Result<Vec<CaptureRequest>, CompilerError> {
        let nodes = self.unit.semantic().nodes();
        let mut requests = Vec::with_capacity(class_field_key_bindings.len());
        for (&field_node, &binding) in class_field_key_bindings {
            let AstKind::PropertyDefinition(field) = nodes.kind(field_node) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class-field key capture belongs to a property definition",
                    span: None,
                });
            };
            if field.r#static || !field.computed {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class-field key capture belongs to a computed instance field",
                    span: Some(field.span),
                });
            }
            let AstKind::ClassBody(body) = nodes.parent_kind(field_node) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "computed instance field belongs to a class body",
                    span: Some(field.span),
                });
            };
            let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "computed instance field class body belongs to a class",
                    span: Some(body.span),
                });
            };
            let storage =
                bindings
                    .get(binding.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "class-field key capture binding exists",
                        span: Some(field.key.span()),
                    })?;
            if storage.placement != StoragePlacement::Local
                || storage.policy.kind != DeclarationKind::ClassFieldKey
            {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class-field key capture uses an immutable local binding",
                    span: Some(field.key.span()),
                });
            }
            let constructor = self.instance_field_constructor_owner(class.node_id.get(), class)?;
            if constructor == storage.executable {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class constructor captures a distinct class-scope key binding",
                    span: Some(field.key.span()),
                });
            }
            requests.push(CaptureRequest {
                executable: constructor,
                binding,
                span: field.key.span(),
            });
        }
        requests.sort_unstable_by_key(|request| {
            (
                request.executable.index(),
                request.binding.index(),
                request.span.start,
                request.span.end,
            )
        });
        Ok(requests)
    }

    /// Adds the class-definition receiver cell only when a static field value
    /// lexically observes `this` or resolves a `super` property. The class
    /// scope owns the cell, while arrow function frames capture it like every
    /// other lexical binding.
    fn add_class_static_receiver_bindings(
        &self,
        bindings: &mut Vec<BindingDraft>,
    ) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        for (class_node, node) in semantic.nodes().iter_enumerated() {
            let AstKind::Class(class) = node.kind() else {
                continue;
            };
            if !self.class_static_receiver_is_used(class_node)? {
                continue;
            }
            let owner = self.scope_owner(class.scope_id(), Some(class.span))?;
            bindings.push(BindingDraft {
                symbol_id: None,
                primary_symbol_binding: false,
                class_node: None,
                class_field_node: None,
                class_static_receiver_node: Some(class_node),
                executable: owner,
                name: Arc::from(format!("[[class-static-receiver:{}]]", class_node.index())),
                declaration_spans: Arc::from([class.span]),
                placement: StoragePlacement::Local,
                policy: self.declaration_policy(owner, DeclarationKind::ClassStaticReceiver, false),
                arguments_object: false,
            });
        }
        Ok(())
    }

    fn class_static_receiver_is_used(&self, class_node: NodeId) -> Result<bool, CompilerError> {
        let nodes = self.unit.semantic().nodes();
        for (node_id, node) in nodes.iter_enumerated() {
            if !matches!(node.kind(), AstKind::ThisExpression(_) | AstKind::Super(_)) {
                continue;
            }
            if self.static_field_initializer_class_for_node(node_id)? == Some(class_node) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn class_static_receiver_capture_requests(
        &self,
        class_static_receiver_bindings: &HashMap<NodeId, BindingId>,
        bindings: &[BindingStorage],
    ) -> Result<Vec<CaptureRequest>, CompilerError> {
        let nodes = self.unit.semantic().nodes();
        let mut requests = Vec::new();
        for (node_id, node) in nodes.iter_enumerated() {
            let span = match node.kind() {
                AstKind::ThisExpression(expression) => expression.span,
                AstKind::Super(expression) => expression.span,
                _ => continue,
            };
            let Some(class_node) = self.static_field_initializer_class_for_node(node_id)? else {
                continue;
            };
            let binding = class_static_receiver_bindings
                .get(&class_node)
                .copied()
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "static field lexical receiver has a class receiver binding",
                    span: Some(span),
                })?;
            let storage =
                bindings
                    .get(binding.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "class static-receiver capture binding exists",
                        span: Some(span),
                    })?;
            if storage.placement != StoragePlacement::Local
                || storage.policy.kind != DeclarationKind::ClassStaticReceiver
            {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "class static-receiver capture uses an immutable local binding",
                    span: Some(span),
                });
            }
            let executable = self.scope_owner(node.scope_id(), Some(span))?;
            if executable != storage.executable {
                requests.push(CaptureRequest {
                    executable,
                    binding,
                    span,
                });
            }
        }
        requests.sort_unstable_by_key(|request| {
            (
                request.executable.index(),
                request.binding.index(),
                request.span.start,
                request.span.end,
            )
        });
        requests.dedup_by_key(|request| (request.executable, request.binding));
        Ok(requests)
    }

    /// Returns the innermost class whose static field *value* lexically owns
    /// `node_id`. Ordinary functions establish their own `this` and
    /// `new.target`; arrows deliberately do not.
    fn static_field_initializer_class_for_node(
        &self,
        node_id: NodeId,
    ) -> Result<Option<NodeId>, CompilerError> {
        let nodes = self.unit.semantic().nodes();
        let node_span = nodes.kind(node_id).span();
        for ancestor in nodes.ancestor_ids(node_id) {
            match nodes.kind(ancestor) {
                AstKind::Function(_) => return Ok(None),
                AstKind::PropertyDefinition(field)
                    if field.r#static
                        && field
                            .value
                            .as_ref()
                            .is_some_and(|value| span_within(node_span, value.span())) =>
                {
                    let AstKind::ClassBody(body) = nodes.parent_kind(field.node_id.get()) else {
                        return Err(CompilerError::SemanticInvariant {
                            invariant: "static field belongs to a class body",
                            span: Some(field.span),
                        });
                    };
                    let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                        return Err(CompilerError::SemanticInvariant {
                            invariant: "static field class body belongs to a class",
                            span: Some(body.span),
                        });
                    };
                    return Ok(Some(class.node_id.get()));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn synthetic_default_policy(&self) -> Result<DeclarationPolicy, CompilerError> {
        let mut declarations = self.unit.program().body.iter().filter_map(|statement| {
            let Statement::ExportDefaultDeclaration(declaration) = statement else {
                return None;
            };
            let synthetic = match &declaration.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                    function.id.is_none()
                }
                ExportDefaultDeclarationKind::ClassDeclaration(class) => class.id.is_none(),
                ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => false,
                _ => true,
            };
            synthetic.then_some(declaration)
        });
        let declaration = declarations
            .next()
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "synthetic default export statement exists",
                span: Some(self.root_span),
            })?;
        if declarations.next().is_some() {
            return Err(CompilerError::SemanticInvariant {
                invariant: "one synthetic default export statement",
                span: Some(declaration.span),
            });
        }

        match &declaration.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                if function.id.is_some() {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: "synthetic default function is anonymous",
                        span: Some(function.span),
                    });
                }
                Ok(DeclarationPolicy {
                    kind: DeclarationKind::SyntheticDefault,
                    initialization: InitializationPolicy::FunctionAtInstantiation,
                    writes: WritePolicy::Internal,
                    temporal_dead_zone: false,
                })
            }
            ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                unsupported(UnsupportedFeature::AnonymousDefaultClassExport, class.span)
            }
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(interface) => {
                Err(CompilerError::SemanticInvariant {
                    invariant: "frontend rejected TypeScript default export",
                    span: Some(interface.span),
                })
            }
            _ => Ok(self.declaration_policy(
                ExecutableId(0),
                DeclarationKind::SyntheticDefault,
                false,
            )),
        }
    }

    fn resolved_drafts(
        &self,
        symbol_bindings: &[Option<BindingId>],
        source_symbols: &[Option<SymbolId>],
        class_name_bindings: &HashMap<NodeId, BindingId>,
        bindings: &[BindingStorage],
        implicit_arguments_references: &HashMap<ReferenceId, ExecutableId>,
    ) -> Result<Vec<ResolvedDraft>, CompilerError> {
        let semantic = self.unit.semantic();
        let scoping = semantic.scoping();
        let arguments_bindings = bindings
            .iter()
            .filter(|binding| binding.arguments_object)
            .map(|binding| (binding.executable, binding.id))
            .collect::<HashMap<_, _>>();
        let mut split_parameter_bindings = HashMap::new();
        for (binding, symbol) in bindings.iter().zip(source_symbols.iter().copied()) {
            let Some(symbol) = symbol else {
                continue;
            };
            if binding.policy.kind == DeclarationKind::Parameter
                && symbol_bindings.get(symbol.index()).copied().flatten() != Some(binding.id)
                && split_parameter_bindings
                    .insert(symbol, binding.id)
                    .is_some()
            {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "one split parameter binding per semantic symbol",
                    span: binding.declaration_spans.first().copied(),
                });
            }
        }
        let mut drafts = Vec::with_capacity(scoping.references_len());
        for symbol_id in scoping.symbol_ids() {
            let source_binding = symbol_bindings
                .get(symbol_id.index())
                .and_then(|binding| *binding)
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "semantic symbol has compiler binding",
                    span: Some(scoping.symbol_span(symbol_id)),
                })?;
            for &reference_id in scoping.get_resolved_reference_ids(symbol_id) {
                let reference = scoping.get_reference(reference_id);
                let span = semantic.reference_span(reference);
                let executable = self
                    .instance_field_initializer_owner(reference.node_id())?
                    .unwrap_or(self.scope_owner(reference.scope_id(), Some(span))?);
                let binding = if let Some(owner) =
                    implicit_arguments_references.get(&reference_id).copied()
                {
                    arguments_bindings.get(&owner).copied().ok_or(
                        CompilerError::SemanticInvariant {
                            invariant: "implicit arguments reference has an owned binding",
                            span: Some(span),
                        },
                    )?
                } else if let Some(parameter_binding) =
                    split_parameter_bindings.get(&symbol_id).copied()
                    && bindings
                        .get(source_binding.index())
                        .and_then(|binding| self.parameter_list_span(binding.executable))
                        .is_some_and(|parameters| span_within(span, parameters))
                {
                    parameter_binding
                } else {
                    source_binding
                };
                let binding = self
                    .class_name_binding_for_reference(
                        reference.node_id(),
                        symbol_id,
                        class_name_bindings,
                    )
                    .unwrap_or(binding);
                drafts.push(ResolvedDraft {
                    reference_id,
                    executable,
                    binding,
                    span,
                    access: ReferenceAccess {
                        read: reference.is_read(),
                        write: reference.is_write(),
                    },
                });
            }
        }
        Ok(drafts)
    }

    fn class_name_binding_for_reference(
        &self,
        reference_node: NodeId,
        symbol: SymbolId,
        class_name_bindings: &HashMap<NodeId, BindingId>,
    ) -> Option<BindingId> {
        let nodes = self.unit.semantic().nodes();
        nodes.ancestor_ids(reference_node).find_map(|node_id| {
            let AstKind::Class(class) = nodes.kind(node_id) else {
                return None;
            };
            (class.id.as_ref()?.symbol_id.get() == Some(symbol))
                .then(|| class_name_bindings.get(&node_id).copied())
                .flatten()
        })
    }

    fn unresolved_drafts(&self) -> Result<Vec<UnresolvedDraft>, CompilerError> {
        let semantic = self.unit.semantic();
        let scoping = semantic.scoping();
        let mut references = scoping
            .root_unresolved_references_ids()
            .flatten()
            .collect::<Vec<_>>();
        references
            .sort_by_key(|reference_id| scoping.get_reference(*reference_id).node_id().index());

        let mut drafts = Vec::with_capacity(references.len());
        for reference_id in references {
            let reference = scoping.get_reference(reference_id);
            let span = semantic.reference_span(reference);
            let executable = self
                .instance_field_initializer_owner(reference.node_id())?
                .unwrap_or(self.scope_owner(reference.scope_id(), Some(span))?);
            let name = semantic.reference_name(reference);
            drafts.push(UnresolvedDraft {
                reference_id,
                executable,
                name: Arc::from(name),
                span,
                access: ReferenceAccess {
                    read: reference.is_read(),
                    write: reference.is_write(),
                },
            });
        }
        Ok(drafts)
    }

    fn add_arguments_bindings(
        &self,
        bindings: &mut Vec<BindingDraft>,
    ) -> Result<HashMap<ReferenceId, ExecutableId>, CompilerError> {
        let ImplicitArgumentsPlan {
            reference_owners,
            first_references,
        } = self.collect_implicit_arguments_references(bindings)?;
        let mut first_references = first_references.into_iter().collect::<Vec<_>>();
        first_references
            .sort_unstable_by_key(|(owner, span)| (owner.index(), span.start, span.end));
        for (owner, span) in first_references {
            let mut reusable = bindings.iter_mut().filter(|binding| {
                binding.executable == owner
                    && binding.name.as_ref() == "arguments"
                    && binding.policy.kind == DeclarationKind::Var
                    && !self.executable_drafts[owner.index()]
                        .executable
                        .has_parameter_expressions()
            });
            if let Some(binding) = reusable.next() {
                if reusable.next().is_some()
                    || binding.placement != StoragePlacement::Local
                    || binding.policy.initialization
                        != InitializationPolicy::UndefinedAtInstantiation
                {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: "arguments var is one ordinary function-local binding",
                        span: binding.declaration_spans.first().copied(),
                    });
                }
                binding.arguments_object = true;
                continue;
            }
            bindings.push(BindingDraft {
                symbol_id: None,
                primary_symbol_binding: false,
                class_node: None,
                class_field_node: None,
                class_static_receiver_node: None,
                executable: owner,
                name: Arc::from("arguments"),
                declaration_spans: Arc::from([span]),
                placement: StoragePlacement::Local,
                policy: self.declaration_policy(owner, DeclarationKind::Var, false),
                arguments_object: true,
            });
        }
        Ok(reference_owners)
    }

    fn collect_implicit_arguments_references(
        &self,
        bindings: &[BindingDraft],
    ) -> Result<ImplicitArgumentsPlan, CompilerError> {
        let semantic = self.unit.semantic();
        let scoping = semantic.scoping();
        let arguments_parameter_owners = bindings
            .iter()
            .filter(|binding| {
                binding.name.as_ref() == "arguments"
                    && binding.policy.kind == DeclarationKind::Parameter
            })
            .map(|binding| binding.executable)
            .collect::<HashSet<_>>();
        let mut binding_by_symbol = vec![None; scoping.symbols_len()];
        for (index, binding) in bindings.iter().enumerate() {
            let Some(symbol) = binding.symbol_id else {
                continue;
            };
            if !binding.primary_symbol_binding {
                continue;
            }
            let target = binding_by_symbol.get_mut(symbol.index()).ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "arguments source symbol indexes its binding draft",
                    span: binding.declaration_spans.first().copied(),
                },
            )?;
            if target.replace(index).is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "one binding draft per arguments source symbol",
                    span: binding.declaration_spans.first().copied(),
                });
            }
        }
        let mut implicit_references = HashMap::new();
        let mut first_references = HashMap::<ExecutableId, Span>::new();
        for symbol in scoping
            .symbol_ids()
            .filter(|symbol| scoping.symbol_name(*symbol) == "arguments")
        {
            let binding = binding_by_symbol
                .get(symbol.index())
                .and_then(|index| *index)
                .and_then(|index| bindings.get(index))
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "arguments source symbol has a binding draft",
                    span: Some(scoping.symbol_span(symbol)),
                })?;
            for &reference_id in scoping.get_resolved_reference_ids(symbol) {
                let reference = scoping.get_reference(reference_id);
                let span = semantic.reference_span(reference);
                let executable = self.scope_owner(reference.scope_id(), Some(span))?;
                let Some(owner) = self.arguments_owner(executable, span)? else {
                    continue;
                };
                if self.reference_uses_explicit_arguments_binding(
                    executable,
                    owner,
                    binding,
                    arguments_parameter_owners.contains(&owner),
                    span,
                )? {
                    continue;
                }
                Self::record_implicit_arguments_reference(
                    &mut implicit_references,
                    &mut first_references,
                    reference_id,
                    owner,
                    span,
                )?;
            }
        }
        for reference_id in scoping.root_unresolved_references_ids().flatten() {
            let reference = scoping.get_reference(reference_id);
            if semantic.reference_name(reference) != "arguments" {
                continue;
            }
            let span = semantic.reference_span(reference);
            let executable = self.scope_owner(reference.scope_id(), Some(span))?;
            let Some(owner) = self.arguments_owner(executable, span)? else {
                continue;
            };
            Self::record_implicit_arguments_reference(
                &mut implicit_references,
                &mut first_references,
                reference_id,
                owner,
                span,
            )?;
        }
        self.seed_body_arguments_object_requirements(
            bindings,
            &arguments_parameter_owners,
            &mut first_references,
        )?;
        Ok(ImplicitArgumentsPlan {
            reference_owners: implicit_references,
            first_references,
        })
    }

    fn seed_body_arguments_object_requirements(
        &self,
        bindings: &[BindingDraft],
        arguments_parameter_owners: &HashSet<ExecutableId>,
        first_references: &mut HashMap<ExecutableId, Span>,
    ) -> Result<(), CompilerError> {
        for binding in bindings {
            let needs_separate_object = binding.name.as_ref() == "arguments"
                && matches!(
                    binding.policy.kind,
                    DeclarationKind::Var | DeclarationKind::Function
                )
                && self.executable_drafts[binding.executable.index()]
                    .executable
                    .has_parameter_expressions()
                && !arguments_parameter_owners.contains(&binding.executable);
            if !needs_separate_object {
                continue;
            }
            let span = binding.declaration_spans.first().copied().ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "body arguments declaration has a source span",
                    span: None,
                },
            )?;
            first_references
                .entry(binding.executable)
                .and_modify(|first| {
                    if (span.start, span.end) < (first.start, first.end) {
                        *first = span;
                    }
                })
                .or_insert(span);
        }
        Ok(())
    }

    fn reference_uses_explicit_arguments_binding(
        &self,
        reference_executable: ExecutableId,
        arguments_owner: ExecutableId,
        binding: &BindingDraft,
        owner_has_arguments_parameter: bool,
        span: Span,
    ) -> Result<bool, CompilerError> {
        if binding.executable == arguments_owner {
            if owner_has_arguments_parameter {
                return Ok(true);
            }
            if self.executable_drafts[arguments_owner.index()]
                .executable
                .has_parameter_expressions()
                && matches!(
                    binding.policy.kind,
                    DeclarationKind::Var | DeclarationKind::Function
                )
            {
                let parameters = self.parameter_list_span(arguments_owner).ok_or(
                    CompilerError::SemanticInvariant {
                        invariant: "parameter-expression arguments owner has formal parameters",
                        span: Some(span),
                    },
                )?;
                return Ok(!span_within(span, parameters));
            }
            return Ok(!matches!(
                binding.policy.kind,
                DeclarationKind::Var | DeclarationKind::FunctionName
            ));
        }
        let mut executable = reference_executable;
        while executable != arguments_owner {
            if executable == binding.executable {
                return Ok(true);
            }
            executable = self
                .executable_drafts
                .get(executable.index())
                .and_then(|draft| draft.executable.parent)
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "arguments reference reaches its ordinary function owner",
                    span: Some(span),
                })?;
        }
        Ok(false)
    }

    fn record_implicit_arguments_reference(
        references: &mut HashMap<ReferenceId, ExecutableId>,
        first_references: &mut HashMap<ExecutableId, Span>,
        reference: ReferenceId,
        owner: ExecutableId,
        span: Span,
    ) -> Result<(), CompilerError> {
        if let Some(previous) = references.insert(reference, owner)
            && previous != owner
        {
            return Err(CompilerError::SemanticInvariant {
                invariant: "arguments reference has one ordinary function owner",
                span: Some(span),
            });
        }
        first_references
            .entry(owner)
            .and_modify(|first| {
                if (span.start, span.end) < (first.start, first.end) {
                    *first = span;
                }
            })
            .or_insert(span);
        Ok(())
    }

    fn resolve_arguments_references(
        unresolved: Vec<UnresolvedDraft>,
        bindings: &[BindingStorage],
        implicit_arguments_references: &HashMap<ReferenceId, ExecutableId>,
    ) -> Result<(Vec<ResolvedDraft>, Vec<UnresolvedDraft>), CompilerError> {
        let arguments_bindings = bindings
            .iter()
            .filter(|binding| binding.arguments_object)
            .map(|binding| (binding.executable, binding.id))
            .collect::<HashMap<_, _>>();
        let mut resolved = Vec::new();
        let mut remaining = Vec::new();
        for reference in unresolved {
            let Some(owner) = implicit_arguments_references
                .get(&reference.reference_id)
                .copied()
            else {
                remaining.push(reference);
                continue;
            };
            if reference.name.as_ref() != "arguments" {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "implicit arguments reference retains its source name",
                    span: Some(reference.span),
                });
            }
            let binding = arguments_bindings.get(&owner).copied().ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "implicit arguments reference has an owned binding",
                    span: Some(reference.span),
                },
            )?;
            resolved.push(ResolvedDraft {
                reference_id: reference.reference_id,
                executable: reference.executable,
                binding,
                span: reference.span,
                access: reference.access,
            });
        }
        Ok((resolved, remaining))
    }

    fn arguments_owner(
        &self,
        mut executable: ExecutableId,
        span: Span,
    ) -> Result<Option<ExecutableId>, CompilerError> {
        loop {
            let draft = self.executable_drafts.get(executable.index()).ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "arguments reference executable exists",
                    span: Some(span),
                },
            )?;
            match draft.executable.kind {
                ExecutableKind::Function { .. } => {
                    return Ok(Some(executable));
                }
                ExecutableKind::Arrow { .. } => {
                    executable =
                        draft
                            .executable
                            .parent
                            .ok_or(CompilerError::SemanticInvariant {
                                invariant: "arrow arguments reference has an executable parent",
                                span: Some(span),
                            })?;
                }
                ExecutableKind::ClassDefaultConstructor
                | ExecutableKind::Script { .. }
                | ExecutableKind::Module => return Ok(None),
            }
        }
    }

    fn scope_owner(
        &self,
        scope_id: ScopeId,
        span: Option<Span>,
    ) -> Result<ExecutableId, CompilerError> {
        self.scope_executables
            .get(scope_id.index())
            .and_then(|owner| *owner)
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "scope executable owner",
                span,
            })
    }

    fn instance_field_initializer_owner(
        &self,
        node_id: NodeId,
    ) -> Result<Option<ExecutableId>, CompilerError> {
        let nodes = self.unit.semantic().nodes();
        let node_span = nodes.kind(node_id).span();
        for ancestor in nodes.ancestor_ids(node_id) {
            match nodes.kind(ancestor) {
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_) => return Ok(None),
                AstKind::PropertyDefinition(field)
                    if !field.r#static
                        && field
                            .value
                            .as_ref()
                            .is_some_and(|value| span_within(node_span, value.span())) =>
                {
                    let AstKind::ClassBody(body) = nodes.parent_kind(field.node_id.get()) else {
                        return Err(CompilerError::SemanticInvariant {
                            invariant: "instance field belongs to a class body",
                            span: Some(field.span),
                        });
                    };
                    let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                        return Err(CompilerError::SemanticInvariant {
                            invariant: "instance field class body belongs to a class",
                            span: Some(body.span),
                        });
                    };
                    return self
                        .instance_field_constructor_owner(class.node_id.get(), class)
                        .map(Some);
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn instance_field_constructor_owner(
        &self,
        class_node: NodeId,
        class: &oxc_ast::ast::Class<'arena>,
    ) -> Result<ExecutableId, CompilerError> {
        for element in &class.body.body {
            let ClassElement::MethodDefinition(method) = element else {
                continue;
            };
            if method.kind != MethodDefinitionKind::Constructor {
                continue;
            }
            return self
                .node_executables
                .get(method.value.node_id.get().index())
                .copied()
                .flatten()
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "class constructor owns its public field references",
                    span: Some(method.span),
                });
        }
        self.default_class_constructors
            .get(&class_node)
            .copied()
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "class without a source constructor owns a synthesized field template",
                span: Some(class.span),
            })
    }
}

fn unsupported<T>(feature: UnsupportedFeature, span: Span) -> Result<T, CompilerError> {
    Err(CompilerError::Unsupported { feature, span })
}

fn executable_id(index: usize) -> Result<ExecutableId, CompilerError> {
    u32::try_from(index)
        .map(ExecutableId)
        .map_err(|_| CompilerError::CapacityExceeded {
            domain: "executables",
        })
}

fn binding_pattern_expression_span(pattern: &BindingPattern<'_>) -> Option<Span> {
    match pattern {
        BindingPattern::BindingIdentifier(_) => None,
        BindingPattern::AssignmentPattern(pattern) => Some(pattern.span),
        BindingPattern::ObjectPattern(pattern) => pattern
            .properties
            .iter()
            .find_map(|property| {
                property
                    .computed
                    .then(|| property.key.span())
                    .or_else(|| binding_pattern_expression_span(&property.value))
            })
            .or_else(|| {
                pattern
                    .rest
                    .as_ref()
                    .and_then(|rest| binding_pattern_expression_span(&rest.argument))
            }),
        BindingPattern::ArrayPattern(pattern) => pattern
            .elements
            .iter()
            .flatten()
            .find_map(binding_pattern_expression_span)
            .or_else(|| {
                pattern
                    .rest
                    .as_ref()
                    .and_then(|rest| binding_pattern_expression_span(&rest.argument))
            }),
    }
}

fn is_single_statement_parent(kind: AstKind<'_>) -> bool {
    matches!(
        kind,
        AstKind::IfStatement(_)
            | AstKind::LabeledStatement(_)
            | AstKind::DoWhileStatement(_)
            | AstKind::WhileStatement(_)
            | AstKind::ForStatement(_)
            | AstKind::ForInStatement(_)
            | AstKind::ForOfStatement(_)
            | AstKind::WithStatement(_)
    )
}

fn declaration_spans(scoping: &oxc_semantic::Scoping, symbol_id: SymbolId) -> Arc<[Span]> {
    let redeclarations = scoping.symbol_redeclarations(symbol_id);
    let mut spans = if redeclarations.is_empty() {
        vec![scoping.symbol_span(symbol_id)]
    } else {
        redeclarations
            .iter()
            .map(|declaration| declaration.span)
            .collect::<Vec<_>>()
    };
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup();
    spans.into()
}

const fn span_within(inner: Span, outer: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn declaration_kind_invariant(kind: AstKind<'_>) -> &'static str {
    match kind {
        AstKind::BindingIdentifier(_) => "symbol declaration points to declaration owner",
        _ => "supported JavaScript symbol declaration node",
    }
}

fn placement_order(placement: StoragePlacement) -> u8 {
    match placement {
        StoragePlacement::Argument { .. } => 0,
        StoragePlacement::Local => 1,
        StoragePlacement::GlobalObject => 2,
        StoragePlacement::GlobalLexical => 3,
        StoragePlacement::ModuleLocal => 4,
        StoragePlacement::ModuleImport => 5,
    }
}

fn freeze_bindings(
    drafts: Vec<BindingDraft>,
    symbol_count: usize,
) -> Result<FrozenBindings, CompilerError> {
    let mut bindings = Vec::with_capacity(drafts.len());
    let mut symbol_bindings = vec![None; symbol_count];
    let mut source_symbols = Vec::with_capacity(drafts.len());
    let mut declaration_bindings = HashMap::new();
    let mut class_name_bindings = HashMap::new();
    let mut class_field_key_bindings = HashMap::new();
    let mut class_static_receiver_bindings = HashMap::new();
    for (index, draft) in drafts.into_iter().enumerate() {
        let id = u32::try_from(index)
            .map(BindingId)
            .map_err(|_| CompilerError::CapacityExceeded { domain: "bindings" })?;
        if let Some(symbol_id) = draft.symbol_id {
            let span = draft.declaration_spans.first().copied();
            let slot = symbol_bindings.get_mut(symbol_id.index()).ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "semantic symbol index is in range",
                    span,
                },
            )?;
            if draft.primary_symbol_binding && slot.replace(id).is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "one primary compiler binding per semantic symbol",
                    span,
                });
            }
            for declaration in draft.declaration_spans.iter().copied() {
                if declaration_bindings
                    .insert((symbol_id, declaration.start, declaration.end), id)
                    .is_some()
                {
                    return Err(CompilerError::SemanticInvariant {
                        invariant: "one compiler binding per semantic declaration",
                        span: Some(declaration),
                    });
                }
            }
        }
        if let Some(class_node) = draft.class_node
            && class_name_bindings.insert(class_node, id).is_some()
        {
            return Err(CompilerError::SemanticInvariant {
                invariant: "one synthetic class-name binding per class node",
                span: draft.declaration_spans.first().copied(),
            });
        }
        if let Some(field_node) = draft.class_field_node
            && class_field_key_bindings.insert(field_node, id).is_some()
        {
            return Err(CompilerError::SemanticInvariant {
                invariant: "one synthetic class-field key binding per field node",
                span: draft.declaration_spans.first().copied(),
            });
        }
        if let Some(class_node) = draft.class_static_receiver_node
            && class_static_receiver_bindings
                .insert(class_node, id)
                .is_some()
        {
            return Err(CompilerError::SemanticInvariant {
                invariant: "one synthetic class static-receiver binding per class node",
                span: draft.declaration_spans.first().copied(),
            });
        }
        source_symbols.push(draft.symbol_id);
        bindings.push(BindingStorage {
            id,
            executable: draft.executable,
            name: draft.name,
            declaration_spans: draft.declaration_spans,
            placement: draft.placement,
            policy: draft.policy,
            frame_captured: false,
            arguments_object: draft.arguments_object,
        });
    }
    if symbol_bindings.iter().any(Option::is_none) {
        return Err(CompilerError::SemanticInvariant {
            invariant: "every semantic symbol has compiler binding",
            span: None,
        });
    }
    Ok(FrozenBindings {
        bindings,
        primary_by_symbol: symbol_bindings,
        source_symbols,
        by_declaration: declaration_bindings,
        class_name_bindings,
        class_field_key_bindings,
        class_static_receiver_bindings,
    })
}

fn reverse_binding_scopes(
    symbol_bindings: &[Option<BindingId>],
    binding_count: usize,
    scope_count: usize,
    symbols: impl IntoIterator<Item = (SymbolId, ScopeId, Span)>,
) -> Result<Vec<Option<ScopeId>>, CompilerError> {
    let mut scope_by_binding = vec![None; binding_count];
    for (symbol, scope, span) in symbols {
        let binding = symbol_bindings
            .get(symbol.index())
            .copied()
            .flatten()
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "binding-scope semantic symbol index is in range",
                span: Some(span),
            })?;
        if scope.index() >= scope_count {
            return Err(CompilerError::SemanticInvariant {
                invariant: "binding semantic scope index is in range",
                span: Some(span),
            });
        }
        let target =
            scope_by_binding
                .get_mut(binding.index())
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "binding-scope compiler binding index is in range",
                    span: Some(span),
                })?;
        if target.replace(scope).is_some() {
            return Err(CompilerError::SemanticInvariant {
                invariant: "one semantic scope per compiler binding",
                span: Some(span),
            });
        }
    }
    Ok(scope_by_binding)
}

fn plan_frame_captures(
    executables: &[ExecutableDraft],
    bindings: &mut [BindingStorage],
    resolved: &[ResolvedDraft],
    additional: &[CaptureRequest],
) -> Result<Vec<FrameCapture>, CompilerError> {
    let capture_keys = collect_capture_keys(executables, bindings, resolved, additional)?;
    let slots = assign_capture_slots(&capture_keys, bindings)?;
    freeze_frame_captures(executables, bindings, capture_keys, &slots)
}

type CaptureKey = (ExecutableId, BindingId);

fn collect_capture_keys(
    executables: &[ExecutableDraft],
    bindings: &[BindingStorage],
    resolved: &[ResolvedDraft],
    additional: &[CaptureRequest],
) -> Result<Vec<CaptureKey>, CompilerError> {
    let mut capture_keys = HashSet::new();
    for reference in resolved {
        collect_capture_path(
            executables,
            bindings,
            reference.executable,
            reference.binding,
            reference.span,
            &mut capture_keys,
        )?;
    }
    for request in additional {
        collect_capture_path(
            executables,
            bindings,
            request.executable,
            request.binding,
            request.span,
            &mut capture_keys,
        )?;
    }

    let mut capture_keys = capture_keys.into_iter().collect::<Vec<_>>();
    capture_keys.sort_unstable();
    Ok(capture_keys)
}

fn collect_capture_path(
    executables: &[ExecutableDraft],
    bindings: &[BindingStorage],
    executable: ExecutableId,
    binding: BindingId,
    span: Span,
    capture_keys: &mut HashSet<CaptureKey>,
) -> Result<(), CompilerError> {
    let binding_storage =
        bindings
            .get(binding.index())
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "captured compiler binding exists",
                span: Some(span),
            })?;
    if executable == binding_storage.executable
        || !matches!(
            binding_storage.placement,
            StoragePlacement::Argument { .. } | StoragePlacement::Local
        )
    {
        return Ok(());
    }

    let owner = binding_storage.executable;
    let mut current = executable;
    while current != owner {
        if !capture_keys.insert((current, binding)) {
            break;
        }
        let executable =
            executables
                .get(current.index())
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "capturing executable exists",
                    span: Some(span),
                })?;
        let parent = executable
            .executable
            .parent
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "frame binding owner is an executable ancestor",
                span: Some(span),
            })?;
        if parent >= current {
            return Err(CompilerError::SemanticInvariant {
                invariant: "executable parent precedes child",
                span: Some(span),
            });
        }
        current = parent;
    }
    Ok(())
}

fn assign_capture_slots(
    capture_keys: &[CaptureKey],
    bindings: &mut [BindingStorage],
) -> Result<HashMap<CaptureKey, CaptureSlot>, CompilerError> {
    let mut slots = HashMap::with_capacity(capture_keys.len());
    let mut active_executable = None;
    let mut next_slot = 0_u32;
    for &(executable, binding) in capture_keys {
        if active_executable != Some(executable) {
            active_executable = Some(executable);
            next_slot = 0;
        }
        let slot = CaptureSlot(next_slot);
        next_slot = next_slot
            .checked_add(1)
            .ok_or(CompilerError::CapacityExceeded {
                domain: "frame capture slots",
            })?;
        slots.insert((executable, binding), slot);
        let binding_storage =
            bindings
                .get_mut(binding.index())
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "captured compiler binding exists",
                    span: None,
                })?;
        binding_storage.frame_captured = true;
    }
    Ok(slots)
}

fn freeze_frame_captures(
    executables: &[ExecutableDraft],
    bindings: &[BindingStorage],
    capture_keys: Vec<CaptureKey>,
    slots: &HashMap<CaptureKey, CaptureSlot>,
) -> Result<Vec<FrameCapture>, CompilerError> {
    capture_keys
        .into_iter()
        .map(|(executable, binding)| {
            let slot = slots.get(&(executable, binding)).copied().ok_or(
                CompilerError::SemanticInvariant {
                    invariant: "capture slot assigned",
                    span: None,
                },
            )?;
            Ok(FrameCapture {
                executable,
                binding,
                slot,
                source: capture_source(executables, bindings, slots, executable, binding)?,
            })
        })
        .collect()
}

fn capture_source(
    executables: &[ExecutableDraft],
    bindings: &[BindingStorage],
    slots: &HashMap<CaptureKey, CaptureSlot>,
    executable: ExecutableId,
    binding: BindingId,
) -> Result<CaptureSource, CompilerError> {
    let owner = bindings
        .get(binding.index())
        .ok_or(CompilerError::SemanticInvariant {
            invariant: "captured compiler binding exists",
            span: None,
        })?;
    let parent = executables
        .get(executable.index())
        .and_then(|draft| draft.executable.parent)
        .ok_or(CompilerError::SemanticInvariant {
            invariant: "capturing executable has parent",
            span: None,
        })?;
    if parent == owner.executable {
        Ok(CaptureSource::ParentBinding(binding))
    } else {
        slots
            .get(&(parent, binding))
            .copied()
            .map(CaptureSource::ParentCapture)
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "intermediate executable forwards capture",
                span: None,
            })
    }
}

fn freeze_resolved(
    drafts: Vec<ResolvedDraft>,
    reference_by_id: &mut [Option<NativeReferenceId>],
) -> Result<Vec<ResolvedReference>, CompilerError> {
    let mut references = Vec::with_capacity(drafts.len());
    for (index, draft) in drafts.into_iter().enumerate() {
        let id = u32::try_from(index).map(ResolvedReferenceId).map_err(|_| {
            CompilerError::CapacityExceeded {
                domain: "resolved references",
            }
        })?;
        freeze_native_reference(
            reference_by_id,
            draft.reference_id,
            NativeReferenceId::Resolved(id),
            draft.span,
        )?;
        references.push(ResolvedReference {
            id,
            executable: draft.executable,
            binding: draft.binding,
            span: draft.span,
            access: draft.access,
        });
    }
    Ok(references)
}

fn freeze_unresolved(
    drafts: Vec<UnresolvedDraft>,
    reference_by_id: &mut [Option<NativeReferenceId>],
) -> Result<Vec<UnresolvedGlobal>, CompilerError> {
    let mut references = Vec::with_capacity(drafts.len());
    for (index, draft) in drafts.into_iter().enumerate() {
        let id = u32::try_from(index).map(UnresolvedGlobalId).map_err(|_| {
            CompilerError::CapacityExceeded {
                domain: "unresolved globals",
            }
        })?;
        freeze_native_reference(
            reference_by_id,
            draft.reference_id,
            NativeReferenceId::Unresolved(id),
            draft.span,
        )?;
        references.push(UnresolvedGlobal {
            id,
            executable: draft.executable,
            name: draft.name,
            span: draft.span,
            access: draft.access,
        });
    }
    Ok(references)
}

fn freeze_native_reference(
    reference_by_id: &mut [Option<NativeReferenceId>],
    reference_id: ReferenceId,
    native: NativeReferenceId,
    span: Span,
) -> Result<(), CompilerError> {
    let slot =
        reference_by_id
            .get_mut(reference_id.index())
            .ok_or(CompilerError::SemanticInvariant {
                invariant: "semantic reference index is in range",
                span: Some(span),
            })?;
    if slot.replace(native).is_some() {
        return Err(CompilerError::SemanticInvariant {
            invariant: "one compiler identity per semantic reference",
            span: Some(span),
        });
    }
    Ok(())
}

fn assign_ranges(
    executables: &mut [Executable],
    bindings: &[BindingStorage],
    resolved: &[ResolvedReference],
    unresolved: &[UnresolvedGlobal],
    captures: &[FrameCapture],
) -> Result<(), CompilerError> {
    for executable in executables {
        let binding_start = bindings.partition_point(|binding| binding.executable < executable.id);
        let binding_end = bindings.partition_point(|binding| binding.executable <= executable.id);
        executable.binding_start =
            u32::try_from(binding_start).map_err(|_| CompilerError::CapacityExceeded {
                domain: "binding ranges",
            })?;
        executable.binding_end =
            u32::try_from(binding_end).map_err(|_| CompilerError::CapacityExceeded {
                domain: "binding ranges",
            })?;

        let resolved_start =
            resolved.partition_point(|reference| reference.executable < executable.id);
        let resolved_end =
            resolved.partition_point(|reference| reference.executable <= executable.id);
        executable.resolved_start =
            u32::try_from(resolved_start).map_err(|_| CompilerError::CapacityExceeded {
                domain: "resolved-reference ranges",
            })?;
        executable.resolved_end =
            u32::try_from(resolved_end).map_err(|_| CompilerError::CapacityExceeded {
                domain: "resolved-reference ranges",
            })?;

        let unresolved_start =
            unresolved.partition_point(|reference| reference.executable < executable.id);
        let unresolved_end =
            unresolved.partition_point(|reference| reference.executable <= executable.id);
        executable.unresolved_start =
            u32::try_from(unresolved_start).map_err(|_| CompilerError::CapacityExceeded {
                domain: "unresolved-global ranges",
            })?;
        executable.unresolved_end =
            u32::try_from(unresolved_end).map_err(|_| CompilerError::CapacityExceeded {
                domain: "unresolved-global ranges",
            })?;

        let capture_start = captures.partition_point(|capture| capture.executable < executable.id);
        let capture_end = captures.partition_point(|capture| capture.executable <= executable.id);
        executable.capture_start =
            u32::try_from(capture_start).map_err(|_| CompilerError::CapacityExceeded {
                domain: "frame-capture ranges",
            })?;
        executable.capture_end =
            u32::try_from(capture_end).map_err(|_| CompilerError::CapacityExceeded {
                domain: "frame-capture ranges",
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_scope_reverse_map_uses_dense_binding_identity() {
        let scopes = reverse_binding_scopes(
            &[Some(BindingId(1)), Some(BindingId(0))],
            2,
            3,
            [
                (SymbolId::new(0), ScopeId::new(2), Span::new(0, 1)),
                (SymbolId::new(1), ScopeId::new(1), Span::new(2, 3)),
            ],
        )
        .expect("reverse scope map");

        assert_eq!(scopes, [Some(ScopeId::new(1)), Some(ScopeId::new(2))]);
    }

    #[test]
    fn binding_scope_reverse_map_rejects_duplicate_binding_identity() {
        let error = reverse_binding_scopes(
            &[Some(BindingId(0)), Some(BindingId(0))],
            1,
            2,
            [
                (SymbolId::new(0), ScopeId::new(0), Span::new(0, 1)),
                (SymbolId::new(1), ScopeId::new(1), Span::new(2, 3)),
            ],
        )
        .expect_err("duplicate binding scope");

        assert!(matches!(
            error,
            CompilerError::SemanticInvariant {
                invariant: "one semantic scope per compiler binding",
                span: Some(span),
            } if span == Span::new(2, 3)
        ));
    }

    #[test]
    fn binding_scope_reverse_map_rejects_out_of_range_identities() {
        let binding_error = reverse_binding_scopes(
            &[Some(BindingId(1))],
            1,
            1,
            [(SymbolId::new(0), ScopeId::new(0), Span::new(0, 1))],
        )
        .expect_err("out-of-range binding");
        assert!(matches!(
            binding_error,
            CompilerError::SemanticInvariant {
                invariant: "binding-scope compiler binding index is in range",
                ..
            }
        ));

        let scope_error = reverse_binding_scopes(
            &[Some(BindingId(0))],
            1,
            1,
            [(SymbolId::new(0), ScopeId::new(1), Span::new(0, 1))],
        )
        .expect_err("out-of-range scope");
        assert!(matches!(
            scope_error,
            CompilerError::SemanticInvariant {
                invariant: "binding semantic scope index is in range",
                ..
            }
        ));
    }
}
