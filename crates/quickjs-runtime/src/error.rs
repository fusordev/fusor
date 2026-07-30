use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{BytecodePc, FinalOpcode, FunctionTemplateId, SourceByteSpan};

use crate::{AtomError, JsString, JsStringError, JsValue};

/// Public runtime handle category.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandleKind {
    /// A runtime realm.
    Realm,
    /// An arbitrary rooted JavaScript value.
    Value,
    /// A rooted bytecode function.
    Function,
    /// A rooted ordinary object.
    Object,
}

impl fmt::Display for HandleKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Realm => "realm",
            Self::Value => "value",
            Self::Function => "function",
            Self::Object => "object",
        })
    }
}

/// Observable JavaScript value family currently admitted by the runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ValueKind {
    /// The `undefined` primitive.
    Undefined,
    /// The `null` primitive.
    Null,
    /// An ECMAScript Boolean.
    Boolean,
    /// An ECMAScript Number.
    Number,
    /// An ECMAScript String.
    String,
    /// An ordinary bytecode function object.
    Function,
    /// An ordinary JavaScript object.
    Object,
}

impl fmt::Display for ValueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Undefined => "undefined",
            Self::Null => "null",
            Self::Boolean => "Boolean",
            Self::Number => "Number",
            Self::String => "String",
            Self::Function => "function",
            Self::Object => "object",
        })
    }
}

/// Failure to use a runtime-local public handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandleError {
    /// The owning runtime has already been dropped.
    Orphaned {
        /// Orphaned handle category.
        kind: HandleKind,
    },
    /// The handle belongs to another live runtime.
    ForeignRuntime {
        /// Foreign handle category.
        kind: HandleKind,
    },
    /// The generational runtime slot no longer exists.
    Stale {
        /// Stale handle category.
        kind: HandleKind,
        /// Arena slot index.
        index: usize,
        /// Rejected slot generation.
        generation: u32,
    },
    /// A value did not have the required observable kind.
    WrongValueKind {
        /// Required kind.
        expected: ValueKind,
        /// Actual kind.
        actual: ValueKind,
    },
}

impl fmt::Display for HandleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Orphaned { kind } => write!(formatter, "{kind} handle is orphaned"),
            Self::ForeignRuntime { kind } => {
                write!(formatter, "{kind} handle belongs to another runtime")
            }
            Self::Stale {
                kind,
                index,
                generation,
            } => write!(
                formatter,
                "{kind} handle names stale slot {index} generation {generation}"
            ),
            Self::WrongValueKind { expected, actual } => {
                write!(formatter, "expected {expected}, found {actual}")
            }
        }
    }
}

impl Error for HandleError {}

/// Runtime-owned resource governed by an inclusive limit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeResource {
    /// Runtime realms.
    Realms,
    /// Installed verified bytecode instances.
    InstalledCode,
    /// Installed function templates.
    InstalledTemplates,
    /// Installed function-local atoms.
    InstalledAtoms,
    /// Installed constants.
    InstalledConstants,
    /// Runtime function objects.
    HeapFunctions,
    /// Runtime ordinary objects.
    HeapObjects,
    /// Own property slots across ordinary objects and functions.
    ObjectProperties,
    /// Captured binding cells.
    BindingCells,
    /// Constructor-realm global binding records.
    RealmGlobalBindings,
    /// Independently rooted public function and ordinary-object values.
    PublicRoots,
    /// Values retained by one active frame.
    FrameValues,
    /// Active interpreter frames.
    Frames,
    /// Caller locations retained for one escaping JavaScript exception.
    ExceptionFrames,
    /// A runtime-owned collection allocation.
    Collection,
    /// The deferred public-root release mailbox.
    ReleaseMailbox,
}

