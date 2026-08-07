//! Oxc-backed JavaScript parsing and ECMAScript early-error validation.
//!
//! This module is the reusable source boundary for the compiler crate.
//!
//! Oxc identifies regular-expression literal boundaries and flags. The
//! project-owned `quickjs-regexp` layer then applies the specification's
//! `IsValidRegularExpressionLiteral` early error and owns executable pattern
//! semantics.

use std::{collections::HashMap, error::Error, fmt};

pub use oxc_allocator::Allocator;
pub use oxc_ast::ast::Program;
use oxc_ast::{
    AstKind,
    ast::{
        Argument, Directive, ImportPhase, ModuleExportName, Statement, VariableDeclarationKind,
        WithClauseKeyword,
    },
    builder::AstBuilder,
};
use oxc_diagnostics::{Diagnostics, OxcDiagnostic};
use oxc_parser::{ParseOptions as OxcParseOptions, Parser, ParserReturn};
use oxc_semantic::{AstNodes, SemanticBuilder};
pub use oxc_semantic::{Scoping, Semantic};
pub use oxc_span::Span;
use oxc_span::{GetSpan, SourceType};
pub use oxc_syntax::module_record::ModuleRecord;
use oxc_syntax::node::NodeId;
use quickjs_diagnostics::{
    Diagnostic as SharedDiagnostic, DiagnosticCode as SharedDiagnosticCode, DiagnosticCodeError,
    DiagnosticLabel as SharedDiagnosticLabel, DiagnosticSeverity, SourceError, SourceId,
    SourceRegistry,
};

use crate::module_syntax::{ModuleSyntaxLoweringError, ModuleSyntaxRecord};

/// The default maximum UTF-8 source size accepted by one front-end entry.
///
/// Hosts can select a different ceiling with [`FrontendLimits`].
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

/// The default maximum number of separately supplied dynamic-function
/// fragments, including the body fragment.
pub const DEFAULT_MAX_DYNAMIC_FUNCTION_FRAGMENTS: usize = 1_048_576;

/// The default maximum UTF-8 bytes retained in dynamic-function origin labels.
pub const DEFAULT_MAX_DYNAMIC_FUNCTION_ORIGIN_BYTES: usize = 16 * 1024 * 1024;

/// Stack reservation for the isolated Oxc parser/semantic worker.
///
/// Oxc owns its internal traversal strategy. Keeping it on a dedicated thread
/// prevents its stack requirements from consuming the host or runtime thread's
/// stack; project-owned lowering remains iterative.
pub const DEFAULT_ISOLATED_FRONTEND_STACK_BYTES: usize = 64 * 1024 * 1024;

const MAX_OXC_SOURCE_BYTES: usize = {
    let span_limit = u32::MAX as usize;
    let slice_limit = isize::MAX.unsigned_abs();
    if span_limit < slice_limit {
        span_limit
    } else {
        slice_limit
    }
};

const DYNAMIC_FUNCTION_PARAMETERS_BODY_SEPARATOR: &str = "\n) {\n";
const DYNAMIC_FUNCTION_SUFFIX: &str = "\n})";
const MAX_FIXED_CALL_ARGUMENTS: usize = u16::MAX as usize;

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
/// faithful wrapper construction and source-span remapping.
///
/// This UTF-8 boundary cannot represent an isolated UTF-16 surrogate. The
/// eventual runtime `JSString` adapter must reject such input before calling
/// this API until a lossless UTF-16-to-Oxc preprocessing layer exists. This
/// crate never substitutes a replacement character.
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
            Self::GlobalScript(_) => Ok(ParseMode::Script),
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

/// A compilation goal that cannot be processed by the ordinary naked-source
/// [`parse`] entry, either because its contextual adapter is pending or
/// because it requires a dedicated source-preparation entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnsupportedCompilationGoal {
    /// An indirect eval requested forced strictness.
    IndirectEval(IndirectEvalGoal),
    /// Direct eval requires caller grammar state and scope-chain integration.
    DirectEval(DirectEvalCapabilities),
    /// Dynamic function source was passed naked instead of as constructor
    /// fragments to [`with_dynamic_function_source`].
    DynamicFunction(DynamicFunctionKind),
}

impl UnsupportedCompilationGoal {
    fn message(self) -> String {
        match self {
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
                format!(
                    "dynamic function compilation (kind={kind}) requires exact fragment preparation through with_dynamic_function_source"
                )
            }
        }
    }
}

impl fmt::Display for UnsupportedCompilationGoal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

impl DynamicFunctionKind {
    const fn wrapper_prefix(self) -> &'static str {
        match self {
            Self::Function => "(function anonymous(",
            Self::GeneratorFunction => "(function* anonymous(",
            Self::AsyncFunction => "(async function anonymous(",
            Self::AsyncGeneratorFunction => "(async function* anonymous(",
        }
    }
}

/// The role of one caller-supplied dynamic-function fragment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DynamicFunctionFragmentRole {
    /// One parameter-list argument, in constructor argument order.
    Parameter {
        /// Zero-based parameter-fragment index.
        index: u32,
    },
    /// The constructor's final body argument.
    Body,
}

/// A half-open UTF-8 byte range in generated or fragment source.
///
/// Values returned by this crate are always ordered and bounded by their
/// owning source. The fields are private so callers cannot forge a range and
/// mistake it for one validated by the fragment map.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DynamicFunctionByteRange {
    start: u32,
    end: u32,
}

impl DynamicFunctionByteRange {
    const fn new(start: u32, end: u32) -> Self {
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

    /// Returns the byte length.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Returns whether the range contains no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// The purpose of bytes inserted by the dynamic-function wrapper.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DynamicFunctionSyntheticKind {
    /// The family-specific `(function ... anonymous(` prefix.
    Prefix,
    /// A comma between separately supplied parameter fragments.
    ParameterSeparator,
    /// The exact `\n) {\n` separator before the body.
    ParametersBodySeparator,
    /// The exact `\n})` wrapper suffix.
    Suffix,
}

#[derive(Debug, Eq, PartialEq)]
struct DynamicFunctionFragmentRecord {
    role: DynamicFunctionFragmentRole,
    generated_range: DynamicFunctionByteRange,
    text: String,
    origin: Option<String>,
}

impl DynamicFunctionFragmentRecord {
    fn text(&self) -> &str {
        &self.text
    }

    fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicFunctionSegmentSource {
    Synthetic(DynamicFunctionSyntheticKind),
    Fragment(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DynamicFunctionMapSegment {
    generated_range: DynamicFunctionByteRange,
    source: DynamicFunctionSegmentSource,
}

/// Bias used when mapping a zero-width generated span at a segment boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DynamicFunctionSpanBias {
    /// Prefer the segment immediately before the boundary.
    Earlier,
    /// Prefer the segment immediately after the boundary.
    Later,
}

/// The source represented by one clipped map result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicFunctionMappedSource<'map> {
    /// Generated wrapper syntax with no caller fragment.
    Synthetic(DynamicFunctionSyntheticKind),
    /// A verbatim caller fragment and the corresponding original byte range.
    Copied {
        /// Parameter/body identity.
        role: DynamicFunctionFragmentRole,
        /// Original half-open byte range within `text`.
        original_range: DynamicFunctionByteRange,
        /// Retained caller-facing origin label.
        origin: Option<&'map str>,
        /// Retained complete original fragment.
        text: &'map str,
    },
}

/// One generated span clipped to an exact fragment-map segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicFunctionMappedSegment<'map> {
    generated_range: DynamicFunctionByteRange,
    source: DynamicFunctionMappedSource<'map>,
}

impl<'map> DynamicFunctionMappedSegment<'map> {
    /// Returns the clipped generated byte range.
    #[must_use]
    pub const fn generated_range(self) -> DynamicFunctionByteRange {
        self.generated_range
    }

    /// Returns the synthetic or copied source classification.
    #[must_use]
    pub const fn source(self) -> DynamicFunctionMappedSource<'map> {
        self.source
    }
}

/// A rejected query against a dynamic-function fragment map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DynamicFunctionMapError {
    /// The requested generated span was reversed or outside the wrapper.
    InvalidGeneratedSpan {
        /// Requested start byte.
        start: u32,
        /// Requested end byte.
        end: u32,
        /// Generated wrapper byte length.
        generated_len: u32,
    },
    /// A requested endpoint split one UTF-8 scalar copied from a fragment.
    InvalidUtf8Boundary {
        /// Rejected generated byte offset.
        offset: u32,
    },
    /// The result vector could not reserve memory.
    AllocationFailed {
        /// Number of result entries requested.
        requested: usize,
    },
}

