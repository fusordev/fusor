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
    DiagnosticLabel as SharedDiagnosticLabel, DiagnosticSeverity, SourceError, SourceId,
    SourceRegistry,
};

/// The ECMAScript parse goal.
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

/// Options accepted by the engine's JavaScript-only front end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontendOptions {
    mode: ParseMode,
    allow_top_level_return: bool,
}

impl FrontendOptions {
    /// Creates options for an explicit Script or Module parse goal.
    #[must_use]
    pub const fn new(mode: ParseMode) -> Self {
        Self {
            mode,
            allow_top_level_return: false,
        }
    }

    /// Allows a top-level `return`, as required when parsing a Function
    /// constructor body.
    ///
    /// Engine callers should enable this only for that grammar context.
    #[must_use]
    pub const fn with_top_level_return(mut self, yes: bool) -> Self {
        self.allow_top_level_return = yes;
        self
    }

    /// Returns the selected parse goal.
    #[must_use]
    pub const fn mode(self) -> ParseMode {
        self.mode
    }

    /// Returns whether Function-constructor-style top-level returns are
    /// enabled.
    #[must_use]
    pub const fn allows_top_level_return(self) -> bool {
        self.allow_top_level_return
    }
}

impl Default for FrontendOptions {
    fn default() -> Self {
        Self::new(ParseMode::Script)
    }
}

/// The validation phase that rejected a source unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticStage {
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
/// into QuickJS-exact diagnostic kinds. Compatibility-profile exclusions have
/// one identity per excluded syntax feature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FrontendDiagnosticCode {
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

    const fn profile_help(self) -> Option<&'static str> {
        match self {
            Self::OxcParser | Self::OxcSemantic => None,
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

/// A rejected JavaScript source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendError {
    stage: DiagnosticStage,
    diagnostics: Vec<FrontendDiagnostic>,
    parser_panicked: bool,
}

impl FrontendError {
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
        }
    }

    fn from_profile(diagnostics: Vec<FrontendDiagnostic>) -> Self {
        Self {
            stage: DiagnosticStage::Profile,
            diagnostics,
            parser_panicked: false,
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

impl Error for FrontendError {}

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
    if let Some(help) = diagnostic.code.profile_help() {
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

impl Error for RegisteredFrontendDiagnostics {}

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

/// Parses and validates JavaScript using a caller-owned Oxc arena.
///
/// The returned AST borrows both `allocator` and `source_text`; callers must
/// keep both alive and must not reset the allocator while the AST is in use.
/// TypeScript, JSX, and unambiguous source-mode detection are not exposed.
///
/// # Errors
///
/// Returns an error if the parser emits any diagnostic (including a
/// recoverable one), if the AST uses syntax outside the pinned `QuickJS`
/// compatibility profile, or if deferred semantic early-error checking emits
/// any diagnostic.
pub fn parse<'arena>(
    allocator: &'arena Allocator,
    source_text: &'arena str,
    options: FrontendOptions,
) -> Result<Program<'arena>, FrontendError> {
    let parser_options = OxcParseOptions {
        allow_return_outside_function: options.mode == ParseMode::Script
            && options.allow_top_level_return,
        ..OxcParseOptions::default()
    };
    let parsed = Parser::new(allocator, source_text, options.mode.source_type())
        .with_options(parser_options)
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

    Ok(program)
}

/// Parses and validates a source unit inside a short-lived arena.
///
/// The higher-ranked callback cannot return a value that borrows the AST, so
/// arena-backed nodes cannot escape this function.
///
/// # Errors
///
/// Returns the same parser or semantic diagnostics as [`parse`].
pub fn with_parsed_program<R>(
    source_text: &str,
    options: FrontendOptions,
    callback: impl for<'arena> FnOnce(&Program<'arena>) -> R,
) -> Result<R, FrontendError> {
    let allocator = Allocator::new();
    let program = parse(&allocator, source_text, options)?;
    Ok(callback(&program))
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
pub fn with_registered_program<R>(
    sources: &SourceRegistry,
    source_id: &SourceId,
    options: FrontendOptions,
    callback: impl for<'arena> FnOnce(&Program<'arena>) -> R,
) -> Result<R, RegisteredFrontendError> {
    let source = sources
        .source(source_id)
        .map_err(FrontendSourceError::Registry)
        .map_err(RegisteredFrontendError::Source)?;
    let allocator = Allocator::new();
    match parse(&allocator, source.text(), options) {
        Ok(program) => Ok(callback(&program)),
        Err(error) => {
            let diagnostics = error
                .into_registered_diagnostics(sources, source_id)
                .map_err(RegisteredFrontendError::Source)?;
            Err(RegisteredFrontendError::Diagnostics(diagnostics))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticLabel, DiagnosticStage, FrontendDiagnostic, FrontendDiagnosticCode,
        FrontendError, FrontendSourceError,
    };
    use oxc_span::Span;
    use quickjs_diagnostics::SourceRegistry;

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
