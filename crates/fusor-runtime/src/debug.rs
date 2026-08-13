//! Runtime-neutral debugger inspection hooks.
//!
//! The runtime owns execution frames and invokes this hook only at verified
//! instruction boundaries. Protocol adapters live outside this crate so the
//! engine remains independent from transport and serialization choices.

use std::sync::Arc;

use fusor_bytecode::{BytecodePc, FunctionTemplateId, SourceByteSpan};

/// One verified source location currently executing in the VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugLocation {
    function: FunctionTemplateId,
    pc: BytecodePc,
    source_name: Arc<str>,
    source_text: Arc<str>,
    source_span: SourceByteSpan,
}

impl DebugLocation {
    #[must_use]
    pub(crate) const fn new(
        function: FunctionTemplateId,
        pc: BytecodePc,
        source_name: Arc<str>,
        source_text: Arc<str>,
        source_span: SourceByteSpan,
    ) -> Self {
        Self {
            function,
            pc,
            source_name,
            source_text,
            source_span,
        }
    }

    /// Returns the graph-local function template containing this instruction.
    #[must_use]
    pub const fn function(&self) -> FunctionTemplateId {
        self.function
    }

    /// Returns the verified bytecode program counter.
    #[must_use]
    pub const fn pc(&self) -> BytecodePc {
        self.pc
    }

    /// Returns the source display name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the complete immutable source text.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Returns the exact source span for the executing instruction.
    #[must_use]
    pub const fn source_span(&self) -> SourceByteSpan {
        self.source_span
    }
}

/// Snapshot supplied at a verified instruction boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugExecutionSnapshot {
    location: DebugLocation,
    stack: Arc<[DebugLocation]>,
    debugger_statement: bool,
}

impl DebugExecutionSnapshot {
    #[must_use]
    pub(crate) fn new(
        location: DebugLocation,
        stack: Arc<[DebugLocation]>,
        debugger_statement: bool,
    ) -> Self {
        Self {
            location,
            stack,
            debugger_statement,
        }
    }

    /// Returns the currently executing source location.
    #[must_use]
    pub const fn location(&self) -> &DebugLocation {
        &self.location
    }

    /// Returns active bytecode frames from innermost to outermost.
    #[must_use]
    pub fn stack(&self) -> &[DebugLocation] {
        &self.stack
    }

    /// Returns whether the current location is a source `debugger` statement.
    #[must_use]
    pub const fn is_debugger_statement(&self) -> bool {
        self.debugger_statement
    }
}

/// Host debugger hook invoked at verified instruction boundaries.
///
/// Implementations may block while a debugger client decides whether to resume.
/// They must not re-enter the runtime, because the active VM frames remain owned
/// by the caller for the duration of this method.
pub trait DebuggerHook: Send + Sync {
    /// Observes one instruction boundary and may synchronously pause execution.
    fn on_instruction(&self, snapshot: &DebugExecutionSnapshot);
}

impl<F> DebuggerHook for F
where
    F: Fn(&DebugExecutionSnapshot) + Send + Sync,
{
    fn on_instruction(&self, snapshot: &DebugExecutionSnapshot) {
        self(snapshot);
    }
}
