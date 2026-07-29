//! Oxc-backed JavaScript parsing and ECMAScript early-error validation.
//!
//! This module is the reusable source boundary for the compiler crate.
//!
//! Regular-expression pattern parsing is deliberately disabled. Oxc identifies
//! literal boundaries and flags, while the QuickJS-compatible `RegExp` layer owns
//! pattern semantics.

use std::{error::Error, fmt};

pub use oxc_allocator::Allocator;
pub use oxc_ast::ast::Program;
use oxc_ast::{
    AstKind,
    ast::{ImportPhase, VariableDeclarationKind, WithClauseKeyword},
};
use oxc_diagnostics::{Diagnostics, OxcDiagnostic};
use oxc_parser::{ParseOptions as OxcParseOptions, Parser};
use oxc_semantic::{AstNodes, SemanticBuilder};
use oxc_span::SourceType;
pub use oxc_span::Span;
use quickjs_diagnostics::{
    Diagnostic as SharedDiagnostic, DiagnosticCode as SharedDiagnosticCode, DiagnosticCodeError,
    DiagnosticLabel as SharedDiagnosticLabel, DiagnosticSeverity, SourceError, SourceId, SourceMap,
    SourceRegistry,
};

/// The default maximum UTF-8 source size accepted by one front-end entry.
///
/// Hosts can select a different ceiling with [`FrontendLimits`].
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

const MAX_OXC_SOURCE_BYTES: usize = {
    let span_limit = u32::MAX as usize;
    let slice_limit = isize::MAX.unsigned_abs();
    if span_limit < slice_limit {
        span_limit
    } else {
        slice_limit
    }
};

/// Oxc's underlying ECMAScript source mode.
///
/// This compatibility type remains useful for ordinary Script and Module
/// callers. Context-sensitive engine entry points should use
/// [`CompilationGoal`] instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseMode {
    /// Parse a Script, where module declarations and top-level `await` are
    /// rejected.
    Script,
    /// Parse an ECMAScript Module, with implicit strict mode.
    Module,
}

impl ParseMode {
    const fn source_type(self) -> SourceType {
        match self {
            Self::Script => SourceType::script(),
            Self::Module => SourceType::mjs(),
        }
        .with_standard(true)
    }
}

/// Options for parsing a global Script.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GlobalScriptGoal {
    force_strict: bool,
    allow_top_level_await: bool,
}

impl GlobalScriptGoal {
    /// Creates the ordinary, non-forced-strict Script goal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            force_strict: false,
            allow_top_level_await: false,
        }
    }

    /// Selects whether the host forces strict mode independently of source
    /// directives.
    #[must_use]
    pub const fn with_forced_strict(mut self, yes: bool) -> Self {
        self.force_strict = yes;
        self
    }

    /// Selects whether the host admits top-level `await` in a Script.
    #[must_use]
    pub const fn with_top_level_await(mut self, yes: bool) -> Self {
        self.allow_top_level_await = yes;
        self
    }

    /// Returns whether the host forces strict mode.
    #[must_use]
    pub const fn forces_strict(self) -> bool {
        self.force_strict
    }

    /// Returns whether top-level `await` is admitted.
    #[must_use]
    pub const fn allows_top_level_await(self) -> bool {
        self.allow_top_level_await
    }
}

/// Options for an indirect `eval` parse.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndirectEvalGoal {
    force_strict: bool,
}

impl IndirectEvalGoal {
    /// Creates an indirect-eval goal whose strictness comes from its source.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            force_strict: false,
        }
    }

    /// Selects whether the host forces strict mode independently of source
    /// directives.
    #[must_use]
    pub const fn with_forced_strict(mut self, yes: bool) -> Self {
        self.force_strict = yes;
        self
    }

    /// Returns whether the host forces strict mode.
    #[must_use]
    pub const fn forces_strict(self) -> bool {
        self.force_strict
    }
}

/// Syntax capabilities inherited by a direct `eval` from its caller.
///
/// These flags mirror caller context, not permissive parser switches. The
/// direct-eval implementation must use them together with
/// [`DirectEvalScopeSnapshot`] when it builds its contextual parse.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DirectEvalCapabilities {
    bits: u8,
}

impl DirectEvalCapabilities {
    const STRICT: u8 = 1 << 0;
    const NEW_TARGET: u8 = 1 << 1;
    const SUPER_PROPERTY: u8 = 1 << 2;
    const SUPER_CALL: u8 = 1 << 3;
    const ARGUMENTS_ALLOWED: u8 = 1 << 4;

    /// Creates a context with no inherited capabilities.
    #[must_use]
    pub const fn new() -> Self {
        Self { bits: 0 }
    }

    /// Selects whether the eval source is forced strict by its caller.
    #[must_use]
    pub const fn with_strict(mut self, yes: bool) -> Self {
        self.set(Self::STRICT, yes);
        self
    }

    /// Selects whether `new.target` is meaningful in the caller.
    #[must_use]
    pub const fn with_new_target(mut self, yes: bool) -> Self {
        self.set(Self::NEW_TARGET, yes);
        self
    }

    /// Selects whether the caller admits `super` property access.
    #[must_use]
    pub const fn with_super_property(mut self, yes: bool) -> Self {
        self.set(Self::SUPER_PROPERTY, yes);
        self
    }

    /// Selects whether the caller admits a direct `super()` call.
    #[must_use]
    pub const fn with_super_call(mut self, yes: bool) -> Self {
        self.set(Self::SUPER_CALL, yes);
        self
    }

    /// Selects whether the `arguments` identifier is syntactically allowed.
    ///
    /// This is a grammar capability inherited from the caller. It does not
    /// assert that an `arguments` binding is present in any scope frame.
    #[must_use]
    pub const fn with_arguments_allowed(mut self, yes: bool) -> Self {
        self.set(Self::ARGUMENTS_ALLOWED, yes);
        self
    }

    /// Returns whether the eval source is forced strict by its caller.
    #[must_use]
    pub const fn is_strict(self) -> bool {
        self.contains(Self::STRICT)
    }

    /// Returns whether `new.target` is meaningful in the caller.
    #[must_use]
    pub const fn allows_new_target(self) -> bool {
        self.contains(Self::NEW_TARGET)
    }

    /// Returns whether the caller admits `super` property access.
    #[must_use]
    pub const fn allows_super_property(self) -> bool {
        self.contains(Self::SUPER_PROPERTY)
    }

    /// Returns whether the caller admits a direct `super()` call.
    #[must_use]
    pub const fn allows_super_call(self) -> bool {
        self.contains(Self::SUPER_CALL)
    }

    /// Returns whether the `arguments` identifier is syntactically allowed.
    ///
    /// Binding presence is represented independently by the scope snapshot.
    #[must_use]
    pub const fn allows_arguments(self) -> bool {
        self.contains(Self::ARGUMENTS_ALLOWED)
    }