impl fmt::Display for RuntimeResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Realms => "realms",
            Self::InstalledCode => "installed bytecode instances",
            Self::InstalledTemplates => "installed function templates",
            Self::InstalledAtoms => "installed atoms",
            Self::InstalledConstants => "installed constants",
            Self::HeapFunctions => "heap functions",
            Self::HeapObjects => "heap objects",
            Self::ObjectProperties => "object properties",
            Self::BindingCells => "binding cells",
            Self::RealmGlobalBindings => "realm global bindings",
            Self::PublicRoots => "public roots",
            Self::FrameValues => "active frame values",
            Self::Frames => "active frames",
            Self::ExceptionFrames => "exception stack frames",
            Self::Collection => "runtime collection",
            Self::ReleaseMailbox => "release mailbox",
        })
    }
}

/// Failure to create or extend a runtime outside bytecode installation.
#[derive(Debug)]
pub enum RuntimeError {
    /// Atom-table construction failed.
    Atom(AtomError),
    /// An inclusive resource ceiling was exceeded.
    LimitExceeded {
        /// Limited resource.
        resource: RuntimeResource,
        /// Inclusive configured limit.
        limit: u64,
        /// Observed usage.
        observed: u64,
    },
    /// A recoverable collection allocation failed.
    AllocationFailed {
        /// Collection being grown.
        resource: RuntimeResource,
        /// Additional elements requested.
        additional: usize,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Atom(source) => source.fmt(formatter),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} exceeded by observed usage {observed}"
            ),
            Self::AllocationFailed {
                resource,
                additional,
            } => write!(
                formatter,
                "failed to reserve {additional} additional entries for {resource}"
            ),
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Atom(source) => Some(source),
            Self::LimitExceeded { .. } | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<AtomError> for RuntimeError {
    fn from(source: AtomError) -> Self {
        Self::Atom(source)
    }
}

/// Failure to turn verified bytecode into one runtime-local function.
#[derive(Debug)]
pub enum InstallError {
    /// An exact opcode is outside this interpreter profile.
    UnsupportedOpcode {
        /// Function template containing the opcode.
        function: FunctionTemplateId,
        /// Verified bytecode position.
        pc: BytecodePc,
        /// Exact retained source span.
        source_span: SourceByteSpan,
        /// Rejected final opcode.
        opcode: FinalOpcode,
    },
    /// An inclusive resource ceiling was exceeded before commit.
    LimitExceeded {
        /// Limited resource.
        resource: RuntimeResource,
        /// Inclusive configured limit.
        limit: u64,
        /// Observed usage.
        observed: u64,
    },
    /// A recoverable collection allocation failed.
    AllocationFailed {
        /// Collection being grown.
        resource: RuntimeResource,
        /// Additional elements requested.
        additional: usize,
    },
    /// A compiler string could not become a runtime string.
    String(JsStringError),
    /// Runtime-local atom interning failed.
    Atom(AtomError),
    /// Verified authority contradicted an installation invariant.
    AuthorityInvariant {
        /// Concise invariant description.
        message: &'static str,
    },
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOpcode {
                function,
                pc,
                opcode,
                ..
            } => write!(
                formatter,
                "function {function} bytecode PC {pc}: opcode {} is not executable yet",
                opcode.mnemonic()
            ),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} exceeded by observed usage {observed}"
            ),
            Self::AllocationFailed {
                resource,
                additional,
            } => write!(
                formatter,
                "failed to reserve {additional} additional entries for {resource}"
            ),
            Self::String(source) => source.fmt(formatter),
            Self::Atom(source) => source.fmt(formatter),
            Self::AuthorityInvariant { message } => {
                write!(formatter, "verified bytecode invariant failed: {message}")
            }
        }
    }
}

impl Error for InstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::String(source) => Some(source),
            Self::Atom(source) => Some(source),
            Self::UnsupportedOpcode { .. }
            | Self::LimitExceeded { .. }
            | Self::AllocationFailed { .. }
            | Self::AuthorityInvariant { .. } => None,
        }
    }
}

impl From<JsStringError> for InstallError {
    fn from(source: JsStringError) -> Self {
        Self::String(source)
    }
}

impl From<AtomError> for InstallError {
    fn from(source: AtomError) -> Self {
        Self::Atom(source)
    }
}

