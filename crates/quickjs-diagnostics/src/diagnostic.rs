use std::{error::Error, fmt, sync::Arc};

use miette::{
    Diagnostic as MietteDiagnostic, GraphicalReportHandler, GraphicalTheme, LabeledSpan,
    NamedSource, Severity,
};

use crate::{SourceError, SourceMapError, SourceRegistry, SourceSpan};

/// A validated stable diagnostic code.
///
/// Codes are non-empty ASCII identifiers containing letters, digits, `:`,
/// `_`, `-`, or `.`. Rust-path-style values such as
/// `quickjs::parser::unexpected_token` are recommended.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticCode(Arc<str>);

impl DiagnosticCode {
    /// Validates a stable diagnostic code.
    ///
    /// # Errors
    ///
    /// Returns an error when the code is empty or contains a character outside
    /// the documented stable ASCII alphabet.
    pub fn new(code: impl Into<Arc<str>>) -> Result<Self, DiagnosticCodeError> {
        let code = code.into();
        if code.is_empty() {
            return Err(DiagnosticCodeError::Empty);
        }
        if let Some((index, character)) = code.char_indices().find(|(_, character)| {
            !(character.is_ascii_alphanumeric() || matches!(character, ':' | '_' | '-' | '.'))
        }) {
            return Err(DiagnosticCodeError::InvalidCharacter { index, character });
        }
        Ok(Self(code))
    }

    /// Returns the code as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validation failures for [`DiagnosticCode`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCodeError {
    /// The code was empty.
    Empty,
    /// The code contained a character outside the stable ASCII alphabet.
    InvalidCharacter {
        /// UTF-8 byte offset of the character.
        index: usize,
        /// Invalid character.
        character: char,
    },
}

impl fmt::Display for DiagnosticCodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("diagnostic code must not be empty"),
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "diagnostic code contains invalid character `{character}` at byte {index}"
            ),
        }
    }
}

impl Error for DiagnosticCodeError {}

/// Stable severity independent of a particular renderer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DiagnosticSeverity {
    /// Compilation or execution cannot continue.
    Error,
    /// Suspicious input that does not necessarily prevent continuation.
    Warning,
    /// Non-critical guidance.
    Advice,
}

/// A source span and optional explanatory label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLabel {
    span: SourceSpan,
    message: Option<String>,
    primary: bool,
}

impl DiagnosticLabel {
    /// Creates a primary label.
    #[must_use]
    pub fn primary(span: SourceSpan, message: impl Into<Option<String>>) -> Self {
        Self {
            span,
            message: message.into(),
            primary: true,
        }
    }

    /// Creates a secondary label.
    #[must_use]
    pub fn secondary(span: SourceSpan, message: impl Into<Option<String>>) -> Self {
        Self {
            span,
            message: message.into(),
            primary: false,
        }
    }

    /// Returns the source-qualified span.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }

    /// Returns the optional label message.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Returns whether this is a primary label.
    #[must_use]
    pub const fn is_primary(&self) -> bool {
        self.primary
    }
}

/// A renderer-independent compiler/runtime diagnostic.
///
/// The canonical message, code, severity, help, and UTF-8 byte labels are
/// stable data. Pretty renderer output is intentionally not a semantic
/// compatibility surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: DiagnosticCode,
    severity: DiagnosticSeverity,
    message: String,
    help: Option<String>,
    labels: Vec<DiagnosticLabel>,
}

impl Diagnostic {
    /// Creates a diagnostic without help or labels.
    #[must_use]
    pub fn new(
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            help: None,
            labels: Vec::new(),
        }
    }

    /// Adds or replaces help text.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Appends a label.
    #[must_use]
    pub fn with_label(mut self, label: DiagnosticLabel) -> Self {
        self.labels.push(label);
        self
    }

    /// Extends the label list.
    #[must_use]
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = DiagnosticLabel>) -> Self {
        self.labels.extend(labels);
        self
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &DiagnosticCode {
        &self.code
    }

    /// Returns the stable severity.
    #[must_use]
    pub const fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    /// Returns the canonical human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns optional corrective guidance.
    #[must_use]
    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }

    /// Returns all source labels.
    #[must_use]
    pub fn labels(&self) -> &[DiagnosticLabel] {
        &self.labels
    }

    /// Creates an owned Miette adapter.
    ///
    /// # Errors
    ///
    /// Miette associates one source with a diagnostic's labels. This adapter
    /// therefore rejects labels spanning multiple registered sources. Callers
    /// can group such labels into related per-source diagnostics.
    pub fn to_pretty(
        &self,
        sources: &SourceRegistry,
    ) -> Result<PrettyDiagnostic, PrettyDiagnosticError> {
        PrettyDiagnostic::new(sources, self.clone())
    }

    /// Resolves every label through the registered incoming source-map chain.
    ///
    /// A label uses its deepest registered mapped range when both endpoints
    /// resolve into one source. Otherwise the validated generated range is
    /// retained as the safe fallback.
    ///
    /// # Errors
    ///
    /// Returns a structured source-map error for invalid positions, malformed
    /// map state, cycles, or excessive chain depth.
    pub fn resolve_source_maps(&self, sources: &SourceRegistry) -> Result<Self, SourceMapError> {
        let labels = self
            .labels
            .iter()
            .map(|label| {
                let resolved = sources.resolve_span(&label.span)?;
                Ok(DiagnosticLabel {
                    span: resolved.display_span().clone(),
                    message: label.message.clone(),
                    primary: label.primary,
                })
            })
            .collect::<Result<Vec<_>, SourceMapError>>()?;
        Ok(Self {
            code: self.code.clone(),
            severity: self.severity,
            message: self.message.clone(),
            help: self.help.clone(),
            labels,
        })
    }
}

