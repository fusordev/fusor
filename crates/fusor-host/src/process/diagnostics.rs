//! Unified diagnostic rendering (§7.5): every error layer adapts onto
//! one miette `GraphicalReportHandler` pipeline with a documented color
//! policy. The CLI and REPL share this path; no caller formats its own
//! "top-level Display" bypass.
//!
//! The engine keeps miette out of `fusor-runtime`, so the adapters live
//! here in wrapper types. Each adapter carries its numeric error code
//! from the §12.1 classification ([`ErrorCode`]).

use std::fmt;
use std::io::IsTerminal;

use fusor_runtime::{ExecutionError, JsException};
use miette::{
    Diagnostic, GraphicalReportHandler, GraphicalTheme, LabeledSpan, NamedSource, Severity,
    SourceSpan,
};

use crate::ops::OpError;
use crate::process::error_codes::ErrorCode;

/// The ANSI color policy (§7.5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorPolicy {
    /// stderr being a TTY decides, with `NO_COLOR`/`--no-color` winning
    /// over TTY detection.
    Auto,
    /// Always emit ANSI colors.
    Always,
    /// Never emit ANSI colors.
    Never,
}

impl ColorPolicy {
    /// Resolves `Auto` against the environment (§7.5).
    ///
    /// Explicit `Always`/`Never` win over both inputs.
    #[must_use]
    pub const fn resolve(self, stderr_is_tty: bool, no_color_env: bool) -> Self {
        match self {
            Self::Auto => {
                if stderr_is_tty && !no_color_env {
                    Self::Always
                } else {
                    Self::Never
                }
            }
            resolved => resolved,
        }
    }

    /// The policy the host applies by default (§7.5): `Auto` resolved
    /// against stderr's TTY status and the `NO_COLOR` environment
    /// variable. This function never panics.
    #[must_use]
    pub fn from_env() -> Self {
        Self::Auto.resolve(
            std::io::stderr().is_terminal(),
            std::env::var_os("NO_COLOR").is_some(),
        )
    }

    /// The miette graphical theme for the resolved policy.
    fn theme(self) -> GraphicalTheme {
        match self {
            Self::Always => GraphicalTheme::unicode(),
            Self::Auto | Self::Never => GraphicalTheme::unicode_nocolor(),
        }
    }
}

/// Renders one adapted diagnostic through the single §7.5 pipeline.
///
/// This function never panics and returns the complete graphical report
/// as a `String` (with a trailing newline).
pub fn render_diagnostic<E>(diagnostic: E, policy: ColorPolicy) -> String
where
    E: Diagnostic,
{
    let mut output = String::new();
    let _ = GraphicalReportHandler::new_themed(policy.theme())
        .render_report(&mut output, &diagnostic);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

/// The evaluation-layer adapter (§7.5): one [`ExecutionError`] rendered
/// with its JavaScript exception frames as source labels. Non-exception
/// failures render their message without labels.
#[derive(Debug)]
pub struct HostDiagnostic {
    error: ExecutionError,
    /// The origin frame's named retained source text, owned here so the
    /// handler can render the labeled span inside it.
    source: Option<NamedSource<String>>,
}

impl HostDiagnostic {
    /// Wraps one execution error.
    #[must_use]
    pub fn new(error: ExecutionError) -> Self {
        let source = match &error {
            ExecutionError::Exception(exception) => Some(NamedSource::new(
                exception.source_name().to_owned(),
                exception.source_text().to_owned(),
            )),
            _ => None,
        };
        Self { error, source }
    }

    /// The uncaught exception, when this error is one.
    fn exception(&self) -> Option<&JsException> {
        match &self.error {
            ExecutionError::Exception(exception) => Some(exception),
            _ => None,
        }
    }
}

impl fmt::Display for HostDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The raw ExecutionError Display flattens exceptions to
        // "uncaught JavaScript value"; the diagnostic carries the exact
        // class and message so the report names the failure (§7.5). The
        // numeric error code is folded into the header line — a standalone
        // miette `code()` would render on its own floating line and shift
        // REPL input.
        let code = ErrorCode::from_execution_error(&self.error);
        match self.exception() {
            Some(exception) => match (exception.kind(), exception.message()) {
                (Some(kind), Some(message)) => write!(
                    formatter,
                    "{code} uncaught {kind:?}: {}",
                    message.to_utf8_lossy().unwrap_or_default()
                ),
                _ => write!(formatter, "{code} uncaught JavaScript value"),
            },
            None => write!(formatter, "{code} {error}", error = self.error),
        }
    }
}

impl std::error::Error for HostDiagnostic {}

impl Diagnostic for HostDiagnostic {
    fn severity(&self) -> Option<Severity> {
        Some(Severity::Error)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let exception = self.exception()?;
        let mut spans = Vec::new();
        spans.push(LabeledSpan::new_with_span(
            Some("uncaught here".to_owned()),
            miette_span(exception.source_span()),
        ));
        for frame in exception.caller_frames() {
            spans.push(LabeledSpan::new_with_span(
                Some(format!("called from {}", frame.source_name())),
                miette_span(frame.source_span()),
            ));
        }
        Some(Box::new(spans.into_iter()))
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        // The origin frame's retained source text; miette renders the
        // labeled span inside it with context lines.
        let source: &dyn miette::SourceCode = self.source.as_ref()?;
        Some(source)
    }
}

/// The op-layer adapter (§7.5): one [`OpError`] rendered with its class
/// and message.
#[derive(Debug)]
pub struct OpDiagnostic {
    error: OpError,
}

impl OpDiagnostic {
    /// Wraps one op error.
    #[must_use]
    pub const fn new(error: OpError) -> Self {
        Self { error }
    }
}

impl fmt::Display for OpDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for OpDiagnostic {}

impl Diagnostic for OpDiagnostic {
    fn code(&self) -> Option<Box<dyn fmt::Display + '_>> {
        Some(match self.error.code {
            Some(code) => Box::new(code) as Box<dyn fmt::Display + '_>,
            None => Box::new(ErrorCode::OpFailure) as Box<dyn fmt::Display + '_>,
        })
    }

    fn severity(&self) -> Option<Severity> {
        Some(Severity::Error)
    }
}

/// A message-only diagnostic for the loop's default paths (§7.3): an
/// uncaught value or an unhandled rejection reason renders through the
/// same §7.5 pipeline without frame labels.
#[derive(Debug)]
pub struct MessageDiagnostic {
    message: String,
}

impl MessageDiagnostic {
    /// Creates a message-only diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MessageDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MessageDiagnostic {}

impl Diagnostic for MessageDiagnostic {
    fn severity(&self) -> Option<Severity> {
        Some(Severity::Error)
    }
}

/// Converts one verified half-open UTF-8 source span into a miette span.
fn miette_span(span: fusor_bytecode::SourceByteSpan) -> SourceSpan {
    let start = span.start() as usize;
    let end = span.end() as usize;
    SourceSpan::from((start, end.saturating_sub(start)))
}
