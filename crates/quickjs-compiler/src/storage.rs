use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    sync::Arc,
};

use oxc_ast::{
    AstKind,
    ast::{
        BindingPattern, ExportDefaultDeclarationKind, FunctionType, Statement,
        VariableDeclarationKind,
    },
};
use oxc_semantic::{NodeId, ReferenceId, ScopeId, SymbolFlags, SymbolId};
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
    /// ordinary dynamic-Function wrapper Script.
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
    simple_parameter_list: bool,
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

    /// Returns whether every formal parameter is a plain binding identifier.
    #[must_use]
    pub const fn has_simple_parameter_list(&self) -> bool {
        self.simple_parameter_list
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
    /// A nonordinary dynamic-function constructor family.
    DynamicFunctionKind(DynamicFunctionKind),
    /// A syntactic bare `eval(...)` call.
    DirectEval,
    /// A `with` statement.
    WithStatement,
    /// A parameter initializer, destructuring default, or computed pattern key.
    ParameterExpressions,
    /// A formal rest parameter.
    NonSimpleParameters,
    /// A non-simple parameter binding merged with a body function declaration.
    ParameterFunctionRedeclaration,
    /// Annex B's paired block-lexical and var-like function binding.
    AnnexBBlockFunction,
    /// Class-created functions, private names, and synthetic slots.
    ClassSyntheticSlots,
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
    simple: bool,
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
    executable: ExecutableId,
    name: Arc<str>,
    declaration_spans: Arc<[Span]>,
    placement: StoragePlacement,
    policy: DeclarationPolicy,
    arguments_object: bool,
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
            CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) => (
                CompilationUnitKind::Script,
                ExecutableKind::Script {
                    asynchronous: false,
                },
            ),
            CompilationGoal::DynamicFunction(kind) => {
                return Err(CompilerError::Unsupported {
                    feature: UnsupportedFeature::DynamicFunctionKind(kind),
                    span: root_span,
                });
            }
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
        })
    }

    fn build(mut self) -> Result<PlannedStorage, CompilerError> {
        self.reject_preflight_features()?;
        self.inventory_executables()?;
        self.assign_scope_owners()?;
        self.reject_synthetic_binding_uses()?;

        let mut binding_drafts = self.binding_drafts()?;
        let implicit_arguments_references = self.add_arguments_bindings(&mut binding_drafts)?;
        self.add_synthetic_default_binding(&mut binding_drafts)?;
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
        let (mut bindings, symbol_bindings) =
            freeze_bindings(binding_drafts, self.unit.semantic().scoping().symbols_len())?;
        let scope_by_binding = self.binding_scope_map(&symbol_bindings, &bindings)?;

        let mut resolved_drafts =
            self.resolved_drafts(&symbol_bindings, &bindings, &implicit_arguments_references)?;
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
        let frame_captures =
            plan_frame_captures(&self.executable_drafts, &mut bindings, &resolved_drafts)?;

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
                scope_by_binding: scope_by_binding.into_boxed_slice(),
                reference_by_id: reference_by_id.into_boxed_slice(),
            },
        })
    }

    fn binding_scope_map(
        &self,
        symbol_bindings: &[Option<BindingId>],
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
        Ok(scopes)
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
                AstKind::Class(class) => {
                    return unsupported(UnsupportedFeature::ClassSyntheticSlots, class.span);
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

    fn inventory_executables(&mut self) -> Result<(), CompilerError> {
        let semantic = self.unit.semantic();
        let nodes = semantic.nodes();
        for (node_id, node) in nodes.iter_enumerated() {
            let (kind, scope_id, span, name, name_span, parameters) = match node.kind() {
                AstKind::Program(program) => (
                    self.root_executable_kind,
                    program.scope_id(),
                    program.span,
                    None,
                    None,
                    ParameterLayout::default(),
                ),
                AstKind::Function(function) => {
                    let parameters = self.validate_parameters(function.params.as_ref())?;
                    (
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
                    )
                }
                AstKind::ArrowFunctionExpression(arrow) => {
                    let parameters = self.validate_parameters(arrow.params.as_ref())?;
                    (
                        ExecutableKind::Arrow {
                            asynchronous: arrow.r#async,
                        },
                        arrow.scope_id(),
                        arrow.span,
                        None,
                        None,
                        parameters,
                    )
                }
                _ => continue,
            };

            let id = executable_id(self.executable_drafts.len())?;
            let parent = if matches!(kind, ExecutableKind::Script { .. } | ExecutableKind::Module) {
                None
            } else {
                nodes
                    .ancestor_ids(node_id)
                    .find_map(|ancestor| self.node_executables[ancestor.index()])
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "executable parent",
                        span: Some(span),
                    })?
                    .into()
            };
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
                simple_parameter_list: parameters.simple,
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
            self.node_executables[node_id.index()] = Some(id);
            self.exact_scope_executables[scope_id.index()] = Some(id);
            self.executable_drafts.push(ExecutableDraft {
                executable,
                node_id,
                scope_id,
            });
        }

        self.validate_root_executable()
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

    fn validate_parameters(
        &mut self,
        parameters: &oxc_ast::ast::FormalParameters<'arena>,
    ) -> Result<ParameterLayout, CompilerError> {
        if let Some(rest) = &parameters.rest {
            return unsupported(UnsupportedFeature::NonSimpleParameters, rest.span);
        }
        let executable = executable_id(self.executable_drafts.len())?;
        let simple = parameters.items.iter().all(|parameter| {
            parameter.initializer.is_none()
                && matches!(parameter.pattern, BindingPattern::BindingIdentifier(_))
        });
        for (index, parameter) in parameters.items.iter().enumerate() {
            if parameter.initializer.is_some() {
                return unsupported(UnsupportedFeature::ParameterExpressions, parameter.span);
            }
            if let Some(span) = binding_pattern_expression_span(&parameter.pattern) {
                return unsupported(UnsupportedFeature::ParameterExpressions, span);
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
        if !simple {
            return Ok(ParameterLayout {
                count,
                simple,
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
            simple,
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

    fn reject_synthetic_binding_uses(&self) -> Result<(), CompilerError> {
        for node in self.unit.semantic().nodes().iter() {
            let span = match node.kind() {
                AstKind::NewTarget(expression) => expression.span,
                AstKind::Super(expression) => expression.span,
                _ => continue,
            };
            let owner = self.scope_owner(node.scope_id(), Some(span))?;
            if owner != ExecutableId(0) {
                return unsupported(UnsupportedFeature::FunctionSyntheticBinding, span);
            }
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
            let kind = facts
                .effective_kind()
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "known JavaScript declaration kind",
                    span: Some(scoping.symbol_span(symbol_id)),
                })?;
            let owner = self.scope_owner(
                scoping.symbol_scope_id(symbol_id),
                Some(scoping.symbol_span(symbol_id)),
            )?;
            if facts.contains(DeclarationFacts::PARAMETER)
                && facts.contains(DeclarationFacts::FUNCTION)
                && !self
                    .executable_drafts
                    .get(owner.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "parameter binding owner exists",
                        span: Some(scoping.symbol_span(symbol_id)),
                    })?
                    .executable
                    .has_simple_parameter_list()
            {
                return unsupported(
                    UnsupportedFeature::ParameterFunctionRedeclaration,
                    scoping.symbol_span(symbol_id),
                );
            }
            let name = scoping.symbol_name(symbol_id);
            let placement = self.placement(symbol_id, owner, kind)?;
            let policy = self.declaration_policy(owner, kind, facts.function_scope_entry);
            let declaration_spans = declaration_spans(scoping, symbol_id);
            drafts.push(BindingDraft {
                symbol_id: Some(symbol_id),
                executable: owner,
                name: Arc::from(name),
                declaration_spans,
                placement,
                policy,
                arguments_object: false,
            });
        }
        Ok(drafts)
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
                AstKind::CatchParameter(_) => facts.insert(DeclarationFacts::CATCH),
                AstKind::ImportSpecifier(_) | AstKind::ImportDefaultSpecifier(_) => {
                    facts.insert(DeclarationFacts::IMPORT);
                }
                AstKind::ImportNamespaceSpecifier(_) => {
                    facts.insert(DeclarationFacts::NAMESPACE_IMPORT);
                }
                AstKind::Class(class) => {
                    return unsupported(UnsupportedFeature::ClassSyntheticSlots, class.span);
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
                DeclarationKind::Let | DeclarationKind::Const
                    if self.unit.goal()
                        == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) =>
                {
                    Ok(StoragePlacement::Local)
                }
                DeclarationKind::Let | DeclarationKind::Const => {
                    Ok(StoragePlacement::GlobalLexical)
                }
                DeclarationKind::Var | DeclarationKind::Function => {
                    Ok(StoragePlacement::GlobalObject)
                }
                DeclarationKind::FunctionName
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
                | DeclarationKind::Function => Ok(StoragePlacement::ModuleLocal),
                DeclarationKind::FunctionName
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
            DeclarationKind::Parameter => {
                (InitializationPolicy::Argument, WritePolicy::Mutable, false)
            }
            DeclarationKind::Var => (
                InitializationPolicy::UndefinedAtInstantiation,
                WritePolicy::Mutable,
                false,
            ),
            DeclarationKind::Let => (
                InitializationPolicy::AtDeclaration,
                WritePolicy::Mutable,
                true,
            ),
            DeclarationKind::Const => (
                InitializationPolicy::AtDeclaration,
                WritePolicy::Immutable,
                true,
            ),
            DeclarationKind::Function => (
                if function_scope_entry {
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
            executable: ExecutableId(0),
            name: Arc::from("*default*"),
            declaration_spans: synthetic_spans.into(),
            placement: StoragePlacement::ModuleLocal,
            policy,
            arguments_object: false,
        });
        Ok(())
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
                unsupported(UnsupportedFeature::ClassSyntheticSlots, class.span)
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
                let executable = self.scope_owner(reference.scope_id(), Some(span))?;
                let binding = if let Some(owner) =
                    implicit_arguments_references.get(&reference_id).copied()
                {
                    arguments_bindings.get(&owner).copied().ok_or(
                        CompilerError::SemanticInvariant {
                            invariant: "implicit arguments reference has an owned binding",
                            span: Some(span),
                        },
                    )?
                } else {
                    source_binding
                };
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
            let executable = self.scope_owner(reference.scope_id(), Some(span))?;
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
        let mut binding_by_symbol = vec![None; scoping.symbols_len()];
        for (index, binding) in bindings.iter().enumerate() {
            let Some(symbol) = binding.symbol_id else {
                continue;
            };
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
                if self
                    .reference_uses_explicit_arguments_binding(executable, owner, binding, span)?
                {
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
        Ok(ImplicitArgumentsPlan {
            reference_owners: implicit_references,
            first_references,
        })
    }

    fn reference_uses_explicit_arguments_binding(
        &self,
        reference_executable: ExecutableId,
        arguments_owner: ExecutableId,
        binding: &BindingDraft,
        span: Span,
    ) -> Result<bool, CompilerError> {
        if binding.executable == arguments_owner {
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
                ExecutableKind::Script { .. } | ExecutableKind::Module => return Ok(None),
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
) -> Result<(Vec<BindingStorage>, Vec<Option<BindingId>>), CompilerError> {
    let mut bindings = Vec::with_capacity(drafts.len());
    let mut symbol_bindings = vec![None; symbol_count];
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
            if slot.replace(id).is_some() {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "one compiler binding per semantic symbol",
                    span,
                });
            }
        }
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
    Ok((bindings, symbol_bindings))
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
) -> Result<Vec<FrameCapture>, CompilerError> {
    let capture_keys = collect_capture_keys(executables, bindings, resolved)?;
    let slots = assign_capture_slots(&capture_keys, bindings)?;
    freeze_frame_captures(executables, bindings, capture_keys, &slots)
}

type CaptureKey = (ExecutableId, BindingId);

fn collect_capture_keys(
    executables: &[ExecutableDraft],
    bindings: &[BindingStorage],
    resolved: &[ResolvedDraft],
) -> Result<Vec<CaptureKey>, CompilerError> {
    let mut capture_keys = HashSet::new();
    for reference in resolved {
        let binding =
            bindings
                .get(reference.binding.index())
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "resolved compiler binding exists",
                    span: Some(reference.span),
                })?;
        if reference.executable == binding.executable
            || !matches!(
                binding.placement,
                StoragePlacement::Argument { .. } | StoragePlacement::Local
            )
        {
            continue;
        }

        let owner = binding.executable;
        let mut current = reference.executable;
        while current != owner {
            if !capture_keys.insert((current, reference.binding)) {
                break;
            }
            let executable =
                executables
                    .get(current.index())
                    .ok_or(CompilerError::SemanticInvariant {
                        invariant: "capturing executable exists",
                        span: Some(reference.span),
                    })?;
            let parent = executable
                .executable
                .parent
                .ok_or(CompilerError::SemanticInvariant {
                    invariant: "frame binding owner is an executable ancestor",
                    span: Some(reference.span),
                })?;
            if parent >= current {
                return Err(CompilerError::SemanticInvariant {
                    invariant: "executable parent precedes child",
                    span: Some(reference.span),
                });
            }
            current = parent;
        }
    }

    let mut capture_keys = capture_keys.into_iter().collect::<Vec<_>>();
    capture_keys.sort_unstable();
    Ok(capture_keys)
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
