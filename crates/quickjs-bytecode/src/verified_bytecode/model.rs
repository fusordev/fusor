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
    /// An ECMAScript Module root body.
    ///
    /// The module root owns a module environment of cells materialized by the
    /// runtime linker; its top-level bindings are module-local or imported
    /// cells and never touch the realm global environment.
    Module,
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

/// One static module request retained for the runtime linker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleRequestDescriptor {
    specifier: AtomPoolIndex,
    has_assertions: bool,
}

impl ModuleRequestDescriptor {
    /// Creates one module request descriptor.
    #[must_use]
    pub const fn new(specifier: AtomPoolIndex, has_assertions: bool) -> Self {
        Self {
            specifier,
            has_assertions,
        }
    }

    /// Returns the module specifier atom.
    #[must_use]
    pub const fn specifier(self) -> AtomPoolIndex {
        self.specifier
    }

    /// Returns whether the request carries import attributes.
    #[must_use]
    pub const fn has_assertions(self) -> bool {
        self.has_assertions
    }
}

/// The origin category of one module-environment binding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModuleBindingOrigin {
    /// A module-local declaration cell.
    Local,
    /// A named or default live import cell.
    Import,
    /// A namespace import cell holding a module namespace object.
    Namespace,
}

/// The import-side name carried by a module import binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleImportName {
    request: u32,
    kind: ModuleImportNameKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ModuleImportNameKind {
    Named(AtomPoolIndex),
    Default,
    Namespace,
}

impl ModuleImportName {
    /// Creates a named import binding descriptor.
    #[must_use]
    pub const fn named(request: u32, name: AtomPoolIndex) -> Self {
        Self {
            request,
            kind: ModuleImportNameKind::Named(name),
        }
    }

    /// Creates a default import binding descriptor.
    #[must_use]
    pub const fn default(request: u32) -> Self {
        Self {
            request,
            kind: ModuleImportNameKind::Default,
        }
    }

    /// Creates a namespace import binding descriptor.
    #[must_use]
    pub const fn namespace(request: u32) -> Self {
        Self {
            request,
            kind: ModuleImportNameKind::Namespace,
        }
    }

    /// Returns the static module request index.
    #[must_use]
    pub const fn request(&self) -> u32 {
        self.request
    }

    /// Returns the named imported export atom, when present.
    #[must_use]
    pub const fn named_atom(&self) -> Option<AtomPoolIndex> {
        match self.kind {
            ModuleImportNameKind::Named(atom) => Some(atom),
            _ => None,
        }
    }

    /// Returns whether this is a default import.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        matches!(self.kind, ModuleImportNameKind::Default)
    }

    /// Returns whether this is a namespace import.
    #[must_use]
    pub const fn is_namespace(&self) -> bool {
        matches!(self.kind, ModuleImportNameKind::Namespace)
    }
}

/// One module-environment binding descriptor awaiting final verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedModuleBindingDescriptor {
    name: AtomPoolIndex,
    slot: u32,
    policy: CompilerBindingPolicy,
    origin: ModuleBindingOrigin,
    initializer: Option<u32>,
    import: Option<ModuleImportName>,
}

impl UnverifiedModuleBindingDescriptor {
    /// Creates one unverified module binding descriptor.
    #[must_use]
    pub const fn new(
        name: AtomPoolIndex,
        slot: u32,
        policy: CompilerBindingPolicy,
        origin: ModuleBindingOrigin,
    ) -> Self {
        Self {
            name,
            slot,
            policy,
            origin,
            initializer: None,
            import: None,
        }
    }

    /// Attaches the function-template constant that initializes a hoisted
    /// module-level function declaration at instantiation.
    #[must_use]
    pub const fn with_initializer(mut self, constant: u32) -> Self {
        self.initializer = Some(constant);
        self
    }

    /// Attaches the import-side name for an imported binding.
    #[must_use]
    pub const fn with_import(mut self, import: ModuleImportName) -> Self {
        self.import = Some(import);
        self
    }
}

/// Complete module instantiation metadata awaiting final verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedModuleDeclarationRecord {
    bindings: Arc<[UnverifiedModuleBindingDescriptor]>,
    requests: Arc<[ModuleRequestDescriptor]>,
}