    const fn set(&mut self, flag: u8, yes: bool) {
        if yes {
            self.bits |= flag;
        } else {
            self.bits &= !flag;
        }
    }

    const fn contains(self, flag: u8) -> bool {
        self.bits & flag != 0
    }
}

/// The semantic declaration kind of a binding visible to direct `eval`.
///
/// Storage is deliberately represented by [`DirectEvalBindingLocation`]
/// instead of being folded into this enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DirectEvalBindingKind {
    /// An ordinary declaration.
    Normal,
    /// A lexical function declaration.
    FunctionDeclaration,
    /// An async or generator lexical function declaration.
    NewFunctionDeclaration,
    /// A catch binding.
    Catch,
    /// A named function-expression binding.
    FunctionName,
    /// A global function declaration.
    GlobalFunctionDeclaration,
}

/// The storage location of a binding visible to direct `eval`.
///
/// The index is retained independently of declaration semantics, lexicality,
/// and constness. The variants cover every closure-storage category used by
/// the pinned `QuickJS` release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DirectEvalBindingLocation {
    /// An argument slot in the calling function.
    Argument {
        /// Zero-based argument slot.
        index: u16,
    },
    /// A local slot in the calling function.
    Local {
        /// Zero-based local slot.
        index: u16,
    },
    /// A closure slot inherited from the calling function.
    Closure {
        /// Zero-based closure slot.
        index: u16,
    },
    /// A closure slot referencing a global variable.
    GlobalReference {
        /// Zero-based closure slot.
        index: u16,
    },
    /// A global declaration introduced by eval code.
    GlobalDeclaration {
        /// Zero-based global declaration slot.
        index: u16,
    },
    /// A global variable used by eval code.
    Global {
        /// Zero-based global slot.
        index: u16,
    },
    /// A module-local declaration.
    ModuleDeclaration {
        /// Zero-based module declaration slot.
        index: u16,
    },
    /// A module import binding.
    ModuleImport {
        /// Zero-based module import slot.
        index: u16,
    },
}

/// An ordinary binding visible in one direct-eval scope frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectEvalBinding<'scope> {
    name: &'scope str,
    kind: DirectEvalBindingKind,
    is_lexical: bool,
    is_const: bool,
    location: DirectEvalBindingLocation,
}

impl<'scope> DirectEvalBinding<'scope> {
    /// Creates a lossless binding snapshot.
    ///
    /// `kind`, `is_lexical`, and `is_const` retain independent semantic
    /// metadata. `location` identifies storage without changing those
    /// semantics.
    #[must_use]
    pub const fn new(
        name: &'scope str,
        kind: DirectEvalBindingKind,
        is_lexical: bool,
        is_const: bool,
        location: DirectEvalBindingLocation,
    ) -> Self {
        Self {
            name,
            kind,
            is_lexical,
            is_const,
            location,
        }
    }

    /// Returns the JavaScript binding name.
    #[must_use]
    pub const fn name(self) -> &'scope str {
        self.name
    }

    /// Returns the binding role.
    #[must_use]
    pub const fn kind(self) -> DirectEvalBindingKind {
        self.kind
    }

    /// Returns whether this is a lexical binding.
    #[must_use]
    pub const fn is_lexical(self) -> bool {
        self.is_lexical
    }

    /// Returns whether writes to this binding are forbidden.
    #[must_use]
    pub const fn is_const(self) -> bool {
        self.is_const
    }

    /// Returns the independent storage location.
    #[must_use]
    pub const fn location(self) -> DirectEvalBindingLocation {
        self.location
    }
}

/// The role of a private name visible to direct `eval`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DirectEvalPrivateNameKind {
    /// A private field.
    Field,
    /// A private method.
    Method,
    /// A private getter.
    Getter,
    /// A private setter.
    Setter,
    /// A combined private getter/setter pair.
    GetterSetter,
}

/// A private name visible in one direct-eval scope frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectEvalPrivateName<'scope> {
    name: &'scope str,
    kind: DirectEvalPrivateNameKind,
    is_static: bool,
    is_lexical: bool,
    is_const: bool,
    location: DirectEvalBindingLocation,
}

impl<'scope> DirectEvalPrivateName<'scope> {
    /// Creates a private-name snapshot without discarding storage metadata.
    #[must_use]
    pub const fn new(
        name: &'scope str,
        kind: DirectEvalPrivateNameKind,
        is_static: bool,
        is_lexical: bool,
        is_const: bool,
        location: DirectEvalBindingLocation,
    ) -> Self {
        Self {
            name,
            kind,
            is_static,
            is_lexical,
            is_const,
            location,
        }
    }

    /// Returns the private name, without a leading `#`.
    #[must_use]
    pub const fn name(self) -> &'scope str {
        self.name
    }

    /// Returns the private-name role.
    #[must_use]
    pub const fn kind(self) -> DirectEvalPrivateNameKind {
        self.kind
    }

    /// Returns whether the name belongs to the static class context.
    #[must_use]
    pub const fn is_static(self) -> bool {
        self.is_static
    }

    /// Returns whether this private name is lexical.
    #[must_use]
    pub const fn is_lexical(self) -> bool {
        self.is_lexical
    }

    /// Returns whether writes to this private-name binding are forbidden.
    #[must_use]
    pub const fn is_const(self) -> bool {
        self.is_const
    }

    /// Returns the private name's independent storage location.
    #[must_use]
    pub const fn location(self) -> DirectEvalBindingLocation {
        self.location
    }
}

/// The role of one scope frame visible to direct `eval`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DirectEvalScopeKind {
    /// The global lexical environment.
    Global,
    /// A function's parameter environment.
    FunctionParameters,
    /// A function's body environment.
    FunctionBody,
    /// A lexical block.
    Block,
    /// A `catch` parameter/body environment.
    Catch,
    /// A class private-name environment.
    Class,
    /// A dynamically resolved `with` environment.
    With,
    /// An ECMAScript module environment.
    Module,
    /// A dynamically resolved caller environment object.
    Dynamic,
    /// A compiler-created pseudo environment.
    Pseudo,
}

/// One lexical frame visible to direct `eval`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectEvalScopeFrame<'scope> {
    kind: DirectEvalScopeKind,
    bindings: &'scope [DirectEvalBinding<'scope>],
    private_names: &'scope [DirectEvalPrivateName<'scope>],
}

impl<'scope> DirectEvalScopeFrame<'scope> {
    /// Creates a scope frame.
    #[must_use]
    pub const fn new(
        kind: DirectEvalScopeKind,
        bindings: &'scope [DirectEvalBinding<'scope>],
        private_names: &'scope [DirectEvalPrivateName<'scope>],
    ) -> Self {
        Self {
            kind,
            bindings,
            private_names,
        }
    }

    /// Returns the frame role.
    #[must_use]
    pub const fn kind(self) -> DirectEvalScopeKind {
        self.kind
    }

    /// Returns the ordinary bindings in this frame.
    #[must_use]
    pub const fn bindings(self) -> &'scope [DirectEvalBinding<'scope>] {
        self.bindings
    }

    /// Returns the private names in this frame.
    #[must_use]
    pub const fn private_names(self) -> &'scope [DirectEvalPrivateName<'scope>] {
        self.private_names
    }
}