/// Category of an engine-created JavaScript error.
///
/// An explicitly thrown JavaScript value has no category unless that value is
/// itself later modeled as an Error object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExceptionKind {
    /// A lexical binding was read or written before initialization.
    ReferenceError,
    /// A value was used in an operation requiring another JavaScript type.
    TypeError,
}

/// One verified caller location retained on an escaping JavaScript exception.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsStackFrame {
    function: FunctionTemplateId,
    pc: BytecodePc,
    source_name: Arc<str>,
    source_text: Arc<str>,
    source_span: SourceByteSpan,
}

impl JsStackFrame {
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

    /// Returns the graph-local function template containing this call.
    #[must_use]
    pub const fn function(&self) -> FunctionTemplateId {
        self.function
    }

    /// Returns the verified bytecode position of this call.
    #[must_use]
    pub const fn pc(&self) -> BytecodePc {
        self.pc
    }

    /// Returns the retained source display name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the immutable retained source artifact containing this call.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Returns the exact retained call-expression span.
    #[must_use]
    pub const fn source_span(&self) -> SourceByteSpan {
        self.source_span
    }
}

#[derive(Clone, Debug)]
enum ExceptionPayload {
    EngineError {
        kind: ExceptionKind,
        message: JsString,
    },
    ThrownValue(JsValue),
}

/// One escaping JavaScript abrupt completion with verified source provenance.
#[derive(Clone, Debug)]
pub struct JsException {
    payload: ExceptionPayload,
    origin: JsStackFrame,
    caller_frames: Vec<JsStackFrame>,
}

impl JsException {
    /// Constructs an engine-created error from already-owned parts.
    ///
    /// This constructor performs no allocation. The caller must reserve and
    /// populate `caller_frames` before transferring it here.
    pub(crate) fn engine_error(
        kind: ExceptionKind,
        message: JsString,
        origin: JsStackFrame,
        caller_frames: Vec<JsStackFrame>,
    ) -> Self {
        Self {
            payload: ExceptionPayload::EngineError { kind, message },
            origin,
            caller_frames,
        }
    }

    /// Constructs an explicit `throw` completion from already-owned parts.
    ///
    /// This constructor performs no allocation. `value` is already a
    /// runtime-local public root when it names a heap value, and the caller
    /// must reserve and populate `caller_frames` before transferring it here.
    pub(crate) fn explicit_throw(
        value: JsValue,
        origin: JsStackFrame,
        caller_frames: Vec<JsStackFrame>,
    ) -> Self {
        Self {
            payload: ExceptionPayload::ThrownValue(value),
            origin,
            caller_frames,
        }
    }

    /// Returns the engine-created error category.
    ///
    /// An arbitrary value escaping from a JavaScript `throw` returns `None`.
    #[must_use]
    pub const fn kind(&self) -> Option<ExceptionKind> {
        match &self.payload {
            ExceptionPayload::EngineError { kind, .. } => Some(*kind),
            ExceptionPayload::ThrownValue(_) => None,
        }
    }

    /// Returns the exact engine-created error message without its name prefix.
    ///
    /// An arbitrary value escaping from a JavaScript `throw` returns `None`.
    #[must_use]
    pub const fn message(&self) -> Option<&JsString> {
        match &self.payload {
            ExceptionPayload::EngineError { message, .. } => Some(message),
            ExceptionPayload::ThrownValue(_) => None,
        }
    }

    /// Returns the exact value supplied to a JavaScript `throw`.
    ///
    /// Engine-created errors return `None`. A returned heap value remains
    /// rooted by this exception until its last shared value root is dropped.
    #[must_use]
    pub const fn thrown_value(&self) -> Option<&JsValue> {
        match &self.payload {
            ExceptionPayload::EngineError { .. } => None,
            ExceptionPayload::ThrownValue(value) => Some(value),
        }
    }

    /// Returns the graph-local function template.
    #[must_use]
    pub const fn function(&self) -> FunctionTemplateId {
        self.origin.function
    }

    /// Returns the verified bytecode position.
    #[must_use]
    pub const fn pc(&self) -> BytecodePc {
        self.origin.pc
    }