impl fmt::Display for DynamicFunctionMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeneratedSpan {
                start,
                end,
                generated_len,
            } => write!(
                formatter,
                "generated span {start}..{end} is outside dynamic-function wrapper 0..{generated_len}"
            ),
            Self::InvalidUtf8Boundary { offset } => write!(
                formatter,
                "generated byte offset {offset} splits a UTF-8 scalar in a dynamic-function fragment"
            ),
            Self::AllocationFailed { requested } => write!(
                formatter,
                "could not reserve {requested} dynamic-function map result entries"
            ),
        }
    }
}

impl Error for DynamicFunctionMapError {}

/// An owned, byte-exact map from a generated dynamic-function wrapper to the
/// caller-supplied fragments.
///
/// The segment list retains both synthetic and copied ranges in construction
/// order. Empty input fragments are preserved as zero-width copied segments.
#[derive(Debug, Eq, PartialEq)]
pub struct DynamicFunctionFragmentMap {
    generated_len: u32,
    fragments: Vec<DynamicFunctionFragmentRecord>,
    segments: Vec<DynamicFunctionMapSegment>,
}

impl DynamicFunctionFragmentMap {
    /// Returns the generated wrapper byte length.
    #[must_use]
    pub const fn generated_len(&self) -> u32 {
        self.generated_len
    }

    /// Splits a generated span at every wrapper/fragment boundary.
    ///
    /// Non-empty spans return every intersected synthetic and copied segment.
    /// A zero-width span returns one biased segment. An empty copied fragment
    /// anchored at that byte takes precedence over adjacent synthetic syntax,
    /// preserving diagnostics for empty constructor arguments.
    ///
    /// # Errors
    ///
    /// Returns a structured error for an invalid generated span or if the
    /// result vector cannot reserve memory.
    pub fn map_generated_span(
        &self,
        span: Span,
        zero_width_bias: DynamicFunctionSpanBias,
    ) -> Result<Vec<DynamicFunctionMappedSegment<'_>>, DynamicFunctionMapError> {
        if span.start > span.end || span.end > self.generated_len {
            return Err(DynamicFunctionMapError::InvalidGeneratedSpan {
                start: span.start,
                end: span.end,
                generated_len: self.generated_len,
            });
        }
        for offset in [span.start, span.end] {
            if !self.is_generated_char_boundary(offset) {
                return Err(DynamicFunctionMapError::InvalidUtf8Boundary { offset });
            }
        }

        if span.start == span.end {
            let segment = self.zero_width_segment(span.start, zero_width_bias);
            let mut mapped = Vec::new();
            if let Some(segment) = segment {
                mapped
                    .try_reserve_exact(1)
                    .map_err(|_| DynamicFunctionMapError::AllocationFailed { requested: 1 })?;
                mapped.push(self.map_segment(segment, span.start, span.end));
            }
            return Ok(mapped);
        }

        let requested = self
            .segments
            .iter()
            .filter(|segment| {
                let range = segment.generated_range;
                !range.is_empty() && range.end > span.start && range.start < span.end
            })
            .count();
        let mut mapped = Vec::new();
        mapped
            .try_reserve_exact(requested)
            .map_err(|_| DynamicFunctionMapError::AllocationFailed { requested })?;
        for segment in &self.segments {
            let range = segment.generated_range;
            if range.is_empty() || range.end <= span.start || range.start >= span.end {
                continue;
            }
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            mapped.push(self.map_segment(segment, start, end));
        }
        Ok(mapped)
    }

    fn is_generated_char_boundary(&self, offset: u32) -> bool {
        self.segments.iter().all(|segment| {
            let DynamicFunctionSegmentSource::Fragment(index) = segment.source else {
                return true;
            };
            let range = segment.generated_range;
            if offset <= range.start || offset >= range.end {
                return true;
            }
            usize::try_from(offset - range.start)
                .is_ok_and(|relative| self.fragments[index].text.is_char_boundary(relative))
        })
    }

    fn zero_width_segment(
        &self,
        offset: u32,
        bias: DynamicFunctionSpanBias,
    ) -> Option<&DynamicFunctionMapSegment> {
        let empty_fragment = match bias {
            DynamicFunctionSpanBias::Earlier => self.segments.iter().rev().find(|segment| {
                segment.generated_range == DynamicFunctionByteRange::new(offset, offset)
                    && matches!(segment.source, DynamicFunctionSegmentSource::Fragment(_))
            }),
            DynamicFunctionSpanBias::Later => self.segments.iter().find(|segment| {
                segment.generated_range == DynamicFunctionByteRange::new(offset, offset)
                    && matches!(segment.source, DynamicFunctionSegmentSource::Fragment(_))
            }),
        };
        if empty_fragment.is_some() {
            return empty_fragment;
        }

        match bias {
            DynamicFunctionSpanBias::Earlier => self
                .segments
                .iter()
                .rev()
                .find(|segment| {
                    !segment.generated_range.is_empty()
                        && segment.generated_range.start < offset
                        && offset <= segment.generated_range.end
                })
                .or_else(|| {
                    self.segments.iter().find(|segment| {
                        !segment.generated_range.is_empty()
                            && segment.generated_range.start <= offset
                            && offset < segment.generated_range.end
                    })
                }),
            DynamicFunctionSpanBias::Later => self
                .segments
                .iter()
                .find(|segment| {
                    !segment.generated_range.is_empty()
                        && segment.generated_range.start <= offset
                        && offset < segment.generated_range.end
                })
                .or_else(|| {
                    self.segments.iter().rev().find(|segment| {
                        !segment.generated_range.is_empty()
                            && segment.generated_range.start < offset
                            && offset <= segment.generated_range.end
                    })
                }),
        }
    }

    fn map_segment<'map>(
        &'map self,
        segment: &DynamicFunctionMapSegment,
        start: u32,
        end: u32,
    ) -> DynamicFunctionMappedSegment<'map> {
        let generated_range = DynamicFunctionByteRange::new(start, end);
        let source = match segment.source {
            DynamicFunctionSegmentSource::Synthetic(kind) => {
                DynamicFunctionMappedSource::Synthetic(kind)
            }
            DynamicFunctionSegmentSource::Fragment(index) => {
                let fragment = &self.fragments[index];
                let original_start = start - segment.generated_range.start;
                let original_end = end - segment.generated_range.start;
                DynamicFunctionMappedSource::Copied {
                    role: fragment.role,
                    original_range: DynamicFunctionByteRange::new(original_start, original_end),
                    origin: fragment.origin(),
                    text: fragment.text(),
                }
            }
        };
        DynamicFunctionMappedSegment {
            generated_range,
            source,
        }
    }
}