/// A caller scope-chain snapshot for direct `eval`.
///
/// Frames are ordered from the innermost caller scope to the outermost scope.
/// The snapshot is deliberately data-only; runtime handles and object
/// environments will be attached by the compiler/runtime integration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DirectEvalScopeSnapshot<'scope> {
    frames: &'scope [DirectEvalScopeFrame<'scope>],
}

impl<'scope> DirectEvalScopeSnapshot<'scope> {
    /// Creates a scope-chain snapshot.
    #[must_use]
    pub const fn new(frames: &'scope [DirectEvalScopeFrame<'scope>]) -> Self {
        Self { frames }
    }

    /// Returns the frames from innermost to outermost.
    #[must_use]
    pub const fn frames(self) -> &'scope [DirectEvalScopeFrame<'scope>] {
        self.frames
    }
}

/// Caller context needed to parse and resolve direct `eval`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectEvalContext<'scope> {
    capabilities: DirectEvalCapabilities,
    scope_snapshot: DirectEvalScopeSnapshot<'scope>,
}

impl<'scope> DirectEvalContext<'scope> {
    /// Creates a direct-eval context.
    #[must_use]
    pub const fn new(
        capabilities: DirectEvalCapabilities,
        scope_snapshot: DirectEvalScopeSnapshot<'scope>,
    ) -> Self {
        Self {
            capabilities,
            scope_snapshot,
        }
    }

    /// Returns the syntax capabilities inherited from the caller.
    #[must_use]
    pub const fn capabilities(self) -> DirectEvalCapabilities {
        self.capabilities
    }

    /// Returns the caller scope-chain snapshot.
    #[must_use]
    pub const fn scope_snapshot(self) -> DirectEvalScopeSnapshot<'scope> {
        self.scope_snapshot
    }
}

/// The dynamic function constructor family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicFunctionKind {
    /// `Function`.
    Function,
    /// `GeneratorFunction`.
    GeneratorFunction,
    /// `AsyncFunction`.
    AsyncFunction,
    /// `AsyncGeneratorFunction`.
    AsyncGeneratorFunction,
}

/// One separately supplied dynamic-function source fragment.
///
/// Dynamic function constructors receive zero or more parameter fragments and
/// one body fragment. Keeping those fragments separate is necessary for
/// faithful wrapper construction and future source-span remapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceFragment<'source> {
    text: &'source str,
    origin: Option<&'source str>,
}

impl<'source> SourceFragment<'source> {
    /// Creates an anonymous source fragment.
    #[must_use]
    pub const fn new(text: &'source str) -> Self {
        Self { text, origin: None }
    }

    /// Attaches a caller-facing origin label to this fragment.
    #[must_use]
    pub const fn with_origin(mut self, origin: &'source str) -> Self {
        self.origin = Some(origin);
        self
    }

    /// Returns the fragment text.
    #[must_use]
    pub const fn text(self) -> &'source str {
        self.text
    }

    /// Returns the optional caller-facing origin label.
    #[must_use]
    pub const fn origin(self) -> Option<&'source str> {
        self.origin
    }
}

/// Source fragments supplied to a dynamic function constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicFunctionSource<'source> {
    kind: DynamicFunctionKind,
    parameters: &'source [SourceFragment<'source>],
    body: SourceFragment<'source>,
}

impl<'source> DynamicFunctionSource<'source> {
    /// Creates a dynamic-function source description.
    #[must_use]
    pub const fn new(
        kind: DynamicFunctionKind,
        parameters: &'source [SourceFragment<'source>],
        body: SourceFragment<'source>,
    ) -> Self {
        Self {
            kind,
            parameters,
            body,
        }
    }

    /// Returns the constructor family.
    #[must_use]
    pub const fn kind(self) -> DynamicFunctionKind {
        self.kind
    }

    /// Returns the separately supplied parameter fragments.
    #[must_use]
    pub const fn parameters(self) -> &'source [SourceFragment<'source>] {
        self.parameters
    }

    /// Returns the separately supplied body fragment.
    #[must_use]
    pub const fn body(self) -> SourceFragment<'source> {
        self.body
    }

    fn source_bytes(self) -> usize {
        self.parameters
            .iter()
            .fold(self.body.text().len(), |total, fragment| {
                total.saturating_add(fragment.text().len())
            })
    }
}

/// The engine entry point whose grammar and early errors are being parsed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilationGoal<'scope> {
    /// A host-loaded Script.
    GlobalScript(GlobalScriptGoal),
    /// An ECMAScript Module.
    Module,
    /// An indirect call to `eval`.
    IndirectEval(IndirectEvalGoal),
    /// A direct call to `eval`, including its caller context.
    DirectEval(DirectEvalContext<'scope>),
    /// A dynamic function constructor invocation of the selected family.
    ///
    /// Source fragments are accepted only by
    /// [`with_dynamic_function_source`], so the kind cannot disagree with an
    /// independently supplied source description.
    DynamicFunction(DynamicFunctionKind),
}

impl CompilationGoal<'_> {
    const fn parse_mode(self) -> ParseMode {
        match self {
            Self::Module => ParseMode::Module,
            Self::GlobalScript(_)
            | Self::IndirectEval(_)
            | Self::DirectEval(_)
            | Self::DynamicFunction(_) => ParseMode::Script,
        }
    }

    const fn supported_parse_mode(self) -> Result<ParseMode, UnsupportedCompilationGoal> {
        match self {
            Self::GlobalScript(goal) if !goal.forces_strict() && !goal.allows_top_level_await() => {
                Ok(ParseMode::Script)
            }
            Self::GlobalScript(goal) => Err(UnsupportedCompilationGoal::GlobalScript(goal)),
            Self::Module => Ok(ParseMode::Module),
            Self::IndirectEval(goal) if !goal.forces_strict() => Ok(ParseMode::Script),
            Self::IndirectEval(goal) => Err(UnsupportedCompilationGoal::IndirectEval(goal)),
            Self::DirectEval(context) => Err(UnsupportedCompilationGoal::DirectEval(
                context.capabilities(),
            )),
            Self::DynamicFunction(kind) => Err(UnsupportedCompilationGoal::DynamicFunction(kind)),
        }
    }
}