    /// Returns the retained source display name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.origin.source_name
    }

    /// Returns the immutable retained source artifact containing the origin.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.origin.source_text
    }

    /// Returns the exact retained source span.
    #[must_use]
    pub const fn source_span(&self) -> SourceByteSpan {
        self.origin.source_span
    }

    /// Returns caller call sites from the immediate caller outward.
    ///
    /// The exception's own location remains available through
    /// [`Self::function`], [`Self::pc`], [`Self::source_name`],
    /// [`Self::source_text`], and [`Self::source_span`].
    #[must_use]
    pub fn caller_frames(&self) -> &[JsStackFrame] {
        &self.caller_frames
    }
}

impl fmt::Display for JsException {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.payload {
            ExceptionPayload::EngineError { kind, message } => {
                let name = match kind {
                    ExceptionKind::ReferenceError => "ReferenceError",
                    ExceptionKind::TypeError => "TypeError",
                };
                write!(
                    formatter,
                    "{name}: {}",
                    message
                        .to_utf8_lossy()
                        .unwrap_or_else(|_| "<message allocation failed>".to_owned())
                )
            }
            ExceptionPayload::ThrownValue(_) => formatter.write_str("uncaught JavaScript value"),
        }
    }
}

/// Internal contradiction encountered after capability admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineFault {
    /// A generational runtime edge is missing.
    StaleHeapEdge {
        /// Edge category.
        edge: &'static str,
        /// Slot index.
        index: usize,
        /// Slot generation.
        generation: u32,
    },
    /// The verified instruction index is absent.
    MissingInstruction {
        /// Function template.
        function: FunctionTemplateId,
        /// Instruction index.
        instruction: u32,
    },
    /// Runtime and verified operand depths disagree.
    StackDepthMismatch {
        /// Function template.
        function: FunctionTemplateId,
        /// Bytecode position.
        pc: BytecodePc,
        /// Verified entry depth.
        expected: u32,
        /// Actual runtime entry depth.
        actual: usize,
    },
    /// A supposedly reachable instruction is marked unreachable.
    UnreachableInstruction {
        /// Function template.
        function: FunctionTemplateId,
        /// Bytecode position.
        pc: BytecodePc,
    },
    /// A verified unchecked access observed the private TDZ sentinel.
    UnexpectedUninitialized {
        /// Function template.
        function: FunctionTemplateId,
    },
    /// A verified pool index failed to resolve.
    MissingPoolEntry {
        /// Pool category.
        pool: &'static str,
        /// Rejected index.
        index: u32,
    },
    /// A verified successor has an impossible shape.
    InvalidSuccessor {
        /// Function template.
        function: FunctionTemplateId,
        /// Bytecode position.
        pc: BytecodePc,
    },
    /// A closure environment contradicts verified metadata.
    InvalidClosureEnvironment {
        /// Function template.
        function: FunctionTemplateId,
    },
    /// An admitted dispatch unexpectedly encountered an unsupported opcode.
    UnsupportedDispatch {
        /// Rejected opcode.
        opcode: FinalOpcode,
    },
    /// A runtime-only operation produced an impossible error family.
    RuntimeInvariant {
        /// Concise invariant description.
        message: &'static str,
    },
}

impl fmt::Display for EngineFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleHeapEdge {
                edge,
                index,
                generation,
            } => write!(
                formatter,
                "stale {edge} edge at slot {index} generation {generation}"
            ),
            Self::MissingInstruction {
                function,
                instruction,
            } => write!(
                formatter,
                "function {function} has no verified instruction {instruction}"
            ),
            Self::StackDepthMismatch {
                function,
                pc,
                expected,
                actual,
            } => write!(
                formatter,
                "function {function} bytecode PC {pc}: verified stack depth {expected}, runtime depth {actual}"
            ),
            Self::UnreachableInstruction { function, pc } => write!(
                formatter,
                "function {function} bytecode PC {pc} is not a reachable execution entry"
            ),
            Self::UnexpectedUninitialized { function } => write!(
                formatter,
                "function {function} performed an unchecked access to an uninitialized binding"
            ),
            Self::MissingPoolEntry { pool, index } => {
                write!(formatter, "verified {pool} index {index} is missing")
            }
            Self::InvalidSuccessor { function, pc } => write!(
                formatter,
                "function {function} bytecode PC {pc} has an invalid verified successor"
            ),
            Self::InvalidClosureEnvironment { function } => {
                write!(
                    formatter,
                    "function {function} has an invalid closure environment"
                )
            }
            Self::UnsupportedDispatch { opcode } => write!(
                formatter,
                "capability admission leaked unsupported opcode {} into dispatch",
                opcode.mnemonic()
            ),
            Self::RuntimeInvariant { message } => {
                write!(formatter, "runtime invariant failed: {message}")
            }
        }
    }
}