/// An internally generated dynamic-function wrapper and its fragment map.
///
/// The dedicated callback entry keeps this owner alive alongside the Oxc arena.
/// The map translates wrapper positions back to separately supplied parameter
/// and body fragments without using a lossy line/column approximation.
#[derive(Debug, Eq, PartialEq)]
pub struct PreparedDynamicFunctionSource {
    generated_source: String,
    fragment_map: DynamicFunctionFragmentMap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DynamicFunctionPreparationPlan {
    fragment_count: usize,
    generated_bytes: usize,
    generated_len: u32,
    segment_count: usize,
}

impl PreparedDynamicFunctionSource {
    fn prepare(
        source: DynamicFunctionSource<'_>,
        limits: FrontendLimits,
    ) -> Result<Self, FrontendError> {
        let plan = preflight_dynamic_function(source, limits)?;

        let mut generated_source = String::new();
        generated_source
            .try_reserve_exact(plan.generated_bytes)
            .map_err(|_| {
                FrontendError::from_limit(FrontendLimitError::DynamicFunctionAllocationFailed {
                    resource: DynamicFunctionPreparationResource::GeneratedBytes,
                    requested: plan.generated_bytes,
                })
            })?;
        let mut fragments = Vec::new();
        fragments
            .try_reserve_exact(plan.fragment_count)
            .map_err(|_| {
                FrontendError::from_limit(FrontendLimitError::DynamicFunctionAllocationFailed {
                    resource: DynamicFunctionPreparationResource::FragmentRecords,
                    requested: plan.fragment_count,
                })
            })?;
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(plan.segment_count)
            .map_err(|_| {
                FrontendError::from_limit(FrontendLimitError::DynamicFunctionAllocationFailed {
                    resource: DynamicFunctionPreparationResource::MapSegments,
                    requested: plan.segment_count,
                })
            })?;

        let mut generated_offset = 0_u32;
        append_synthetic_segment(
            &mut generated_source,
            &mut segments,
            &mut generated_offset,
            source.kind().wrapper_prefix(),
            DynamicFunctionSyntheticKind::Prefix,
        )?;
        for (index, fragment) in source.parameters().iter().copied().enumerate() {
            if index != 0 {
                append_synthetic_segment(
                    &mut generated_source,
                    &mut segments,
                    &mut generated_offset,
                    ",",
                    DynamicFunctionSyntheticKind::ParameterSeparator,
                )?;
            }
            let parameter_index = u32::try_from(index).map_err(|_| {
                FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
                    resource: DynamicFunctionPreparationResource::FragmentRecords,
                })
            })?;
            append_fragment_segment(
                &mut generated_source,
                &mut fragments,
                &mut segments,
                &mut generated_offset,
                fragment,
                DynamicFunctionFragmentRole::Parameter {
                    index: parameter_index,
                },
            )?;
        }
        append_synthetic_segment(
            &mut generated_source,
            &mut segments,
            &mut generated_offset,
            DYNAMIC_FUNCTION_PARAMETERS_BODY_SEPARATOR,
            DynamicFunctionSyntheticKind::ParametersBodySeparator,
        )?;
        append_fragment_segment(
            &mut generated_source,
            &mut fragments,
            &mut segments,
            &mut generated_offset,
            source.body(),
            DynamicFunctionFragmentRole::Body,
        )?;
        append_synthetic_segment(
            &mut generated_source,
            &mut segments,
            &mut generated_offset,
            DYNAMIC_FUNCTION_SUFFIX,
            DynamicFunctionSyntheticKind::Suffix,
        )?;

        debug_assert_eq!(generated_source.len(), plan.generated_bytes);
        debug_assert_eq!(generated_offset, plan.generated_len);
        Ok(Self {
            generated_source,
            fragment_map: DynamicFunctionFragmentMap {
                generated_len: plan.generated_len,
                fragments,
                segments,
            },
        })
    }

    /// Returns the generated JavaScript wrapper parsed by Oxc.
    #[must_use]
    pub fn generated_source(&self) -> &str {
        &self.generated_source
    }

    /// Returns the byte-exact generated-wrapper to input-fragment map.
    #[must_use]
    pub const fn fragment_map(&self) -> &DynamicFunctionFragmentMap {
        &self.fragment_map
    }
}

fn preflight_dynamic_function(
    source: DynamicFunctionSource<'_>,
    limits: FrontendLimits,
) -> Result<DynamicFunctionPreparationPlan, FrontendError> {
    let fragment_count = source.parameters().len().checked_add(1).ok_or_else(|| {
        FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
            resource: DynamicFunctionPreparationResource::FragmentRecords,
        })
    })?;
    if fragment_count > limits.max_dynamic_function_fragments() {
        return Err(FrontendError::from_limit(
            FrontendLimitError::DynamicFunctionFragmentsExceeded {
                actual: fragment_count,
                limit: limits.max_dynamic_function_fragments(),
            },
        ));
    }

    let fragment_bytes = checked_fragment_bytes(source)?;
    enforce_source_limit(fragment_bytes, limits)?;
    let origin_bytes = checked_origin_bytes(source)?;
    if origin_bytes > limits.max_dynamic_function_origin_bytes() {
        return Err(FrontendError::from_limit(
            FrontendLimitError::DynamicFunctionOriginBytesExceeded {
                actual: origin_bytes,
                limit: limits.max_dynamic_function_origin_bytes(),
            },
        ));
    }

    let parameter_separator_bytes = source.parameters().len().saturating_sub(1);
    let generated_bytes = source
        .kind()
        .wrapper_prefix()
        .len()
        .checked_add(fragment_bytes)
        .and_then(|total| total.checked_add(parameter_separator_bytes))
        .and_then(|total| total.checked_add(DYNAMIC_FUNCTION_PARAMETERS_BODY_SEPARATOR.len()))
        .and_then(|total| total.checked_add(DYNAMIC_FUNCTION_SUFFIX.len()))
        .ok_or_else(|| {
            FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
                resource: DynamicFunctionPreparationResource::GeneratedBytes,
            })
        })?;
    enforce_source_limit(generated_bytes, limits)?;
    let generated_len = u32::try_from(generated_bytes).map_err(|_| {
        FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
            resource: DynamicFunctionPreparationResource::GeneratedBytes,
        })
    })?;
    let segment_count = fragment_count
        .checked_add(parameter_separator_bytes)
        .and_then(|count| count.checked_add(3))
        .ok_or_else(|| {
            FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
                resource: DynamicFunctionPreparationResource::MapSegments,
            })
        })?;
    Ok(DynamicFunctionPreparationPlan {
        fragment_count,
        generated_bytes,
        generated_len,
        segment_count,
    })
}

fn checked_fragment_bytes(source: DynamicFunctionSource<'_>) -> Result<usize, FrontendError> {
    source
        .parameters()
        .iter()
        .try_fold(source.body().text().len(), |total, fragment| {
            total.checked_add(fragment.text().len())
        })
        .ok_or_else(|| {
            FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
                resource: DynamicFunctionPreparationResource::FragmentBytes,
            })
        })
}

fn checked_origin_bytes(source: DynamicFunctionSource<'_>) -> Result<usize, FrontendError> {
    source
        .parameters()
        .iter()
        .copied()
        .chain(std::iter::once(source.body()))
        .filter_map(SourceFragment::origin)
        .try_fold(0_usize, |total, origin| total.checked_add(origin.len()))
        .ok_or_else(|| {
            FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
                resource: DynamicFunctionPreparationResource::OriginBytes,
            })
        })
}

fn append_synthetic_segment(
    generated_source: &mut String,
    segments: &mut Vec<DynamicFunctionMapSegment>,
    generated_offset: &mut u32,
    text: &str,
    kind: DynamicFunctionSyntheticKind,
) -> Result<(), FrontendError> {
    let generated_range = append_generated_text(generated_source, generated_offset, text)?;
    segments.push(DynamicFunctionMapSegment {
        generated_range,
        source: DynamicFunctionSegmentSource::Synthetic(kind),
    });
    Ok(())
}

fn append_fragment_segment(
    generated_source: &mut String,
    fragments: &mut Vec<DynamicFunctionFragmentRecord>,
    segments: &mut Vec<DynamicFunctionMapSegment>,
    generated_offset: &mut u32,
    fragment: SourceFragment<'_>,
    role: DynamicFunctionFragmentRole,
) -> Result<(), FrontendError> {
    let generated_range =
        append_generated_text(generated_source, generated_offset, fragment.text())?;
    let text = try_copy_fragment_metadata(
        fragment.text(),
        DynamicFunctionPreparationResource::FragmentBytes,
    )?;
    let origin = fragment
        .origin()
        .map(|origin| {
            try_copy_fragment_metadata(origin, DynamicFunctionPreparationResource::OriginBytes)
        })
        .transpose()?;
    let fragment_index = fragments.len();
    fragments.push(DynamicFunctionFragmentRecord {
        role,
        generated_range,
        text,
        origin,
    });
    segments.push(DynamicFunctionMapSegment {
        generated_range,
        source: DynamicFunctionSegmentSource::Fragment(fragment_index),
    });
    Ok(())
}

fn append_generated_text(
    generated_source: &mut String,
    generated_offset: &mut u32,
    text: &str,
) -> Result<DynamicFunctionByteRange, FrontendError> {
    let text_len = u32::try_from(text.len()).map_err(|_| {
        FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
            resource: DynamicFunctionPreparationResource::GeneratedBytes,
        })
    })?;
    let end = generated_offset.checked_add(text_len).ok_or_else(|| {
        FrontendError::from_limit(FrontendLimitError::DynamicFunctionSizeOverflow {
            resource: DynamicFunctionPreparationResource::GeneratedBytes,
        })
    })?;
    let range = DynamicFunctionByteRange::new(*generated_offset, end);
    generated_source.push_str(text);
    *generated_offset = end;
    Ok(range)
}