/// A compilation goal that is represented by the public API but not yet
/// implemented faithfully by the Oxc-backed front end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnsupportedCompilationGoal {
    /// A global Script requested forced strictness or top-level `await`.
    GlobalScript(GlobalScriptGoal),
    /// An indirect eval requested forced strictness.
    IndirectEval(IndirectEvalGoal),
    /// Direct eval requires caller grammar state and scope-chain integration.
    DirectEval(DirectEvalCapabilities),
    /// A dynamic function requires exact wrapper construction and span mapping.
    DynamicFunction(DynamicFunctionKind),
}

impl UnsupportedCompilationGoal {
    fn message(self) -> String {
        match self {
            Self::GlobalScript(goal) => format!(
                "global Script compilation (force_strict={}, allow_top_level_await={}) is not implemented",
                goal.forces_strict(),
                goal.allows_top_level_await()
            ),
            Self::IndirectEval(goal) => format!(
                "indirect eval compilation (force_strict={}) is not implemented",
                goal.forces_strict()
            ),
            Self::DirectEval(capabilities) => format!(
                "direct eval compilation (strict={}, new_target={}, super_property={}, super_call={}, arguments_allowed={}) is not implemented",
                capabilities.is_strict(),
                capabilities.allows_new_target(),
                capabilities.allows_super_property(),
                capabilities.allows_super_call(),
                capabilities.allows_arguments()
            ),
            Self::DynamicFunction(kind) => {
                format!("dynamic function compilation (kind={kind}) is not implemented")
            }
        }
    }
}

impl fmt::Display for UnsupportedCompilationGoal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GlobalScript(goal) => write!(
                formatter,
                "global Script (forced strict: {}, top-level await: {})",
                goal.forces_strict(),
                goal.allows_top_level_await()
            ),
            Self::IndirectEval(goal) => write!(
                formatter,
                "indirect eval (forced strict: {})",
                goal.forces_strict()
            ),
            Self::DirectEval(capabilities) => write!(
                formatter,
                "direct eval (strict: {}, new target: {}, super property: {}, super call: {}, arguments allowed: {})",
                capabilities.is_strict(),
                capabilities.allows_new_target(),
                capabilities.allows_super_property(),
                capabilities.allows_super_call(),
                capabilities.allows_arguments()
            ),
            Self::DynamicFunction(kind) => write!(formatter, "dynamic function ({kind})"),
        }
    }
}

impl fmt::Display for DynamicFunctionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Function => "function",
            Self::GeneratorFunction => "generator-function",
            Self::AsyncFunction => "async-function",
            Self::AsyncGeneratorFunction => "async-generator-function",
        })
    }
}

/// An internally generated dynamic-function wrapper and its fragment map.
///
/// Once wrapper construction is implemented, the dedicated callback entry
/// will keep this owner alive alongside the Oxc arena. The map translates
/// wrapper positions back to the separately supplied parameter and body
/// fragments.
#[derive(Clone, Debug)]
pub struct PreparedDynamicFunctionSource {
    generated_source: String,
    source_map: SourceMap,
}

impl PreparedDynamicFunctionSource {
    /// Returns the generated JavaScript wrapper parsed by Oxc.
    #[must_use]
    pub fn generated_source(&self) -> &str {
        &self.generated_source
    }

    /// Returns the generated-wrapper to input-fragment source map.
    #[must_use]
    pub const fn source_map(&self) -> &SourceMap {
        &self.source_map
    }
}

/// Practical resource ceilings enforced by the JavaScript front end.
///
/// The source-byte ceiling is checked before Oxc is invoked and is always
/// bounded by Oxc's `u32` span domain and Rust's `isize` slice domain. The
/// pinned Oxc parser does not currently expose enforceable AST-node,
/// nesting-depth, or allocation budgets, so those remain an explicit residual
/// resource gap. Hosts processing untrusted input should additionally enforce
/// an outer memory/time isolation policy until those budgets can be
/// implemented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontendLimits {
    max_source_bytes: usize,
}

impl FrontendLimits {
    /// Creates limits with a caller-selected UTF-8 source-byte ceiling.
    #[must_use]
    pub const fn new(max_source_bytes: usize) -> Self {
        Self { max_source_bytes }
    }

    /// Replaces the UTF-8 source-byte ceiling.
    #[must_use]
    pub const fn with_max_source_bytes(mut self, max_source_bytes: usize) -> Self {
        self.max_source_bytes = max_source_bytes;
        self
    }

    /// Returns the maximum UTF-8 bytes accepted by one source entry.
    #[must_use]
    pub const fn max_source_bytes(self) -> usize {
        self.max_source_bytes
    }
}

impl Default for FrontendLimits {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SOURCE_BYTES)
    }
}

/// Options accepted by the engine's JavaScript-only front end.
///
/// [`Self::new`] preserves the ordinary Script/Module API. Engine entry points
/// should use [`Self::for_goal`] so contextual syntax cannot be mistaken for a
/// naked Script parse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontendOptions<'scope> {
    goal: CompilationGoal<'scope>,
    limits: FrontendLimits,
}

impl<'scope> FrontendOptions<'scope> {
    /// Creates options for an explicit Script or Module parse goal.
    #[must_use]
    pub const fn new(mode: ParseMode) -> Self {
        let goal = match mode {
            ParseMode::Script => CompilationGoal::GlobalScript(GlobalScriptGoal::new()),
            ParseMode::Module => CompilationGoal::Module,
        };
        Self {
            goal,
            limits: FrontendLimits::new(DEFAULT_MAX_SOURCE_BYTES),
        }
    }

    /// Creates options for a production engine compilation goal.
    #[must_use]
    pub const fn for_goal(goal: CompilationGoal<'scope>) -> Self {
        Self {
            goal,
            limits: FrontendLimits::new(DEFAULT_MAX_SOURCE_BYTES),
        }
    }

    /// Applies caller-selected resource ceilings.
    #[must_use]
    pub const fn with_limits(mut self, limits: FrontendLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Returns the production compilation goal.
    #[must_use]
    pub const fn goal(self) -> CompilationGoal<'scope> {
        self.goal
    }

    /// Returns the configured front-end resource ceilings.
    #[must_use]
    pub const fn limits(self) -> FrontendLimits {
        self.limits
    }

    /// Returns the underlying Oxc Script/Module source mode.
    ///
    /// This does not imply that a contextual goal is implemented. [`parse`]
    /// returns a structured error before invoking Oxc for unsupported goals.
    #[must_use]
    pub const fn mode(self) -> ParseMode {
        self.goal.parse_mode()
    }
}

impl Default for FrontendOptions<'_> {
    fn default() -> Self {
        Self {
            goal: CompilationGoal::GlobalScript(GlobalScriptGoal::new()),
            limits: FrontendLimits::new(DEFAULT_MAX_SOURCE_BYTES),
        }
    }
}

