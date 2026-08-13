use std::{error::Error, fmt};

use fusor_diagnostics::{
    Diagnostic, DiagnosticCode, DiagnosticCodeError, DiagnosticLabel, DiagnosticReport,
    DiagnosticSeverity, SourceError, SourceMapError, SourceRegistry, SourceSpan,
};

use crate::{
    ExceptionKind, ExecutionError, GlobalScriptError, InstallError, JsException, JsStackFrame,
    RuntimeError,
};

/// Failures while converting runtime provenance into shared diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeDiagnosticError {
    /// A stable engine-owned diagnostic code failed validation.
    DiagnosticCode(DiagnosticCodeError),
    /// A runtime frame names a source absent from the supplied registry.
    MissingSource {
        /// Exact retained source display name.
        source_name: String,
    },
    /// A registry entry reused a runtime frame's display name for different
    /// source text.
    SourceTextMismatch {
        /// Exact retained source display name.
        source_name: String,
    },
    /// A retained byte span was invalid for its exact registered source.
    Source(SourceError),
    /// Chained source-map resolution failed.
    SourceMap(SourceMapError),
}

impl fmt::Display for RuntimeDiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiagnosticCode(error) => {
                write!(formatter, "runtime diagnostic code is invalid: {error}")
            }
            Self::MissingSource { source_name } => write!(
                formatter,
                "runtime frame source `{source_name}` is not registered"
            ),
            Self::SourceTextMismatch { source_name } => write!(
                formatter,
                "runtime frame source `{source_name}` does not match the registered text"
            ),
            Self::Source(error) => write!(formatter, "runtime diagnostic span is invalid: {error}"),
            Self::SourceMap(error) => {
                write!(
                    formatter,
                    "runtime diagnostic source-map resolution failed: {error}"
                )
            }
        }
    }
}

impl Error for RuntimeDiagnosticError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::DiagnosticCode(error) => Some(error),
            Self::Source(error) => Some(error),
            Self::SourceMap(error) => Some(error),
            Self::MissingSource { .. } | Self::SourceTextMismatch { .. } => None,
        }
    }
}

impl From<DiagnosticCodeError> for RuntimeDiagnosticError {
    fn from(error: DiagnosticCodeError) -> Self {
        Self::DiagnosticCode(error)
    }
}

impl From<SourceError> for RuntimeDiagnosticError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<SourceMapError> for RuntimeDiagnosticError {
    fn from(error: SourceMapError) -> Self {
        Self::SourceMap(error)
    }
}

fn diagnostic(
    code: &'static str,
    severity: DiagnosticSeverity,
    message: impl Into<String>,
) -> Result<Diagnostic, RuntimeDiagnosticError> {
    Ok(Diagnostic::new(
        DiagnosticCode::new(code)?,
        severity,
        message,
    ))
}

fn frame_span(
    sources: &SourceRegistry,
    frame: &JsStackFrame,
) -> Result<SourceSpan, RuntimeDiagnosticError> {
    let source_id = sources
        .source_id_by_name(frame.source_name())
        .ok_or_else(|| RuntimeDiagnosticError::MissingSource {
            source_name: frame.source_name().to_owned(),
        })?;
    let source = sources.source(&source_id)?;
    if source.text().as_ref() != frame.source_text() {
        return Err(RuntimeDiagnosticError::SourceTextMismatch {
            source_name: frame.source_name().to_owned(),
        });
    }
    let retained = frame.source_span();
    let generated = sources.span(
        &source_id,
        retained.start() as usize,
        retained.end() as usize,
    )?;
    Ok(sources.resolve_span(&generated)?.display_span().clone())
}

fn exception_code(exception: &JsException) -> &'static str {
    match exception.kind() {
        Some(ExceptionKind::InternalError) => "fusor::runtime::exception::internal_error",
        Some(ExceptionKind::RangeError) => "fusor::runtime::exception::range_error",
        Some(ExceptionKind::ReferenceError) => "fusor::runtime::exception::reference_error",
        Some(ExceptionKind::SyntaxError) => "fusor::runtime::exception::syntax_error",
        Some(ExceptionKind::TypeError) => "fusor::runtime::exception::type_error",
        Some(ExceptionKind::UriError) => "fusor::runtime::exception::uri_error",
        None => "fusor::runtime::exception::thrown_value",
    }
}