/// One primary diagnostic with zero or more related diagnostics.
///
/// Related diagnostics allow each stack frame or compilation note to retain
/// its own source, which matches Miette's one-source-per-diagnostic model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticReport {
    primary: Diagnostic,
    related: Vec<Diagnostic>,
}

impl DiagnosticReport {
    /// Creates a report around one primary diagnostic.
    #[must_use]
    pub const fn new(primary: Diagnostic) -> Self {
        Self {
            primary,
            related: Vec::new(),
        }
    }

    /// Appends a related diagnostic.
    #[must_use]
    pub fn with_related(mut self, diagnostic: Diagnostic) -> Self {
        self.related.push(diagnostic);
        self
    }

    /// Extends the related diagnostic list.
    #[must_use]
    pub fn with_related_diagnostics(
        mut self,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Self {
        self.related.extend(diagnostics);
        self
    }

    /// Returns the primary diagnostic.
    #[must_use]
    pub const fn primary(&self) -> &Diagnostic {
        &self.primary
    }

    /// Returns related diagnostics in presentation order.
    #[must_use]
    pub fn related(&self) -> &[Diagnostic] {
        &self.related
    }

    /// Resolves all primary and related labels through registered source maps.
    ///
    /// # Errors
    ///
    /// Returns the first source-map resolution failure.
    pub fn resolve_source_maps(&self, sources: &SourceRegistry) -> Result<Self, SourceMapError> {
        Ok(Self {
            primary: self.primary.resolve_source_maps(sources)?,
            related: self
                .related
                .iter()
                .map(|diagnostic| diagnostic.resolve_source_maps(sources))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    /// Creates an owned Miette report with related diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an adapter error for a missing, foreign, or mixed source.
    pub fn to_pretty(
        &self,
        sources: &SourceRegistry,
    ) -> Result<PrettyDiagnosticReport, PrettyDiagnosticError> {
        PrettyDiagnosticReport::new(sources, self.clone())
    }
}

/// An owned [`miette::Diagnostic`] adapter for a stable [`Diagnostic`].
#[derive(Debug)]
pub struct PrettyDiagnostic {
    diagnostic: Diagnostic,
    source: Option<NamedSource<Arc<str>>>,
    labels: Vec<LabeledSpan>,
}

impl PrettyDiagnostic {
    fn new(
        sources: &SourceRegistry,
        diagnostic: Diagnostic,
    ) -> Result<Self, PrettyDiagnosticError> {
        let Some(first_label) = diagnostic.labels.first() else {
            return Ok(Self {
                diagnostic,
                source: None,
                labels: Vec::new(),
            });
        };
        let source_id = first_label.span.source_id();
        if diagnostic
            .labels
            .iter()
            .any(|label| label.span.source_id() != source_id)
        {
            return Err(PrettyDiagnosticError::MultipleSources);
        }
        let source_file = sources
            .source(source_id)
            .map_err(PrettyDiagnosticError::Source)?;
        let source = NamedSource::new(source_file.display_name(), Arc::clone(source_file.text()))
            .with_language("JavaScript");
        let labels = diagnostic
            .labels
            .iter()
            .map(|label| {
                let bytes = label.span.bytes();
                let span = (bytes.start() as usize, bytes.len() as usize);
                if label.primary {
                    LabeledSpan::new_primary_with_span(label.message.clone(), span)
                } else {
                    LabeledSpan::new_with_span(label.message.clone(), span)
                }
            })
            .collect();
        Ok(Self {
            diagnostic,
            source: Some(source),
            labels,
        })
    }

    /// Returns the stable underlying diagnostic.
    #[must_use]
    pub const fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}

impl fmt::Display for PrettyDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.diagnostic.message)
    }
}

impl Error for PrettyDiagnostic {}

impl MietteDiagnostic for PrettyDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.diagnostic.code.as_str()))
    }

    fn severity(&self) -> Option<Severity> {
        Some(match self.diagnostic.severity {
            DiagnosticSeverity::Error => Severity::Error,
            DiagnosticSeverity::Warning => Severity::Warning,
            DiagnosticSeverity::Advice => Severity::Advice,
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.diagnostic
            .help
            .as_deref()
            .map(|help| Box::new(help) as Box<dyn fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.source
            .as_ref()
            .map(|source| source as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        (!self.labels.is_empty()).then(|| Box::new(self.labels.iter().cloned()) as Box<_>)
    }
}

/// An owned Miette adapter for a [`DiagnosticReport`].
#[derive(Debug)]
pub struct PrettyDiagnosticReport {
    report: DiagnosticReport,
    primary: PrettyDiagnostic,
    related: Vec<PrettyDiagnostic>,
}

impl PrettyDiagnosticReport {
    fn new(
        sources: &SourceRegistry,
        report: DiagnosticReport,
    ) -> Result<Self, PrettyDiagnosticError> {
        let primary = PrettyDiagnostic::new(sources, report.primary.clone())?;
        let related = report
            .related
            .iter()
            .cloned()
            .map(|diagnostic| PrettyDiagnostic::new(sources, diagnostic))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            report,
            primary,
            related,
        })
    }

    /// Returns the stable underlying report.
    #[must_use]
    pub const fn report(&self) -> &DiagnosticReport {
        &self.report
    }
}

impl fmt::Display for PrettyDiagnosticReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.primary.fmt(formatter)
    }
}