/// The validation phase that rejected a source unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticStage {
    /// A configured front-end resource ceiling rejected the source.
    ResourceLimit,
    /// The requested production compilation goal is not implemented faithfully.
    CompilationGoal,
    /// Oxc's lexer or parser emitted a diagnostic.
    Parser,
    /// The AST uses syntax outside the pinned `QuickJS` compatibility profile.
    Profile,
    /// Oxc's deferred ECMAScript early-error checks emitted a diagnostic.
    Semantic,
}

impl fmt::Display for DiagnosticStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit => formatter.write_str("resource-limit"),
            Self::CompilationGoal => formatter.write_str("compilation-goal"),
            Self::Parser => formatter.write_str("parser"),
            Self::Profile => formatter.write_str("profile"),
            Self::Semantic => formatter.write_str("semantic"),
        }
    }
}

/// Stable identity for one normalized front-end diagnostic.
///
/// Oxc parser and semantic diagnostics use stage-level identities because
/// their canonical message text is currently retained rather than translated
/// into QuickJS-exact diagnostic kinds. Compilation-goal and compatibility-
/// profile rejections have stable engine-owned identities.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FrontendDiagnosticCode {
    /// A source exceeded the configured UTF-8 byte ceiling.
    SourceBytesExceeded,
    /// A represented production compilation goal is not implemented faithfully.
    UnsupportedCompilationGoal,
    /// An Oxc lexer or parser diagnostic.
    OxcParser,
    /// An Oxc semantic/early-error diagnostic.
    OxcSemantic,
    /// A `using` declaration unsupported by the pinned `QuickJS` profile.
    UnsupportedUsingDeclaration,
    /// An `await using` declaration unsupported by the pinned `QuickJS` profile.
    UnsupportedAwaitUsingDeclaration,
    /// An `import source` declaration or expression.
    UnsupportedImportSource,
    /// An `import defer` declaration or expression.
    UnsupportedImportDefer,
    /// Decorator syntax.
    UnsupportedDecorator,
    /// A class `accessor` declaration.
    UnsupportedClassAccessor,
    /// A legacy `assert` import clause.
    UnsupportedLegacyImportAssertion,
}

impl FrontendDiagnosticCode {
    /// Returns the stable machine-readable code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceBytesExceeded => "quickjs::frontend::limit::source_bytes",
            Self::UnsupportedCompilationGoal => "quickjs::frontend::unsupported_compilation_goal",
            Self::OxcParser => "quickjs::frontend::oxc::parser",
            Self::OxcSemantic => "quickjs::frontend::oxc::semantic",
            Self::UnsupportedUsingDeclaration => "quickjs::frontend::profile::using_declaration",
            Self::UnsupportedAwaitUsingDeclaration => {
                "quickjs::frontend::profile::await_using_declaration"
            }
            Self::UnsupportedImportSource => "quickjs::frontend::profile::import_source",
            Self::UnsupportedImportDefer => "quickjs::frontend::profile::import_defer",
            Self::UnsupportedDecorator => "quickjs::frontend::profile::decorator",
            Self::UnsupportedClassAccessor => "quickjs::frontend::profile::class_accessor",
            Self::UnsupportedLegacyImportAssertion => {
                "quickjs::frontend::profile::legacy_import_assertion"
            }
        }
    }

    const fn help(self) -> Option<&'static str> {
        match self {
            Self::SourceBytesExceeded
            | Self::UnsupportedCompilationGoal
            | Self::OxcParser
            | Self::OxcSemantic => None,
            Self::UnsupportedUsingDeclaration
            | Self::UnsupportedAwaitUsingDeclaration
            | Self::UnsupportedImportSource
            | Self::UnsupportedImportDefer
            | Self::UnsupportedDecorator
            | Self::UnsupportedClassAccessor
            | Self::UnsupportedLegacyImportAssertion => {
                Some("rewrite this syntax for the QuickJS 2026-06-04 compatibility profile")
            }
        }
    }
}

impl fmt::Display for FrontendDiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A byte-span label attached to a front-end diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLabel {
    /// Half-open UTF-8 byte span.
    pub span: Span,
    /// Optional explanation for this particular label.
    pub message: Option<String>,
}

/// A source diagnostic copied out of Oxc's internal representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendDiagnostic {
    /// Stable normalized identity.
    pub code: FrontendDiagnosticCode,
    /// Primary diagnostic message.
    ///
    /// Parser and semantic messages retain Oxc's canonical text. They are not
    /// yet translated into QuickJS-exact wording; callers should use
    /// [`Self::code`] for stable identity.
    pub message: String,
    /// Labeled UTF-8 byte spans.
    pub labels: Vec<DiagnosticLabel>,
}

impl FrontendDiagnostic {
    fn from_oxc(code: FrontendDiagnosticCode, diagnostic: &OxcDiagnostic) -> Self {
        let labels = diagnostic
            .labels
            .iter()
            .map(|label| {
                let source_span = label.inner();
                let start = source_span.offset();
                let end_offset = source_span.offset().saturating_add(source_span.len());
                DiagnosticLabel {
                    span: Span::new(start, end_offset),
                    message: label.label().map(str::to_owned),
                }
            })
            .collect();

        Self {
            code,
            message: diagnostic.to_string(),
            labels,
        }
    }
}

/// A structured front-end resource-limit rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FrontendLimitError {
    /// One source entry exceeded its configured UTF-8 byte ceiling.
    SourceBytesExceeded {
        /// Observed UTF-8 source bytes.
        actual: usize,
        /// Configured maximum UTF-8 source bytes.
        limit: usize,
    },
}

impl fmt::Display for FrontendLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceBytesExceeded { actual, limit } => write!(
                formatter,
                "JavaScript source contains {actual} UTF-8 bytes, exceeding the configured limit of {limit} bytes"
            ),
        }
    }
}

impl Error for FrontendLimitError {}

/// A rejected JavaScript source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendError {
    stage: DiagnosticStage,
    diagnostics: Vec<FrontendDiagnostic>,
    parser_panicked: bool,
    unsupported_goal: Option<UnsupportedCompilationGoal>,
    limit_error: Option<FrontendLimitError>,
}

impl FrontendError {
    fn source_bytes_exceeded(actual: usize, limit: usize) -> Self {
        let limit_error = FrontendLimitError::SourceBytesExceeded { actual, limit };
        Self {
            stage: DiagnosticStage::ResourceLimit,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::SourceBytesExceeded,
                message: limit_error.to_string(),
                labels: Vec::new(),
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: Some(limit_error),
        }
    }