impl JsException {
    /// Converts an escaping JavaScript exception and its verified caller stack
    /// into a source-map-resolved shared diagnostic report.
    ///
    /// Each caller is a related diagnostic so Miette can render stack frames
    /// from different source files without flattening their provenance.
    ///
    /// # Errors
    ///
    /// Returns an error if a retained frame source is absent, has been replaced
    /// by different text, contains an invalid span, or has an invalid incoming
    /// source-map chain.
    pub fn to_diagnostic_report(
        &self,
        sources: &SourceRegistry,
    ) -> Result<DiagnosticReport, RuntimeDiagnosticError> {
        let mut primary = diagnostic(
            exception_code(self),
            DiagnosticSeverity::Error,
            self.to_string(),
        )?
        .with_label(DiagnosticLabel::primary(
            frame_span(sources, self.origin_frame())?,
            Some("exception originated here".to_owned()),
        ));
        if !self.caller_frames().is_empty() {
            primary = primary.with_help("JavaScript callers are shown as related diagnostics");
        }

        let related = self
            .caller_frames()
            .iter()
            .map(|frame| {
                Ok(diagnostic(
                    "fusor::runtime::stack_frame",
                    DiagnosticSeverity::Advice,
                    format!(
                        "called from {} (function {}, bytecode PC {})",
                        frame.source_name(),
                        frame.function(),
                        frame.pc()
                    ),
                )?
                .with_label(DiagnosticLabel::primary(
                    frame_span(sources, frame)?,
                    Some("call site".to_owned()),
                )))
            })
            .collect::<Result<Vec<_>, RuntimeDiagnosticError>>()?;
        Ok(DiagnosticReport::new(primary).with_related_diagnostics(related))
    }
}

impl RuntimeError {
    /// Converts a runtime construction failure to a stable shared diagnostic.
    ///
    /// # Errors
    ///
    /// Returns an error only if an engine-owned stable code is invalid.
    pub fn to_diagnostic(&self) -> Result<Diagnostic, RuntimeDiagnosticError> {
        let code = match self {
            Self::Atom(_) => "fusor::runtime::atom",
            Self::LimitExceeded { .. } => "fusor::runtime::limit_exceeded",
            Self::AllocationFailed { .. } => "fusor::runtime::allocation_failed",
        };
        diagnostic(code, DiagnosticSeverity::Error, self.to_string())
    }
}

impl InstallError {
    /// Converts a verified-bytecode installation failure to a stable shared
    /// diagnostic.
    ///
    /// Source-carrying installation variants retain their typed fields on the
    /// error; a host facade that owns the source registry may add a label.
    ///
    /// # Errors
    ///
    /// Returns an error only if an engine-owned stable code is invalid.
    pub fn to_diagnostic(&self) -> Result<Diagnostic, RuntimeDiagnosticError> {
        let code = match self {
            Self::UnsupportedOpcode { .. } => "fusor::runtime::install::unsupported_opcode",
            Self::LimitExceeded { .. } => "fusor::runtime::install::limit_exceeded",
            Self::AllocationFailed { .. } => "fusor::runtime::install::allocation_failed",
            Self::String(_) => "fusor::runtime::install::string",
            Self::BigInt(_) => "fusor::runtime::install::bigint",
            Self::Atom(_) => "fusor::runtime::install::atom",
            Self::GlobalDeclarationRejected { .. } => {
                "fusor::runtime::install::global_declaration_rejected"
            }
            Self::AuthorityInvariant { .. } => "fusor::runtime::install::authority_invariant",
        };
        diagnostic(code, DiagnosticSeverity::Error, self.to_string())
    }
}