impl Error for PrettyDiagnosticReport {}

impl MietteDiagnostic for PrettyDiagnosticReport {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        MietteDiagnostic::code(&self.primary)
    }

    fn severity(&self) -> Option<Severity> {
        MietteDiagnostic::severity(&self.primary)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        MietteDiagnostic::help(&self.primary)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        MietteDiagnostic::source_code(&self.primary)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        MietteDiagnostic::labels(&self.primary)
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn MietteDiagnostic> + 'a>> {
        (!self.related.is_empty()).then(|| {
            Box::new(
                self.related
                    .iter()
                    .map(|diagnostic| diagnostic as &dyn MietteDiagnostic),
            ) as Box<_>
        })
    }
}

/// Renders deterministic, color-free graphical output with source snippets.
///
/// This explicit adapter avoids process-global Miette hooks and is suitable for
/// command-line tools, tests, and embedding hosts that want a ready-to-display
/// string.
///
/// # Errors
///
/// Returns an adapter error for missing/mixed sources or an unexpected
/// formatting failure.
pub fn render_pretty(
    sources: &SourceRegistry,
    diagnostic: &Diagnostic,
) -> Result<String, PrettyDiagnosticError> {
    let pretty = diagnostic.to_pretty(sources)?;
    let handler = GraphicalReportHandler::new_themed(GraphicalTheme::none())
        .with_width(100)
        .with_context_lines(2)
        .with_links(false)
        .with_urls(false)
        .without_cause_chain()
        .without_syntax_highlighting();
    let mut rendered = String::new();
    handler
        .render_report(&mut rendered, &pretty)
        .map_err(|_| PrettyDiagnosticError::Render)?;
    Ok(rendered)
}

/// Renders a primary diagnostic and its related diagnostics as deterministic,
/// color-free Miette output.
///
/// # Errors
///
/// Returns an adapter error for missing/mixed sources or an unexpected
/// formatting failure.
pub fn render_pretty_report(
    sources: &SourceRegistry,
    report: &DiagnosticReport,
) -> Result<String, PrettyDiagnosticError> {
    let pretty = report.to_pretty(sources)?;
    let handler = GraphicalReportHandler::new_themed(GraphicalTheme::none())
        .with_width(100)
        .with_context_lines(2)
        .with_links(false)
        .with_urls(false)
        .without_cause_chain()
        .without_syntax_highlighting();
    let mut rendered = String::new();
    handler
        .render_report(&mut rendered, &pretty)
        .map_err(|_| PrettyDiagnosticError::Render)?;
    Ok(rendered)
}

/// Failures while adapting a stable diagnostic for pretty rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PrettyDiagnosticError {
    /// A label referenced an invalid or foreign source.
    Source(SourceError),
    /// Labels referenced more than one source.
    MultipleSources,
    /// The renderer failed to write its string output.
    Render,
}

impl fmt::Display for PrettyDiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "cannot render diagnostic source: {error}"),
            Self::MultipleSources => formatter
                .write_str("one pretty diagnostic cannot contain labels from multiple sources"),
            Self::Render => formatter.write_str("pretty diagnostic renderer failed"),
        }
    }
}

impl Error for PrettyDiagnosticError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::MultipleSources | Self::Render => None,
        }
    }
}