fn try_copy_fragment_metadata(
    text: &str,
    resource: DynamicFunctionPreparationResource,
) -> Result<String, FrontendError> {
    let mut owned = String::new();
    owned.try_reserve_exact(text.len()).map_err(|_| {
        FrontendError::from_limit(FrontendLimitError::DynamicFunctionAllocationFailed {
            resource,
            requested: text.len(),
        })
    })?;
    owned.push_str(text);
    Ok(owned)
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
    source_bytes: usize,
    dynamic_function_fragments: usize,
    dynamic_function_origin_bytes: usize,
}

impl FrontendLimits {
    /// Creates limits with a caller-selected UTF-8 source-byte ceiling.
    #[must_use]
    pub const fn new(max_source_bytes: usize) -> Self {
        Self {
            source_bytes: max_source_bytes,
            dynamic_function_fragments: DEFAULT_MAX_DYNAMIC_FUNCTION_FRAGMENTS,
            dynamic_function_origin_bytes: DEFAULT_MAX_DYNAMIC_FUNCTION_ORIGIN_BYTES,
        }
    }

    /// Replaces the UTF-8 source-byte ceiling.
    #[must_use]
    pub const fn with_max_source_bytes(mut self, max_source_bytes: usize) -> Self {
        self.source_bytes = max_source_bytes;
        self
    }

    /// Returns the maximum UTF-8 bytes accepted by one source entry.
    #[must_use]
    pub const fn max_source_bytes(self) -> usize {
        self.source_bytes
    }

    /// Replaces the dynamic-function parameter/body fragment-count ceiling.
    #[must_use]
    pub const fn with_max_dynamic_function_fragments(mut self, maximum: usize) -> Self {
        self.dynamic_function_fragments = maximum;
        self
    }

    /// Returns the maximum number of retained dynamic-function fragments,
    /// including the body.
    #[must_use]
    pub const fn max_dynamic_function_fragments(self) -> usize {
        self.dynamic_function_fragments
    }

    /// Replaces the aggregate UTF-8 byte ceiling for retained fragment origin
    /// labels.
    #[must_use]
    pub const fn with_max_dynamic_function_origin_bytes(mut self, maximum: usize) -> Self {
        self.dynamic_function_origin_bytes = maximum;
        self
    }

    /// Returns the aggregate retained origin-label byte ceiling.
    #[must_use]
    pub const fn max_dynamic_function_origin_bytes(self) -> usize {
        self.dynamic_function_origin_bytes
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
    /// The requested compilation goal cannot be processed by this source entry.
    CompilationGoal,
    /// The isolated parser/semantic worker could not be created.
    IsolatedContext,
    /// Oxc's lexer or parser emitted a diagnostic.
    Parser,
    /// The AST uses syntax outside the pinned `QuickJS` compatibility profile.
    Profile,
    /// Oxc's deferred ECMAScript early-error checks emitted a diagnostic.
    Semantic,
    /// Copying Oxc module syntax into the arena-independent representation
    /// detected an inconsistent upstream record.
    ModuleSyntaxLowering,
}

impl fmt::Display for DiagnosticStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit => formatter.write_str("resource-limit"),
            Self::CompilationGoal => formatter.write_str("compilation-goal"),
            Self::IsolatedContext => formatter.write_str("isolated-context"),
            Self::Parser => formatter.write_str("parser"),
            Self::Profile => formatter.write_str("profile"),
            Self::Semantic => formatter.write_str("semantic"),
            Self::ModuleSyntaxLowering => formatter.write_str("module-syntax-lowering"),
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
    /// A dynamic function supplied too many parameter/body fragments.
    DynamicFunctionFragmentsExceeded,
    /// Dynamic-function origin labels exceeded their aggregate byte ceiling.
    DynamicFunctionOriginBytesExceeded,
    /// Dynamic-function wrapper preparation overflowed or could not allocate.
    DynamicFunctionPreparationFailed,
    /// The requested compilation goal cannot be processed by this source entry.
    UnsupportedCompilationGoal,
    /// The isolated Oxc parser/semantic worker could not be created.
    IsolatedContextUnavailable,
    /// A module declaration appeared in an asynchronous global Script.
    AsyncScriptModuleDeclaration,
    /// `await` appeared as an identifier or label in an asynchronous global Script.
    AsyncScriptAwaitIdentifier,
    /// `import.meta` appeared in an asynchronous global Script.
    AsyncScriptImportMeta,
    /// An Oxc lexer or parser diagnostic.
    OxcParser,
    /// An Oxc semantic/early-error diagnostic.
    OxcSemantic,
    /// The project-owned `RegExp` grammar rejected a literal as an early error.
    InvalidRegExpLiteral,
    /// A labeled `continue` chain does not terminate in an iteration statement.
    InvalidChainedContinueTarget,
    /// Oxc's AST and module record were inconsistent during owned lowering.
    ModuleSyntaxLowering,
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
    /// An Annex B HTML-style source comment.
    UnsupportedAnnexBHtmlComment,
    /// An Annex B legacy octal numeric literal or string escape.
    UnsupportedAnnexBLegacyOctal,
    /// A legacy `assert` import clause.
    UnsupportedLegacyImportAssertion,
    /// A string-literal imported name in a named re-export.
    UnsupportedStringNamedReExport,
    /// A string-literal namespace export name.
    UnsupportedStringNamespaceExport,
    /// A call or construction has more fixed prefix arguments than `QuickJS` can
    /// encode before its first spread.
    TooManyCallArguments,
}

impl FrontendDiagnosticCode {
    /// Returns the stable machine-readable code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceBytesExceeded => "quickjs::frontend::limit::source_bytes",
            Self::DynamicFunctionFragmentsExceeded => {
                "quickjs::frontend::limit::dynamic_function_fragments"
            }
            Self::DynamicFunctionOriginBytesExceeded => {
                "quickjs::frontend::limit::dynamic_function_origin_bytes"
            }
            Self::DynamicFunctionPreparationFailed => {
                "quickjs::frontend::limit::dynamic_function_preparation"
            }
            Self::UnsupportedCompilationGoal => "quickjs::frontend::unsupported_compilation_goal",
            Self::IsolatedContextUnavailable => "quickjs::frontend::isolated_context_unavailable",
            Self::AsyncScriptModuleDeclaration => {
                "quickjs::frontend::async_script::module_declaration"
            }
            Self::AsyncScriptAwaitIdentifier => "quickjs::frontend::async_script::await_identifier",
            Self::AsyncScriptImportMeta => "quickjs::frontend::async_script::import_meta",
            Self::OxcParser => "quickjs::frontend::oxc::parser",
            Self::OxcSemantic => "quickjs::frontend::oxc::semantic",
            Self::InvalidRegExpLiteral => "quickjs::frontend::regexp::invalid_literal",
            Self::InvalidChainedContinueTarget => {
                "quickjs::frontend::semantic::invalid_chained_continue_target"
            }
            Self::ModuleSyntaxLowering => "quickjs::frontend::lowering::module_syntax",
            Self::UnsupportedUsingDeclaration => "quickjs::frontend::profile::using_declaration",
            Self::UnsupportedAwaitUsingDeclaration => {
                "quickjs::frontend::profile::await_using_declaration"
            }
            Self::UnsupportedImportSource => "quickjs::frontend::profile::import_source",
            Self::UnsupportedImportDefer => "quickjs::frontend::profile::import_defer",
            Self::UnsupportedDecorator => "quickjs::frontend::profile::decorator",
            Self::UnsupportedClassAccessor => "quickjs::frontend::profile::class_accessor",
            Self::UnsupportedAnnexBHtmlComment => {
                "quickjs::frontend::profile::annex_b_html_comment"
            }
            Self::UnsupportedAnnexBLegacyOctal => {
                "quickjs::frontend::profile::annex_b_legacy_octal"
            }
            Self::UnsupportedLegacyImportAssertion => {
                "quickjs::frontend::profile::legacy_import_assertion"
            }
            Self::UnsupportedStringNamedReExport => {
                "quickjs::frontend::profile::string_named_reexport"
            }
            Self::UnsupportedStringNamespaceExport => {
                "quickjs::frontend::profile::string_namespace_export"
            }
            Self::TooManyCallArguments => "quickjs::frontend::profile::too_many_call_arguments",
        }
    }

    const fn help(self) -> Option<&'static str> {
        match self {
            Self::SourceBytesExceeded
            | Self::DynamicFunctionFragmentsExceeded
            | Self::DynamicFunctionOriginBytesExceeded
            | Self::DynamicFunctionPreparationFailed
            | Self::UnsupportedCompilationGoal
            | Self::IsolatedContextUnavailable
            | Self::AsyncScriptModuleDeclaration
            | Self::AsyncScriptAwaitIdentifier
            | Self::AsyncScriptImportMeta
            | Self::OxcParser
            | Self::OxcSemantic
            | Self::InvalidRegExpLiteral
            | Self::InvalidChainedContinueTarget
            | Self::ModuleSyntaxLowering => None,
            Self::TooManyCallArguments => Some(
                "reduce the fixed argument prefix or introduce a spread before it reaches 65,535 arguments",
            ),
            Self::UnsupportedUsingDeclaration
            | Self::UnsupportedAwaitUsingDeclaration
            | Self::UnsupportedImportSource
            | Self::UnsupportedImportDefer
            | Self::UnsupportedDecorator
            | Self::UnsupportedClassAccessor
            | Self::UnsupportedAnnexBHtmlComment
            | Self::UnsupportedAnnexBLegacyOctal
            | Self::UnsupportedLegacyImportAssertion
            | Self::UnsupportedStringNamedReExport
            | Self::UnsupportedStringNamespaceExport => {
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

/// A resource whose size or allocation is checked during dynamic-function
/// wrapper preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DynamicFunctionPreparationResource {
    /// Aggregate caller-supplied parameter/body bytes.
    FragmentBytes,
    /// Aggregate retained origin-label bytes.
    OriginBytes,
    /// Complete generated wrapper bytes.
    GeneratedBytes,
    /// Owned fragment records.
    FragmentRecords,
    /// Exact generated map segments.
    MapSegments,
}

impl fmt::Display for DynamicFunctionPreparationResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::FragmentBytes => "fragment bytes",
            Self::OriginBytes => "origin-label bytes",
            Self::GeneratedBytes => "generated wrapper bytes",
            Self::FragmentRecords => "fragment records",
            Self::MapSegments => "fragment-map segments",
        })
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
    /// A dynamic function supplied more fragments than the host permits.
    DynamicFunctionFragmentsExceeded {
        /// Parameter fragments plus the body fragment.
        actual: usize,
        /// Configured fragment-count ceiling.
        limit: usize,
    },
    /// Retained origin labels exceeded the configured aggregate byte ceiling.
    DynamicFunctionOriginBytesExceeded {
        /// Aggregate UTF-8 origin-label bytes.
        actual: usize,
        /// Configured aggregate byte ceiling.
        limit: usize,
    },
    /// Checked size arithmetic overflowed before allocation.
    DynamicFunctionSizeOverflow {
        /// Resource whose size could not be represented.
        resource: DynamicFunctionPreparationResource,
    },
    /// A fallible reservation failed before wrapper construction.
    DynamicFunctionAllocationFailed {
        /// Resource whose storage could not be reserved.
        resource: DynamicFunctionPreparationResource,
        /// Number of bytes or entries requested.
        requested: usize,
    },
}

