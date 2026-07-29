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
use oxc_diagnostics::{Diagnostics, OxcDiagnostic};
use oxc_parser::{ParseOptions as OxcParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
pub use oxc_span::Span;

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
    /// Oxc's deferred ECMAScript early-error checks emitted a diagnostic.
    Semantic,
}

impl fmt::Display for DiagnosticStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parser => formatter.write_str("parser"),
            Self::Semantic => formatter.write_str("semantic"),
        }
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
    /// Primary diagnostic message.
    pub message: String,
    /// Labeled UTF-8 byte spans.
    pub labels: Vec<DiagnosticLabel>,
}

impl From<OxcDiagnostic> for FrontendDiagnostic {
    fn from(diagnostic: OxcDiagnostic) -> Self {
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
    fn from_oxc(stage: DiagnosticStage, diagnostics: Diagnostics, parser_panicked: bool) -> Self {
        let mut diagnostics = diagnostics
            .into_iter()
            .map(FrontendDiagnostic::from)
            .collect::<Vec<_>>();
        if diagnostics.is_empty() {
            diagnostics.push(FrontendDiagnostic {
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
}

impl fmt::Display for FrontendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} validation failed: {}",
            self.stage, self.diagnostics[0].message
        )
    }
}

impl Error for FrontendError {}

/// Parses and validates JavaScript using a caller-owned Oxc arena.
///
/// The returned AST borrows both `allocator` and `source_text`; callers must
/// keep both alive and must not reset the allocator while the AST is in use.
/// TypeScript, JSX, and unambiguous source-mode detection are not exposed.
///
/// # Errors
///
/// Returns an error if the parser emits any diagnostic (including a
/// recoverable one) or if deferred semantic early-error checking emits any
/// diagnostic.
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
            parsed.diagnostics,
            parsed.panicked,
        ));
    }

    let program = parsed.program;
    let semantic = SemanticBuilder::new_compiler().build(&program);
    if !semantic.diagnostics.is_empty() {
        return Err(FrontendError::from_oxc(
            DiagnosticStage::Semantic,
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
