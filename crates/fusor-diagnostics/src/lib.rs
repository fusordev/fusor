//! Source ownership, stable diagnostics, and source-map v3 resolution.
//!
//! This crate is the shared debug-metadata boundary for the JavaScript front
//! end, compiler, bytecode verifier, runtime, and command-line tools. It does
//! not define JavaScript semantics.
//!
//! Source text is immutable and registry-owned. [`SourceId`] values retain
//! registry provenance, [`ByteSpan`] values are validated half-open UTF-8 byte
//! ranges, and source-map coordinates use their own explicitly zero-based type.

#![forbid(unsafe_code)]

mod diagnostic;
mod source;
mod source_map;

pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticCodeError, DiagnosticLabel, DiagnosticReport,
    DiagnosticSeverity, PrettyDiagnostic, PrettyDiagnosticError, PrettyDiagnosticReport,
    render_pretty, render_pretty_report,
};
pub use source::{
    ByteSpan, ColumnEncoding, LineColumn, SourceError, SourceFile, SourceId, SourceRegistry,
    SourceSnippet, SourceSpan,
};
pub use source_map::{
    OriginalLocation, ResolvedLocation, ResolvedSpan, SourceMap, SourceMapError,
    SourceMapErrorKind, SourceMapMapping, SourceMapPosition,
};