impl fmt::Display for FrontendLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceBytesExceeded { actual, limit } => write!(
                formatter,
                "JavaScript source contains {actual} UTF-8 bytes, exceeding the configured limit of {limit} bytes"
            ),
            Self::DynamicFunctionFragmentsExceeded { actual, limit } => write!(
                formatter,
                "dynamic function contains {actual} parameter/body fragments, exceeding the configured limit of {limit}"
            ),
            Self::DynamicFunctionOriginBytesExceeded { actual, limit } => write!(
                formatter,
                "dynamic-function origin labels contain {actual} UTF-8 bytes, exceeding the configured limit of {limit} bytes"
            ),
            Self::DynamicFunctionSizeOverflow { resource } => write!(
                formatter,
                "dynamic-function {resource} cannot be represented on this platform"
            ),
            Self::DynamicFunctionAllocationFailed {
                resource,
                requested,
            } => write!(
                formatter,
                "could not reserve {requested} units for dynamic-function {resource}"
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
        Self::from_limit(FrontendLimitError::SourceBytesExceeded { actual, limit })
    }

    fn from_limit(limit_error: FrontendLimitError) -> Self {
        let code = match limit_error {
            FrontendLimitError::SourceBytesExceeded { .. } => {
                FrontendDiagnosticCode::SourceBytesExceeded
            }
            FrontendLimitError::DynamicFunctionFragmentsExceeded { .. } => {
                FrontendDiagnosticCode::DynamicFunctionFragmentsExceeded
            }
            FrontendLimitError::DynamicFunctionOriginBytesExceeded { .. } => {
                FrontendDiagnosticCode::DynamicFunctionOriginBytesExceeded
            }
            FrontendLimitError::DynamicFunctionSizeOverflow { .. }
            | FrontendLimitError::DynamicFunctionAllocationFailed { .. } => {
                FrontendDiagnosticCode::DynamicFunctionPreparationFailed
            }
        };
        Self {
            stage: DiagnosticStage::ResourceLimit,
            diagnostics: vec![FrontendDiagnostic {
                code,
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

    fn isolated_context_unavailable(error: &std::io::Error) -> Self {
        Self {
            stage: DiagnosticStage::IsolatedContext,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::IsolatedContextUnavailable,
                message: format!("could not create the isolated Oxc worker: {error}"),
                labels: Vec::new(),
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn async_script_module_syntax(span: Span) -> Self {
        Self {
            stage: DiagnosticStage::Parser,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::AsyncScriptModuleDeclaration,
                message: "module declarations are not allowed in an asynchronous global Script"
                    .to_owned(),
                labels: vec![DiagnosticLabel {
                    span,
                    message: None,
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn async_script_await_identifier(span: Span) -> Self {
        Self {
            stage: DiagnosticStage::Parser,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::AsyncScriptAwaitIdentifier,
                message: "`await` cannot be used as an identifier in an asynchronous global Script"
                    .to_owned(),
                labels: vec![DiagnosticLabel {
                    span,
                    message: None,
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn async_script_import_meta(span: Span) -> Self {
        Self {
            stage: DiagnosticStage::Parser,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::AsyncScriptImportMeta,
                message: "`import.meta` is not available in an asynchronous global Script"
                    .to_owned(),
                labels: vec![DiagnosticLabel {
                    span,
                    message: None,
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn invalid_chained_continue_target(span: Span) -> Self {
        Self {
            stage: DiagnosticStage::Semantic,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::InvalidChainedContinueTarget,
                message: "break/continue label not found".to_owned(),
                labels: vec![DiagnosticLabel {
                    span,
                    message: Some(
                        "this label chain does not terminate in an iteration statement".to_owned(),
                    ),
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn invalid_regexp_literal(span: Span, source: &quickjs_regexp::CompileError) -> Self {
        Self {
            stage: DiagnosticStage::Semantic,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::InvalidRegExpLiteral,
                message: source.to_string(),
                labels: vec![DiagnosticLabel {
                    span,
                    message: Some("this RegExp literal is not syntactically valid".to_owned()),
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
            limit_error: None,
        }
    }

    fn unsupported_annex_b_html_comment(span: Span) -> Self {
        Self {
            stage: DiagnosticStage::Profile,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::UnsupportedAnnexBHtmlComment,
                message: "Annex B HTML comments are not supported".to_owned(),
                labels: vec![DiagnosticLabel {
                    span,
                    message: Some("rewrite this as an ECMAScript comment".to_owned()),
                }],
            }],
            parser_panicked: false,
            unsupported_goal: None,
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

    fn from_module_syntax(error: ModuleSyntaxLoweringError) -> Self {
        Self {
            stage: DiagnosticStage::ModuleSyntaxLowering,
            diagnostics: vec![FrontendDiagnostic {
                code: FrontendDiagnosticCode::ModuleSyntaxLowering,
                message: error.to_string(),
                labels: vec![DiagnosticLabel {
                    span: error.span(),
                    message: Some("inconsistent Oxc module syntax".to_owned()),
                }],
            }],
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

/// A dynamic-function preparation or generated-Script parsing failure.
///
/// Parser, profile, and semantic failures retain the exact prepared wrapper
/// and fragment map so generated Oxc spans remain usable after the arena is
/// dropped. Preflight resource failures have no prepared source.
#[derive(Debug, Eq, PartialEq)]
pub struct DynamicFunctionError {
    frontend: FrontendError,
    prepared: Option<PreparedDynamicFunctionSource>,
}

impl DynamicFunctionError {
    fn preparation(frontend: FrontendError) -> Self {
        Self {
            frontend,
            prepared: None,
        }
    }

    fn generated(frontend: FrontendError, prepared: PreparedDynamicFunctionSource) -> Self {
        Self {
            frontend,
            prepared: Some(prepared),
        }
    }

    /// Returns the underlying normalized front-end failure.
    #[must_use]
    pub const fn frontend_error(&self) -> &FrontendError {
        &self.frontend
    }

    /// Returns the prepared wrapper for parser/profile/semantic failures.
    ///
    /// Resource failures detected before wrapper construction return `None`.
    #[must_use]
    pub const fn prepared_source(&self) -> Option<&PreparedDynamicFunctionSource> {
        self.prepared.as_ref()
    }

    /// Returns the phase that rejected the dynamic function.
    #[must_use]
    pub const fn stage(&self) -> DiagnosticStage {
        self.frontend.stage()
    }

    /// Returns all normalized front-end diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[FrontendDiagnostic] {
        self.frontend.diagnostics()
    }

    /// Returns whether Oxc stopped after an unrecoverable parser error.
    #[must_use]
    pub const fn parser_panicked(&self) -> bool {
        self.frontend.parser_panicked()
    }

    /// Returns the structured unsupported goal, when applicable.
    #[must_use]
    pub const fn unsupported_goal(&self) -> Option<UnsupportedCompilationGoal> {
        self.frontend.unsupported_goal()
    }

    /// Returns the structured resource failure, when applicable.
    #[must_use]
    pub const fn limit_error(&self) -> Option<FrontendLimitError> {
        self.frontend.limit_error()
    }

    /// Consumes the failure into its normalized error and optional prepared
    /// source.
    #[must_use]
    pub fn into_parts(self) -> (FrontendError, Option<PreparedDynamicFunctionSource>) {
        (self.frontend, self.prepared)
    }
}

impl fmt::Display for DynamicFunctionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.frontend.fmt(formatter)
    }
}

impl Error for DynamicFunctionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.frontend)
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

#[allow(
    clippy::too_many_lines,
    reason = "profile validation remains a single exhaustive AST decision table"
)]
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
            AstKind::ExportNamedDeclaration(declaration) if declaration.source.is_some() => {
                for specifier in &declaration.specifiers {
                    if let ModuleExportName::StringLiteral(literal) = &specifier.local {
                        violations.push(ProfileViolation {
                            span: literal.span,
                            code: FrontendDiagnosticCode::UnsupportedStringNamedReExport,
                            message: "QuickJS 2026-06-04 requires an identifier before `as` in a named re-export",
                        });
                    }
                }
            }
            AstKind::ExportAllDeclaration(declaration) => {
                if let Some(ModuleExportName::StringLiteral(literal)) = &declaration.exported {
                    violations.push(ProfileViolation {
                        span: literal.span,
                        code: FrontendDiagnosticCode::UnsupportedStringNamespaceExport,
                        message: "QuickJS 2026-06-04 requires an identifier namespace export name",
                    });
                }
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
            AstKind::NumericLiteral(literal)
                if is_annex_b_legacy_octal_numeric_literal(literal) =>
            {
                violations.push(ProfileViolation {
                    span: literal.span,
                    code: FrontendDiagnosticCode::UnsupportedAnnexBLegacyOctal,
                    message: "Annex B legacy octal literals are not supported",
                });
            }
            AstKind::StringLiteral(literal) if is_annex_b_legacy_octal_escape(literal) => {
                violations.push(ProfileViolation {
                    span: literal.span,
                    code: FrontendDiagnosticCode::UnsupportedAnnexBLegacyOctal,
                    message: "Annex B legacy octal escapes are not supported",
                });
            }
            AstKind::WithClause(clause) if clause.keyword == WithClauseKeyword::Assert => {
                violations.push(ProfileViolation {
                    span: clause.span,
                    code: FrontendDiagnosticCode::UnsupportedLegacyImportAssertion,
                    message: "QuickJS 2026-06-04 does not support legacy import assertions; use import attributes with `with`",
                });
            }
            AstKind::CallExpression(expression) => {
                push_call_argument_prefix_violation(&mut violations, &expression.arguments);
            }
            AstKind::NewExpression(expression) => {
                push_call_argument_prefix_violation(&mut violations, &expression.arguments);
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

fn is_annex_b_legacy_octal_numeric_literal(literal: &oxc_ast::ast::NumericLiteral<'_>) -> bool {
    let Some(raw) = literal.raw.as_ref().map(oxc_ast::ast::Str::as_str) else {
        return false;
    };
    let bytes = raw.as_bytes();
    bytes.len() > 1 && bytes[0] == b'0' && bytes[1].is_ascii_digit()
}

fn is_annex_b_legacy_octal_escape(literal: &oxc_ast::ast::StringLiteral<'_>) -> bool {
    let Some(raw) = literal.raw.as_ref().map(oxc_ast::ast::Str::as_str) else {
        return false;
    };
    let bytes = raw.as_bytes();
    let mut index = 1;
    while index + 1 < bytes.len() {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let mut slash_count = 1;
        index += 1;
        while index < bytes.len() && bytes[index] == b'\\' {
            slash_count += 1;
            index += 1;
        }
        if slash_count % 2 == 1
            && index < bytes.len()
            && (matches!(bytes[index], b'1'..=b'9')
                || (bytes[index] == b'0' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)))
        {
            return true;
        }
        index += 1;
    }
    false
}

fn push_call_argument_prefix_violation(
    violations: &mut Vec<ProfileViolation>,
    arguments: &[Argument<'_>],
) {
    for (fixed_arguments, argument) in arguments.iter().enumerate() {
        if fixed_arguments == MAX_FIXED_CALL_ARGUMENTS {
            violations.push(ProfileViolation {
                span: argument.span(),
                code: FrontendDiagnosticCode::TooManyCallArguments,
                message: "Too many call arguments",
            });
            return;
        }
        if argument.is_spread() {
            return;
        }
    }
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
pub struct ParsedUnit<'arena, 'scope> {
    goal: CompilationGoal<'scope>,
    program: &'arena Program<'arena>,
    module_record: ModuleRecord<'arena>,
    semantic: Semantic<'arena>,
    module_syntax: ModuleSyntaxRecord,
    synthetic_strict_directive: bool,
}

impl fmt::Debug for ParsedUnit<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedUnit")
            .field("goal", &self.goal)
            .field("program", &self.program)
            .field("module_record", &self.module_record)
            .field("semantic_nodes", &self.semantic.nodes().len())
            .field("semantic_scopes", &self.semantic.scoping().scopes_len())
            .field("semantic_symbols", &self.semantic.scoping().symbols_len())
            .field("module_syntax", &self.module_syntax)
            .field(
                "synthetic_strict_directive",
                &self.synthetic_strict_directive,
            )
            .finish()
    }
}

impl<'arena, 'scope> ParsedUnit<'arena, 'scope> {
    /// Returns the validated engine compilation goal.
    #[must_use]
    pub const fn goal(&self) -> CompilationGoal<'scope> {
        self.goal
    }

    /// Returns the arena-owned Oxc semantic program.
    ///
    /// A host-forced strict Script contains one zero-span synthetic
    /// `"use strict"` directive at index zero so published Oxc binds the
    /// program under strict rules. [`Self::source_directives`] omits that
    /// semantic sentinel.
    #[must_use]
    pub const fn program(&self) -> &Program<'arena> {
        self.program
    }

    /// Returns only directives that occur in the caller's source text.
    #[must_use]
    pub fn source_directives(&self) -> &[Directive<'arena>] {
        if self.synthetic_strict_directive {
            &self.program.directives[1..]
        } else {
            &self.program.directives
        }
    }

    /// Returns whether strict binding required a synthetic semantic directive.
    #[must_use]
    pub const fn has_synthetic_strict_directive(&self) -> bool {
        self.synthetic_strict_directive
    }

    /// Returns Oxc's parsed ECMAScript module record.
    ///
    /// Successful Script units retain dynamic-import entries; module-only
    /// declarations and `import.meta` still reject in Script mode.
    #[must_use]
    pub const fn module_record(&self) -> &ModuleRecord<'arena> {
        &self.module_record
    }

    /// Returns Oxc's complete semantic model.
    ///
    /// This includes AST-node mapping, scopes, symbols, references, and class
    /// private-name analysis. The compiler may consume it as syntax analysis
    /// input, but `QuickJS` runtime storage locations and declaration
    /// instantiation remain project-owned.
    #[must_use]
    pub const fn semantic(&self) -> &Semantic<'arena> {
        &self.semantic
    }

    /// Returns the QuickJS-owned, arena-independent static module syntax.
    ///
    /// Requests retain source occurrence order, attributes, and byte spans.
    /// Import and export entries retain the roles required by later module
    /// linking without copying Oxc's scope/symbol/reference model.
    #[must_use]
    pub const fn module_syntax(&self) -> &ModuleSyntaxRecord {
        &self.module_syntax
    }

    /// Returns Oxc's owned scope, symbol, and reference tables.
    #[must_use]
    pub fn scoping(&self) -> &Scoping {
        self.semantic.scoping()
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
/// any diagnostic. Goals that require a different contextual or
/// fragment-preparation entry return
/// [`FrontendDiagnosticCode::UnsupportedCompilationGoal`] before Oxc is
/// invoked.
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
    parse_in_mode(allocator, source_text, goal, mode, options.limits)
}

fn parse_in_mode<'arena, 'scope>(
    allocator: &'arena Allocator,
    source_text: &'arena str,
    goal: CompilationGoal<'scope>,
    mode: ParseMode,
    limits: FrontendLimits,
) -> Result<ParsedUnit<'arena, 'scope>, FrontendError> {
    enforce_source_limit(source_text.len(), limits)?;
    let (force_strict, allow_top_level_await) = match goal {
        CompilationGoal::GlobalScript(goal) => {
            (goal.forces_strict(), goal.allows_top_level_await())
        }
        CompilationGoal::Module
        | CompilationGoal::IndirectEval(_)
        | CompilationGoal::DirectEval(_)
        | CompilationGoal::DynamicFunction(_) => (false, false),
    };
    let mut parsed = if allow_top_level_await {
        parse_async_global_script(allocator, source_text, mode)
    } else {
        Parser::new(allocator, source_text, mode.source_type())
            .with_options(OxcParseOptions::default())
            .parse()
    };

    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(FrontendError::from_oxc(
            DiagnosticStage::Parser,
            FrontendDiagnosticCode::OxcParser,
            parsed.diagnostics,
            parsed.panicked,
        ));
    }
    if let Some(comment) = parsed
        .program
        .comments
        .iter()
        .find(|comment| is_script_html_comment(source_text, comment.span))
    {
        return Err(FrontendError::unsupported_annex_b_html_comment(
            comment.span,
        ));
    }
    if allow_top_level_await {
        if let Some(module_declaration) = parsed
            .program
            .body
            .iter()
            .find(|statement| statement.is_module_declaration())
        {
            return Err(FrontendError::async_script_module_syntax(
                module_declaration.span(),
            ));
        }
        parsed.program.source_type = mode.source_type();
    }
    let mut module_record = parsed.module_record;
    if allow_top_level_await {
        module_record.has_module_syntax = false;
    }
    let synthetic_strict_directive =
        force_strict && inject_forced_strict_directive(allocator, &mut parsed.program);
    let program: &'arena Program<'arena> = allocator.alloc(parsed.program);
    let semantic = SemanticBuilder::new_compiler()
        .with_build_nodes(true)
        .build(program);
    if allow_top_level_await
        && let Some(span) = async_script_await_identifier_span(&semantic.semantic)
    {
        return Err(FrontendError::async_script_await_identifier(span));
    }
    if allow_top_level_await && let Some(span) = async_script_import_meta_span(&semantic.semantic) {
        return Err(FrontendError::async_script_import_meta(span));
    }
    if let Some((span, error)) = invalid_regexp_literal(
        semantic.semantic.nodes(),
        limits.max_source_bytes().min(MAX_OXC_SOURCE_BYTES),
    ) {
        return Err(FrontendError::invalid_regexp_literal(span, &error));
    }
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
    if let Some(span) = invalid_chained_continue_target_span(semantic.semantic.nodes()) {
        return Err(FrontendError::invalid_chained_continue_target(span));
    }
    let semantic = semantic.semantic;
    let module_syntax = ModuleSyntaxRecord::from_oxc(program, &module_record)
        .map_err(FrontendError::from_module_syntax)?;

    Ok(ParsedUnit {
        goal,
        program,
        module_record,
        semantic,
        module_syntax,
        synthetic_strict_directive,
    })
}

fn invalid_regexp_literal(
    nodes: &AstNodes<'_>,
    max_pattern_bytes: usize,
) -> Option<(Span, quickjs_regexp::CompileError)> {
    nodes.iter().find_map(|node| {
        let AstKind::RegExpLiteral(literal) = node.kind() else {
            return None;
        };
        let flags = literal.regex.flags.to_string();
        quickjs_regexp::validate_literal(
            literal.regex.pattern.text.as_str(),
            &flags,
            max_pattern_bytes,
        )
        .err()
        .map(|error| (literal.span, error))
    })
}

fn parse_async_global_script<'arena>(
    allocator: &'arena Allocator,
    source_text: &'arena str,
    mode: ParseMode,
) -> ParserReturn<'arena> {
    let module = Parser::new(allocator, source_text, ParseMode::Module.source_type())
        .with_options(OxcParseOptions::default())
        .parse();
    if !module.panicked && module.diagnostics.is_empty() {
        return module;
    }

    let fallback = Parser::new(
        allocator,
        source_text,
        mode.source_type().with_unambiguous(true),
    )
    .with_options(OxcParseOptions::default())
    .parse();
    if !fallback.panicked && fallback.diagnostics.is_empty() {
        return fallback;
    }

    module
}

fn is_script_html_comment(source_text: &str, span: Span) -> bool {
    source_text
        .get(span.start as usize..span.end as usize)
        .is_some_and(|text| text.starts_with("<!--") || text.starts_with("-->"))
}

fn inject_forced_strict_directive<'arena>(
    allocator: &'arena Allocator,
    program: &mut Program<'arena>,
) -> bool {
    if program.has_use_strict_directive() {
        return false;
    }

    let builder = AstBuilder::new(allocator);
    program
        .directives
        .insert(0, Directive::new_use_strict(&builder));
    true
}

fn async_script_await_identifier_span(semantic: &Semantic<'_>) -> Option<Span> {
    let scoping = semantic.scoping();
    let root = scoping.root_scope_id();
    for symbol_id in scoping.symbol_ids() {
        if scoping.symbol_scope_id(symbol_id) == root && scoping.symbol_name(symbol_id) == "await" {
            return Some(scoping.symbol_span(symbol_id));
        }
    }

    if let Some(reference_id) = scoping
        .root_unresolved_references()
        .get("await")
        .and_then(|references| references.first())
    {
        let node_id = scoping.get_reference(*reference_id).node_id();
        return Some(semantic.nodes().get_node(node_id).kind().span());
    }

    semantic.nodes().iter().find_map(|node| {
        let AstKind::LabeledStatement(statement) = node.kind() else {
            return None;
        };
        if statement.label.name != "await"
            || scoping
                .scope_ancestors(node.scope_id())
                .any(|scope_id| scoping.scope_flags(scope_id).is_function())
        {
            return None;
        }
        Some(statement.label.span)
    })
}

fn async_script_import_meta_span(semantic: &Semantic<'_>) -> Option<Span> {
    semantic.nodes().iter().find_map(|node| {
        let AstKind::ImportMeta(import_meta) = node.kind() else {
            return None;
        };
        Some(import_meta.span)
    })
}

#[derive(Clone, Copy)]
struct ActiveContinueLabel {
    boundary: Option<NodeId>,
    ends_in_iteration: bool,
}

struct ContinueAncestor<'a> {
    node_id: NodeId,
    replaced_label: Option<(&'a str, Option<ActiveContinueLabel>)>,
    opens_boundary: bool,
}

fn leave_continue_ancestor<'a>(
    ancestor: &ContinueAncestor<'a>,
    active_labels: &mut HashMap<&'a str, ActiveContinueLabel>,
    boundaries: &mut Vec<NodeId>,
) {
    if let Some((name, previous)) = ancestor.replaced_label {
        if let Some(previous) = previous {
            active_labels.insert(name, previous);
        } else {
            active_labels.remove(name);
        }
    }
    if ancestor.opens_boundary {
        let popped = boundaries.pop();
        debug_assert_eq!(popped, Some(ancestor.node_id));
    }
}

fn chained_label_iteration_targets(nodes: &AstNodes<'_>) -> HashMap<NodeId, bool> {
    // Oxc accepts `outer: inner: switch (...) { continue outer; }`, while
    // QuickJS requires every label in a continue-target chain to end at an
    // iteration statement. Cache each chain once so adversarial nested labels
    // cannot make this compatibility check quadratic.
    let mut label_targets = HashMap::<NodeId, bool>::new();
    let mut uncached_chain = Vec::new();
    for node in nodes.iter() {
        let AstKind::LabeledStatement(statement) = node.kind() else {
            continue;
        };
        if label_targets.contains_key(&statement.node_id.get()) {
            continue;
        }

        uncached_chain.clear();
        let mut current = statement;
        let ends_in_iteration = loop {
            let node_id = current.node_id.get();
            if let Some(cached) = label_targets.get(&node_id) {
                break *cached;
            }
            uncached_chain.push(node_id);
            match &current.body {
                Statement::LabeledStatement(nested) => current = nested,
                body => break body.is_iteration_statement(),
            }
        };
        for node_id in uncached_chain.drain(..) {
            label_targets.insert(node_id, ends_in_iteration);
        }
    }
    label_targets
}

fn invalid_chained_continue_target_span(nodes: &AstNodes<'_>) -> Option<Span> {
    let label_targets = chained_label_iteration_targets(nodes);
    // Semantic nodes are stored in preorder. Maintain the active labels and
    // compilation boundary while advancing through that order; every node is
    // entered and left once, and each labelled continue is one hash lookup.
    let mut ancestors = Vec::<ContinueAncestor<'_>>::new();
    let mut active_labels = HashMap::<&str, ActiveContinueLabel>::new();
    let mut boundaries = Vec::<NodeId>::new();

    for (node_id, node) in nodes.iter_enumerated() {
        if !ancestors.is_empty() {
            let parent_id = nodes.parent_id(node_id);
            while ancestors
                .last()
                .is_some_and(|entry| entry.node_id != parent_id)
            {
                let ancestor = ancestors
                    .pop()
                    .expect("the non-empty ancestor stack has a final entry");
                leave_continue_ancestor(&ancestor, &mut active_labels, &mut boundaries);
            }
        }

        let kind = node.kind();
        let opens_boundary = kind.is_function_like() || matches!(kind, AstKind::StaticBlock(_));
        if opens_boundary {
            boundaries.push(node_id);
        }

        let replaced_label = if let AstKind::LabeledStatement(statement) = kind {
            let name = statement.label.name.as_str();
            let target = ActiveContinueLabel {
                boundary: boundaries.last().copied(),
                ends_in_iteration: label_targets
                    .get(&node_id)
                    .copied()
                    .unwrap_or_else(|| statement.body.is_iteration_statement()),
            };
            Some((name, active_labels.insert(name, target)))
        } else {
            None
        };

        if let AstKind::ContinueStatement(statement) = kind
            && let Some(label) = &statement.label
            && let Some(target) = active_labels.get(label.name.as_str())
            && target.boundary == boundaries.last().copied()
            && !target.ends_in_iteration
        {
            return Some(label.span);
        }

        ancestors.push(ContinueAncestor {
            node_id,
            replaced_label,
            opens_boundary,
        });
    }

    None
}

/// An owned execution boundary for Oxc parser and semantic work.
///
/// The context creates a short-lived worker with a dedicated stack for each
/// operation. Oxc arenas and semantic nodes never cross that worker boundary;
/// only the callback result may escape. This also keeps parser/semantic stack
/// use off runtime and host event-loop threads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IsolatedFrontendContext {
    stack_bytes: usize,
}

impl IsolatedFrontendContext {
    /// Creates the production isolated frontend context.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stack_bytes: DEFAULT_ISOLATED_FRONTEND_STACK_BYTES,
        }
    }

    /// Parses and validates a source unit inside an isolated short-lived arena.
    ///
    /// # Errors
    ///
    /// Returns the same parser and semantic diagnostics as [`parse`], or
    /// [`FrontendDiagnosticCode::IsolatedContextUnavailable`] if the worker
    /// cannot be created. A panic from `callback` resumes on the caller.
    pub fn with_parsed_program<'scope, R>(
        self,
        source_text: &str,
        options: FrontendOptions<'scope>,
        callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'scope>) -> R + Send,
    ) -> Result<R, FrontendError>
    where
        R: Send,
    {
        std::thread::scope(|scope| {
            let worker = std::thread::Builder::new()
                .name("quickjs-frontend".to_owned())
                .stack_size(self.stack_bytes)
                .spawn_scoped(scope, move || {
                    let allocator = Allocator::new();
                    let unit = parse(&allocator, source_text, options)?;
                    Ok(callback(&unit))
                })
                .map_err(|error| FrontendError::isolated_context_unavailable(&error))?;
            match worker.join() {
                Ok(result) => result,
                Err(payload) => std::panic::resume_unwind(payload),
            }
        })
    }
}

impl Default for IsolatedFrontendContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses and validates a source unit inside an isolated short-lived arena.
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
    callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'scope>) -> R + Send,
) -> Result<R, FrontendError>
where
    R: Send,
{
    IsolatedFrontendContext::new().with_parsed_program(source_text, options, callback)
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
    callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'scope>) -> R + Send,
) -> Result<R, RegisteredFrontendError>
where
    R: Send,
{
    let source = sources
        .source(source_id)
        .map_err(FrontendSourceError::Registry)
        .map_err(RegisteredFrontendError::Source)?;
    match with_parsed_program(source.text(), options, callback) {
        Ok(result) => Ok(result),
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
/// and body fragments are first assembled into the exact wrapper used by
/// `QuickJS` 2026-06-04 and accompanied by a byte-exact fragment map. Oxc then
/// parses the complete generated source as a Script. The callback receives both
/// the parsed unit and owning [`PreparedDynamicFunctionSource`].
///
/// The complete Script is intentionally not required to contain exactly one
/// function expression. The compatibility release allows constructor input to
/// escape the wrapper, so enforcing an AST shape here would be observably
/// incompatible.
///
/// # Errors
///
/// Returns a structured preflight resource error if wrapper construction
/// exceeds `limits` or cannot reserve storage. Parser/profile/semantic errors
/// retain the prepared source and map in [`DynamicFunctionError`].
#[allow(
    clippy::result_large_err,
    reason = "the error intentionally owns the prepared wrapper without another infallible allocation"
)]
pub fn with_dynamic_function_source<R>(
    source: DynamicFunctionSource<'_>,
    limits: FrontendLimits,
    callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'static>, &PreparedDynamicFunctionSource) -> R
    + Send,
) -> Result<R, DynamicFunctionError>
where
    R: Send,
{
    with_dynamic_function_source_and_prepared(source, limits, callback)
        .map(|(result, _prepared)| result)
}

/// Parses one exactly wrapped dynamic-function Script and returns its owner.
///
/// This ownership-preserving form lets a compiler return arena-independent
/// output together with the exact generated wrapper and fragment map without
/// cloning either allocation. Oxc identities remain confined to the callback
/// and isolated parser thread.
///
/// # Errors
///
/// Returns the same structured preparation, parser, profile, and semantic
/// failures as [`with_dynamic_function_source`].
#[allow(
    clippy::result_large_err,
    reason = "the error intentionally owns the prepared wrapper without another infallible allocation"
)]
pub fn with_dynamic_function_source_and_prepared<R>(
    source: DynamicFunctionSource<'_>,
    limits: FrontendLimits,
    callback: impl for<'arena> FnOnce(&ParsedUnit<'arena, 'static>, &PreparedDynamicFunctionSource) -> R
    + Send,
) -> Result<(R, PreparedDynamicFunctionSource), DynamicFunctionError>
where
    R: Send,
{
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("quickjs-dynamic-frontend".to_owned())
            .stack_size(DEFAULT_ISOLATED_FRONTEND_STACK_BYTES)
            .spawn_scoped(scope, move || {
                let prepared = PreparedDynamicFunctionSource::prepare(source, limits)
                    .map_err(DynamicFunctionError::preparation)?;
                let allocator = Allocator::new();
                let goal = CompilationGoal::DynamicFunction(source.kind());
                match parse_in_mode(
                    &allocator,
                    prepared.generated_source(),
                    goal,
                    ParseMode::Script,
                    limits,
                ) {
                    Ok(unit) => {
                        let result = callback(&unit, &prepared);
                        Ok((result, prepared))
                    }
                    Err(error) => Err(DynamicFunctionError::generated(error, prepared)),
                }
            })
            .map_err(|error| {
                DynamicFunctionError::preparation(FrontendError::isolated_context_unavailable(
                    &error,
                ))
            })?;
        match worker.join() {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
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