impl ExecutionError {
    /// Converts an execution failure to a stable shared diagnostic report.
    ///
    /// Escaping JavaScript exceptions include a source-map-resolved origin and
    /// related caller diagnostics. Host/resource/invariant failures retain a
    /// stable code and message without inventing unavailable source locations.
    ///
    /// # Errors
    ///
    /// Returns a source/provenance error while converting a JavaScript
    /// exception, or a stable-code validation error for another variant.
    pub fn to_diagnostic_report(
        &self,
        sources: &SourceRegistry,
    ) -> Result<DiagnosticReport, RuntimeDiagnosticError> {
        let code = match self {
            Self::Exception(exception) => return exception.to_diagnostic_report(sources),
            Self::Handle(_) => "fusor::runtime::execution::handle",
            Self::Atom(_) => "fusor::runtime::execution::atom",
            Self::DynamicFunctionCompilation(_) => {
                "fusor::runtime::execution::dynamic_function_compilation"
            }
            Self::DynamicFunctionInstallation(_) => {
                "fusor::runtime::execution::dynamic_function_installation"
            }
            Self::Interrupted { .. } => "fusor::runtime::execution::interrupted",
            Self::InstructionLimitExceeded { .. } => {
                "fusor::runtime::execution::instruction_limit_exceeded"
            }
            Self::LimitExceeded { .. } => "fusor::runtime::execution::limit_exceeded",
            Self::AllocationFailed { .. } => "fusor::runtime::execution::allocation_failed",
            Self::String(_) => "fusor::runtime::execution::string",
            Self::EngineFault(_) => "fusor::runtime::execution::engine_fault",
        };
        Ok(DiagnosticReport::new(diagnostic(
            code,
            DiagnosticSeverity::Error,
            self.to_string(),
        )?))
    }
}

impl GlobalScriptError {
    /// Converts Global Script installation or execution failure into a shared
    /// diagnostic report.
    ///
    /// # Errors
    ///
    /// Returns any runtime diagnostic conversion failure.
    pub fn to_diagnostic_report(
        &self,
        sources: &SourceRegistry,
    ) -> Result<DiagnosticReport, RuntimeDiagnosticError> {
        match self {
            Self::Install(error) => Ok(DiagnosticReport::new(error.to_diagnostic()?)),
            Self::Execution(error) => error.to_diagnostic_report(sources),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fusor_bytecode::{BytecodePc, FunctionTemplateId, SourceByteSpan};
    use fusor_diagnostics::{SourceMap, SourceRegistry, render_pretty_report};

    use super::RuntimeDiagnosticError;
    use crate::{ExceptionKind, JsException, JsStackFrame, JsString};

    #[test]
    fn exception_reports_resolve_origins_and_render_callers_as_related_sources() {
        let source_map = SourceMap::from_slice(
            br#"{"version":3,"sources":["original.ts"],"names":[],"mappings":"AAAA"}"#,
        )
        .expect("source map");
        let mut sources = SourceRegistry::new();
        sources
            .register_with_source_map("bundle.js", "x", Some(source_map))
            .expect("bundle");
        sources.register("original.ts", "x").expect("original");
        sources.register("caller.js", "y").expect("caller");
        let origin = JsStackFrame::new(
            FunctionTemplateId::new(0),
            BytecodePc::new(0),
            Arc::from("bundle.js"),
            Arc::from("x"),
            SourceByteSpan::new(0, 1),
        );
        let caller = JsStackFrame::new(
            FunctionTemplateId::new(1),
            BytecodePc::new(2),
            Arc::from("caller.js"),
            Arc::from("y"),
            SourceByteSpan::new(0, 1),
        );
        let exception = JsException::engine_error(
            ExceptionKind::TypeError,
            JsString::from_utf8("bad receiver").expect("message"),
            origin,
            vec![caller],
        );

        let report = exception
            .to_diagnostic_report(&sources)
            .expect("runtime report");
        assert_eq!(
            report.primary().code().as_str(),
            "fusor::runtime::exception::type_error"
        );
        assert_eq!(
            report.primary().labels()[0].span().source_id(),
            &sources
                .source_id_by_name("original.ts")
                .expect("original ID")
        );
        let rendered = render_pretty_report(&sources, &report).expect("Miette report");
        assert!(rendered.contains("TypeError: bad receiver"));
        assert!(rendered.contains("original.ts"));
        assert!(rendered.contains("called from caller.js"));
        assert!(rendered.contains("caller.js"));
    }

    #[test]
    fn frame_names_cannot_silently_select_different_registered_text() {
        let mut sources = SourceRegistry::new();
        sources.register("same.js", "different").expect("source");
        let origin = JsStackFrame::new(
            FunctionTemplateId::new(0),
            BytecodePc::new(0),
            Arc::from("same.js"),
            Arc::from("original"),
            SourceByteSpan::new(0, 1),
        );
        let exception = JsException::engine_error(
            ExceptionKind::TypeError,
            JsString::from_utf8("bad receiver").expect("message"),
            origin,
            Vec::new(),
        );

        assert_eq!(
            exception
                .to_diagnostic_report(&sources)
                .expect_err("text mismatch"),
            RuntimeDiagnosticError::SourceTextMismatch {
                source_name: "same.js".to_owned()
            }
        );
    }
}