impl UnverifiedModuleDeclarationRecord {
    /// Creates unverified module instantiation metadata.
    #[must_use]
    pub const fn new(
        bindings: Arc<[UnverifiedModuleBindingDescriptor]>,
        requests: Arc<[ModuleRequestDescriptor]>,
    ) -> Self {
        Self { bindings, requests }
    }
}

/// One verified module-environment binding descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleBindingDescriptor {
    name: AtomPoolIndex,
    slot: u32,
    policy: CompilerBindingPolicy,
    origin: ModuleBindingOrigin,
    initializer: Option<u32>,
    import: Option<ModuleImportName>,
}

impl ModuleBindingDescriptor {
    /// Returns the binding-name atom in the module root's atom pool.
    #[must_use]
    pub const fn name(&self) -> AtomPoolIndex {
        self.name
    }

    /// Returns the closure-domain slot index in the module root.
    #[must_use]
    pub const fn slot(&self) -> u32 {
        self.slot
    }

    /// Returns the verified declaration policy.
    #[must_use]
    pub const fn policy(&self) -> CompilerBindingPolicy {
        self.policy
    }

    /// Returns the module binding origin category.
    #[must_use]
    pub const fn origin(&self) -> ModuleBindingOrigin {
        self.origin
    }

    /// Returns the function-template constant for a hoisted function, when
    /// present.
    #[must_use]
    pub const fn initializer(&self) -> Option<u32> {
        self.initializer
    }

    /// Returns the import-side name for an imported binding.
    #[must_use]
    pub const fn import(&self) -> Option<&ModuleImportName> {
        self.import.as_ref()
    }
}

/// Verified module instantiation metadata retained for the runtime linker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleDeclarationRecord {
    bindings: Arc<[ModuleBindingDescriptor]>,
    requests: Arc<[ModuleRequestDescriptor]>,
}

impl ModuleDeclarationRecord {
    /// Returns the verified module-environment binding descriptors in
    /// declaration order.
    #[must_use]
    pub fn bindings(&self) -> &[ModuleBindingDescriptor] {
        &self.bindings
    }

    /// Returns the static module requests in source order.
    #[must_use]
    pub fn requests(&self) -> &[ModuleRequestDescriptor] {
        &self.requests
    }
}

/// A staged compiler graph paired with complete parallel metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedCompilerBytecodeGraph {
    graph: Arc<VerifiedCompilerFunctionGraph>,
    metadata: Arc<[UnverifiedFunctionMetadata]>,
    module: Option<Arc<UnverifiedModuleDeclarationRecord>>,
}

impl UnverifiedCompilerBytecodeGraph {
    /// Creates final-verifier input.
    #[must_use]
    pub const fn new(
        graph: Arc<VerifiedCompilerFunctionGraph>,
        metadata: Arc<[UnverifiedFunctionMetadata]>,
    ) -> Self {
        Self {
            graph,
            metadata,
            module: None,
        }
    }

    /// Attaches the module instantiation metadata required by a Module root.
    #[must_use]
    pub fn with_module(
        mut self,
        module: Arc<UnverifiedModuleDeclarationRecord>,
    ) -> Self {
        self.module = Some(module);
        self
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
    /// Module-environment cells materialized and linked by the module linker.
    ModuleBindings,
    /// `in` or `instanceof` object semantics.
    ObjectOperators,
    /// Full dynamic coercion and mixed-type operator semantics.
    DynamicOperators,
}

/// Number of conservative runtime implementation families selectable by the
/// whole-graph compiler authority.
pub const EXECUTION_REQUIREMENT_COUNT: usize = 16;

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
        ExecutionRequirement::ModuleBindings => 13,
        ExecutionRequirement::ObjectOperators => 14,
        ExecutionRequirement::DynamicOperators => 15,
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
    module: Option<Arc<ModuleDeclarationRecord>>,
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

    /// Returns the verified module instantiation metadata, when the root is a
    /// Module executable.
    #[must_use]
    pub fn module(&self) -> Option<&ModuleDeclarationRecord> {
        self.module.as_deref()
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