    fn unsupported_compilation_goal(goal: UnsupportedCompilationGoal) -> Self {
        Self {
            stage: DiagnosticStage::CompilationGoal,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::UnsupportedCompilationGoal,
                message: goal.message(),
                labels: Vec::new(),
            }],
            parser_panicked: false,
            unsupported_goal: Some(goal),
            limit_error: None,
        }
    }

    fn from_oxc(
        stage: DiagnosticStage,
        code: FrontendDiagnosticCode,
        diagnostics: Diagnostics,
        parser_panicked: bool,
    ) -> Self {
        let mut diagnostics = diagnostics
            .into_iter()
            .map(|diagnostic| FrontendDiagnostic::from_oxc(code, &diagnostic))
            .collect::<Vec<_>>();
        if diagnostics.is_empty() {
            diagnostics.push(FrontendDiagnostic {
                code,
                message: "front end aborted without a diagnostic".to_owned(),
                labels: Vec::new(),
            });
        }
        Self {
            stage,
            diagnostics,
            parser_panicked,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn from_profile(diagnostics: Vec<FrontendDiagnostic>) -> Self {
        Self {
            stage: DiagnosticStage::Profile,
            diagnostics,
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    /// Returns the phase that rejected the source.
    #[must_use]
    pub const fn stage(&self) -> DiagnosticStage {
        self.stage
    }

    /// Returns every diagnostic emitted by the rejecting phase.
    #[must_use]
    pub fn diagnostics(&self) -> &[FrontendDiagnostic] {
        &self.diagnostics
    }

    /// Returns whether Oxc stopped after an unrecoverable parser error.
    #[must_use]
    pub const fn parser_panicked(&self) -> bool {
        self.parser_panicked
    }

    /// Returns the structured unsupported goal when rejection happened before
    /// invoking Oxc.
    #[must_use]
    pub const fn unsupported_goal(&self) -> Option<UnsupportedCompilationGoal> {
        self.unsupported_goal
    }

    /// Returns the structured resource-limit rejection, when applicable.
    #[must_use]
    pub const fn limit_error(&self) -> Option<FrontendLimitError> {
        self.limit_error
    }

    /// Converts every diagnostic and label to the shared source-registry
    /// representation.
    ///
    /// Oxc and compatibility-profile spans are validated against the
    /// registered source before any shared diagnostic is returned.
    ///
    /// # Errors
    ///
    /// Returns a structured source-integration error for a foreign source ID,
    /// an invalid UTF-8 byte span, or an invalid internal stable code.
    pub fn into_registered_diagnostics(
        self,
        sources: &SourceRegistry,
        source_id: &SourceId,
    ) -> Result<RegisteredFrontendDiagnostics, FrontendSourceError> {
        sources
            .source(source_id)
            .map_err(FrontendSourceError::Registry)?;
        let diagnostics = self
            .diagnostics
            .into_iter()
            .enumerate()
            .map(|(diagnostic_index, diagnostic)| {
                convert_diagnostic(sources, source_id, diagnostic_index, diagnostic)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RegisteredFrontendDiagnostics {
            source_id: source_id.clone(),
            stage: self.stage,
            diagnostics,
            parser_panicked: self.parser_panicked,
            unsupported_goal: self.unsupported_goal,
            limit_error: self.limit_error,
        })
    }
}

impl fmt::Display for FrontendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self
            .diagnostics
            .first()
            .map_or("front end aborted without a diagnostic", |diagnostic| {
                diagnostic.message.as_str()
            });
        write!(formatter, "{} validation failed: {message}", self.stage)
    }
}

impl Error for FrontendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.limit_error
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

fn convert_diagnostic(
    sources: &SourceRegistry,
    source_id: &SourceId,
    diagnostic_index: usize,
    diagnostic: FrontendDiagnostic,
) -> Result<SharedDiagnostic, FrontendSourceError> {
    let code = SharedDiagnosticCode::new(diagnostic.code.as_str()).map_err(|error| {
        FrontendSourceError::DiagnosticCode {
            diagnostic_index,
            code: diagnostic.code,
            error,
        }
    })?;
    let mut shared = SharedDiagnostic::new(code, DiagnosticSeverity::Error, diagnostic.message);
    if let Some(help) = diagnostic.code.help() {
        shared = shared.with_help(help);
    }
    for (label_index, label) in diagnostic.labels.into_iter().enumerate() {
        let span = sources
            .span(
                source_id,
                label.span.start as usize,
                label.span.end as usize,
            )
            .map_err(|error| FrontendSourceError::DiagnosticSpan {
                diagnostic_index,
                label_index,
                span: label.span,
                error,
            })?;
        let label = if label_index == 0 {
            SharedDiagnosticLabel::primary(span, label.message)
        } else {
            SharedDiagnosticLabel::secondary(span, label.message)
        };
        shared = shared.with_label(label);
    }
    Ok(shared)
}

#[derive(Clone, Copy)]
struct ProfileViolation {
    span: Span,
    code: FrontendDiagnosticCode,
    message: &'static str,
}

fn quickjs_profile_diagnostics(nodes: &AstNodes<'_>) -> Vec<FrontendDiagnostic> {
    let mut violations = Vec::new();

    for node in nodes {
        match node.kind() {
            AstKind::VariableDeclaration(declaration) => match declaration.kind {
                VariableDeclarationKind::Using => violations.push(ProfileViolation {
                    span: declaration.span,
                    code: FrontendDiagnosticCode::UnsupportedUsingDeclaration,
                    message: "QuickJS 2026-06-04 does not support `using` declarations",
                }),
                VariableDeclarationKind::AwaitUsing => violations.push(ProfileViolation {
                    span: declaration.span,
                    code: FrontendDiagnosticCode::UnsupportedAwaitUsingDeclaration,
                    message: "QuickJS 2026-06-04 does not support `await using` declarations",
                }),
                VariableDeclarationKind::Var
                | VariableDeclarationKind::Let
                | VariableDeclarationKind::Const => {}
            },
            AstKind::ImportDeclaration(declaration) => {
                push_import_phase_violation(&mut violations, declaration.phase, declaration.span);
            }
            AstKind::ImportExpression(expression) => {
                push_import_phase_violation(&mut violations, expression.phase, expression.span);
            }
            AstKind::Decorator(decorator) => violations.push(ProfileViolation {
                span: decorator.span,
                code: FrontendDiagnosticCode::UnsupportedDecorator,
                message: "QuickJS 2026-06-04 does not support decorators",
            }),
            AstKind::AccessorProperty(property) => violations.push(ProfileViolation {
                span: property.span,
                code: FrontendDiagnosticCode::UnsupportedClassAccessor,
                message: "QuickJS 2026-06-04 does not support class `accessor` declarations",
            }),
            AstKind::WithClause(clause) if clause.keyword == WithClauseKeyword::Assert => {
                violations.push(ProfileViolation {
                    span: clause.span,
                    code: FrontendDiagnosticCode::UnsupportedLegacyImportAssertion,
                    message: "QuickJS 2026-06-04 does not support legacy import assertions; use import attributes with `with`",
                });
            }
            _ => {}
        }
    }

    violations.sort_unstable_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| left.span.end.cmp(&right.span.end))
            .then_with(|| left.code.as_str().cmp(right.code.as_str()))
    });
    violations
        .into_iter()
        .map(|violation| FrontendDiagnostic {
            code: violation.code,
            message: violation.message.to_owned(),
            labels: vec![DiagnosticLabel {
                span: violation.span,
                message: Some("unsupported by the QuickJS 2026-06-04 profile".to_owned()),
            }],
        })
        .collect()
}