impl Error for EngineFault {}

/// Failure while invoking one runtime function.
#[derive(Debug)]
pub enum ExecutionError {
    /// A public handle was orphaned, foreign, stale, or had the wrong kind.
    Handle(HandleError),
    /// A JavaScript exception escaped the current function.
    Exception(JsException),
    /// Per-call instruction fuel was exhausted.
    InstructionLimitExceeded {
        /// Inclusive configured fuel.
        limit: u64,
        /// Instructions completed before interruption.
        executed: u64,
    },
    /// A runtime execution resource limit was exceeded.
    LimitExceeded {
        /// Limited resource.
        resource: RuntimeResource,
        /// Inclusive configured limit.
        limit: u64,
        /// Observed usage.
        observed: u64,
    },
    /// A recoverable interpreter allocation failed.
    AllocationFailed {
        /// Collection being grown.
        resource: RuntimeResource,
        /// Additional elements requested.
        additional: usize,
    },
    /// Runtime string construction failed.
    String(JsStringError),
    /// Verified authority and runtime state contradicted each other.
    EngineFault(EngineFault),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handle(source) => source.fmt(formatter),
            Self::Exception(exception) => exception.fmt(formatter),
            Self::InstructionLimitExceeded { limit, executed } => write!(
                formatter,
                "instruction limit {limit} exhausted after {executed} instructions"
            ),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} exceeded by observed usage {observed}"
            ),
            Self::AllocationFailed {
                resource,
                additional,
            } => write!(
                formatter,
                "failed to reserve {additional} additional entries for {resource}"
            ),
            Self::String(source) => source.fmt(formatter),
            Self::EngineFault(source) => source.fmt(formatter),
        }
    }
}

impl Error for ExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Handle(source) => Some(source),
            Self::String(source) => Some(source),
            Self::EngineFault(source) => Some(source),
            Self::Exception(_)
            | Self::InstructionLimitExceeded { .. }
            | Self::LimitExceeded { .. }
            | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<HandleError> for ExecutionError {
    fn from(source: HandleError) -> Self {
        Self::Handle(source)
    }
}

impl From<JsStringError> for ExecutionError {
    fn from(source: JsStringError) -> Self {
        Self::String(source)
    }
}

impl From<EngineFault> for ExecutionError {
    fn from(source: EngineFault) -> Self {
        Self::EngineFault(source)
    }
}

/// Failure while installing or executing one verified dynamic-Function Script.
#[derive(Debug)]
pub enum DynamicFunctionScriptError {
    /// Complete authority installation failed before Script execution began.
    Install(InstallError),
    /// The installed Script failed during execution or completion publication.
    Execution(ExecutionError),
}

impl fmt::Display for DynamicFunctionScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install(source) => source.fmt(formatter),
            Self::Execution(source) => source.fmt(formatter),
        }
    }
}

impl Error for DynamicFunctionScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Install(source) => Some(source),
            Self::Execution(source) => Some(source),
        }
    }
}

impl From<InstallError> for DynamicFunctionScriptError {
    fn from(source: InstallError) -> Self {
        Self::Install(source)
    }
}

impl From<ExecutionError> for DynamicFunctionScriptError {
    fn from(source: ExecutionError) -> Self {
        Self::Execution(source)
    }
}