fn push_import_phase_violation(
    violations: &mut Vec<ProfileViolation>,
    phase: Option<ImportPhase>,
    span: Span,
) {
    let (code, message) = match phase {
        Some(ImportPhase::Source) => (
            FrontendDiagnosticCode::UnsupportedImportSource,
            "QuickJS 2026-06-04 does not support `import source`",
        ),
        Some(ImportPhase::Defer) => (
            FrontendDiagnosticCode::UnsupportedImportDefer,
            "QuickJS 2026-06-04 does not support `import defer`",
        ),
        None => return,
    };
    violations.push(ProfileViolation {
        span,
        code,
        message,
    });
}

/// Shared diagnostics produced for one registered source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredFrontendDiagnostics {
    source_id: SourceId,
    stage: DiagnosticStage,
    diagnostics: Vec<SharedDiagnostic>,
    parser_panicked: bool,
    unsupported_goal: Option<UnsupportedCompilationGoal>,
    limit_error: Option<FrontendLimitError>,
}

impl RegisteredFrontendDiagnostics {
    /// Returns the registered source that produced these diagnostics.
    #[must_use]
    pub const fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Returns the rejecting front-end stage.
    #[must_use]
    pub const fn stage(&self) -> DiagnosticStage {
        self.stage
    }

    /// Returns every validated shared diagnostic.
    #[must_use]
    pub fn diagnostics(&self) -> &[SharedDiagnostic] {
        &self.diagnostics
    }

    /// Returns whether Oxc stopped after an unrecoverable parser error.
    #[must_use]
    pub const fn parser_panicked(&self) -> bool {
        self.parser_panicked
    }

    /// Returns the structured unsupported goal when rejection happened before
    /// invoking Oxc.
    #[must_use]
    pub const fn unsupported_goal(&self) -> Option<UnsupportedCompilationGoal> {
        self.unsupported_goal
    }

    /// Returns the structured resource-limit rejection, when applicable.
    #[must_use]
    pub const fn limit_error(&self) -> Option<FrontendLimitError> {
        self.limit_error
    }
}

impl fmt::Display for RegisteredFrontendDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} validation failed with {} diagnostic(s)",
            self.stage,
            self.diagnostics.len()
        )
    }
}

impl Error for RegisteredFrontendDiagnostics {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.limit_error
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

/// Source-registry or diagnostic-conversion failures at the registered-source
/// boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FrontendSourceError {
    /// The source ID was foreign or otherwise invalid.
    Registry(SourceError),
    /// A stable internal code failed shared-code validation.
    DiagnosticCode {
        /// Index of the front-end diagnostic.
        diagnostic_index: usize,
        /// Typed front-end identity.
        code: FrontendDiagnosticCode,
        /// Shared-code validation failure.
        error: DiagnosticCodeError,
    },
    /// A front-end label was not a valid range in the registered source.
    DiagnosticSpan {
        /// Index of the front-end diagnostic.
        diagnostic_index: usize,
        /// Index of the label within that diagnostic.
        label_index: usize,
        /// Rejected Oxc UTF-8 byte span.
        span: Span,
        /// Range or UTF-8-boundary failure.
        error: SourceError,
    },
}

impl fmt::Display for FrontendSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => write!(formatter, "cannot access registered source: {error}"),
            Self::DiagnosticCode {
                diagnostic_index,
                code,
                error,
            } => write!(
                formatter,
                "front-end diagnostic {diagnostic_index} has invalid stable code `{code}`: {error}"
            ),
            Self::DiagnosticSpan {
                diagnostic_index,
                label_index,
                span,
                error,
            } => write!(
                formatter,
                "front-end diagnostic {diagnostic_index} label {label_index} has invalid byte span {}..{}: {error}",
                span.start, span.end
            ),
        }
    }
}

impl Error for FrontendSourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) | Self::DiagnosticSpan { error, .. } => Some(error),
            Self::DiagnosticCode { error, .. } => Some(error),
        }
    }
}

/// Failure from [`with_registered_program`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RegisteredFrontendError {
    /// Registry access or diagnostic conversion failed.
    Source(FrontendSourceError),
    /// The registered JavaScript source was rejected.
    Diagnostics(RegisteredFrontendDiagnostics),
}

impl fmt::Display for RegisteredFrontendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => fmt::Display::fmt(error, formatter),
            Self::Diagnostics(diagnostics) => fmt::Display::fmt(diagnostics, formatter),
        }
    }
}

impl Error for RegisteredFrontendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Diagnostics(diagnostics) => Some(diagnostics),
        }
    }
}

/// One successfully parsed and validated JavaScript compilation unit.
///
/// Oxc's [`SourceType`] distinguishes Script from Module but cannot retain
/// engine entry-point identity such as global Script versus indirect eval.
/// This wrapper keeps the validated [`CompilationGoal`] attached to the arena-
/// owned [`Program`].
#[derive(Debug)]
pub struct ParsedUnit<'arena, 'scope> {
    goal: CompilationGoal<'scope>,
    program: Program<'arena>,
}

impl<'arena, 'scope> ParsedUnit<'arena, 'scope> {
    /// Returns the validated engine compilation goal.
    #[must_use]
    pub const fn goal(&self) -> CompilationGoal<'scope> {
        self.goal
    }

    /// Returns the arena-owned Oxc program.
    #[must_use]
    pub const fn program(&self) -> &Program<'arena> {
        &self.program
    }
}

fn enforce_source_limit(source_bytes: usize, limits: FrontendLimits) -> Result<(), FrontendError> {
    let limit = limits.max_source_bytes().min(MAX_OXC_SOURCE_BYTES);
    if source_bytes > limit {
        Err(FrontendError::source_bytes_exceeded(source_bytes, limit))
    } else {
        Ok(())
    }
}

/// Parses and validates JavaScript using a caller-owned Oxc arena.
///
/// The returned unit retains the validated engine compilation goal alongside
/// the AST. It borrows both `allocator` and `source_text`; callers must keep
/// both alive and must not reset the allocator while the unit is in use.
/// TypeScript, JSX, and unambiguous source-mode detection are not exposed.
///
/// # Errors
///
/// Returns an error if the parser emits any diagnostic (including a
/// recoverable one), if the AST uses syntax outside the pinned `QuickJS`
/// compatibility profile, or if deferred semantic early-error checking emits
/// any diagnostic. Contextual goals that are represented but not yet
/// implemented return [`FrontendDiagnosticCode::UnsupportedCompilationGoal`]
/// before Oxc is invoked.
pub fn parse<'arena, 'scope>(
    allocator: &'arena Allocator,
    source_text: &'arena str,
    options: FrontendOptions<'scope>,
) -> Result<ParsedUnit<'arena, 'scope>, FrontendError> {
    enforce_source_limit(source_text.len(), options.limits)?;
    let goal = options.goal;
    let mode = goal
        .supported_parse_mode()
        .map_err(FrontendError::unsupported_compilation_goal)?;
    let parsed = Parser::new(allocator, source_text, mode.source_type())
        .with_options(OxcParseOptions::default())
        .parse();

    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(FrontendError::from_oxc(
            DiagnosticStage::Parser,
            FrontendDiagnosticCode::OxcParser,
            parsed.diagnostics,
            parsed.panicked,
        ));
    }

    let program = parsed.program;
    let semantic = SemanticBuilder::new_compiler()
        .with_build_nodes(true)
        .build(&program);
    let profile_diagnostics = quickjs_profile_diagnostics(semantic.semantic.nodes());
    if !profile_diagnostics.is_empty() {
        return Err(FrontendError::from_profile(profile_diagnostics));
    }
    if !semantic.diagnostics.is_empty() {
        return Err(FrontendError::from_oxc(
            DiagnosticStage::Semantic,
            FrontendDiagnosticCode::OxcSemantic,
            semantic.diagnostics,
            false,
        ));
    }
    drop(semantic);

    Ok(ParsedUnit { goal, program })
}

/// Parses and validates a source unit inside a short-lived arena.
///
/// The higher-ranked callback cannot return a value that borrows the unit, so
/// arena-backed nodes cannot escape this function. The callback can inspect
/// the validated compilation goal as well as the Oxc program.
///
/// # Errors
///
/// Returns the same parser or semantic diagnostics as [`parse`].
pub fn with_parsed_program<'scope, R>(
    source_text: &str,
    options: FrontendOptions<'scope>,
    callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'scope>) -> R,
) -> Result<R, FrontendError> {
    let allocator = Allocator::new();
    let unit = parse(&allocator, source_text, options)?;
    Ok(callback(&unit))
}

/// Parses one registered source inside a short-lived Oxc arena.
///
/// The source text is obtained from `sources` using `source_id`. The
/// higher-ranked callback cannot return a value borrowing the arena-backed AST,
/// so neither the [`Program`] nor any of its nodes can escape.
///
/// Parser and semantic diagnostics retain the canonical text supplied by the
/// pinned Oxc dependency. Their stable identity is stage-normalized, but their
/// wording is not yet translated to QuickJS-exact messages.
///
/// # Errors
///
/// Returns [`RegisteredFrontendError::Source`] when the source ID is invalid or
/// a produced diagnostic span cannot be validated. Returns
/// [`RegisteredFrontendError::Diagnostics`] when JavaScript parsing, the
/// `QuickJS` compatibility profile, or ECMAScript early-error validation rejects
/// the source.
pub fn with_registered_program<'scope, R>(
    sources: &SourceRegistry,
    source_id: &SourceId,
    options: FrontendOptions<'scope>,
    callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'scope>) -> R,
) -> Result<R, RegisteredFrontendError> {
    let source = sources
        .source(source_id)
        .map_err(FrontendSourceError::Registry)
        .map_err(RegisteredFrontendError::Source)?;
    let allocator = Allocator::new();
    match parse(&allocator, source.text(), options) {
        Ok(unit) => Ok(callback(&unit)),
        Err(error) => {
            let diagnostics = error
                .into_registered_diagnostics(sources, source_id)
                .map_err(RegisteredFrontendError::Source)?;
            Err(RegisteredFrontendError::Diagnostics(diagnostics))
        }
    }
}

/// Prepares and parses source fragments for a dynamic function constructor.
///
/// This is deliberately separate from [`parse`]: the constructor's parameter
/// and body fragments must first be assembled into one exact wrapper and
/// accompanied by a generated-to-fragment source map. Once implemented, the
/// callback will receive both the parsed unit and the owning
/// [`PreparedDynamicFunctionSource`], keeping the generated source and map
/// alive for the complete arena callback.
///
/// The configured source-byte ceiling currently applies to the sum of the
/// caller-supplied fragment bytes. Wrapper overhead will also be checked before
/// Oxc when wrapper construction is implemented.
///
/// # Errors
///
/// Returns a structured resource-limit error when the fragments exceed
/// `limits`. Otherwise returns a structured unsupported-compilation-goal error
/// before invoking Oxc because exact wrapper construction is not implemented.
pub fn with_dynamic_function_source<R>(
    source: DynamicFunctionSource<'_>,
    limits: FrontendLimits,
    _callback: impl for<'arena> FnOnce(
        &ParsedUnit<'arena, 'static>,
        &PreparedDynamicFunctionSource,
    ) -> R,
) -> Result<R, FrontendError> {
    enforce_source_limit(source.source_bytes(), limits)?;
    Err(FrontendError::unsupported_compilation_goal(
        UnsupportedCompilationGoal::DynamicFunction(source.kind()),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticLabel, DiagnosticStage, FrontendDiagnostic, FrontendDiagnosticCode,
        FrontendError, FrontendLimitError, FrontendLimits, FrontendSourceError,
        MAX_OXC_SOURCE_BYTES, enforce_source_limit,
    };
    use oxc_span::Span;
    use quickjs_diagnostics::SourceRegistry;

    #[test]
    fn configured_source_limit_cannot_exceed_oxc_span_capacity() {
        let simulated_length = MAX_OXC_SOURCE_BYTES
            .checked_add(1)
            .expect("the parser ceiling is below usize::MAX");
        let error = enforce_source_limit(simulated_length, FrontendLimits::new(usize::MAX))
            .expect_err("the absolute parser ceiling must be enforced before Oxc");

        assert_eq!(error.stage(), DiagnosticStage::ResourceLimit);
        assert_eq!(
            error.limit_error(),
            Some(FrontendLimitError::SourceBytesExceeded {
                actual: simulated_length,
                limit: MAX_OXC_SOURCE_BYTES,
            })
        );
    }

    #[test]
    fn malformed_internal_label_span_is_a_structured_conversion_error() {
        let mut sources = SourceRegistry::new();
        let source_id = sources.register("malformed.js", "é").expect("source");
        let error = FrontendError {
            stage: DiagnosticStage::Parser,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::OxcParser,
                message: "synthetic malformed span".to_owned(),
                labels: vec![DiagnosticLabel {
                    span: Span::new(1, 2),
                    message: None,
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        };

        let conversion = error
            .into_registered_diagnostics(&sources, &source_id)
            .expect_err("offset one splits the UTF-8 encoding of é");
        assert!(matches!(
            conversion,
            FrontendSourceError::DiagnosticSpan {
                diagnostic_index: 0,
                label_index: 0,
                span,
                ..
            } if span == Span::new(1, 2)
        ));
    }
}
