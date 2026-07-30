use std::{error::Error, fmt};

use quickjs_bytecode::{BytecodePc, FinalOpcode, FunctionTemplateId, SourceByteSpan};

use crate::{AtomError, JsString, JsStringError};

/// Public runtime handle category.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandleKind {
    /// A runtime realm.
    Realm,
    /// An arbitrary rooted JavaScript value.
    Value,
    /// A rooted bytecode function.
    Function,
}

impl fmt::Display for HandleKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Realm => "realm",
            Self::Value => "value",
            Self::Function => "function",
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
    /// Captured binding cells.
    BindingCells,
    /// Independently rooted public function values.
    PublicRoots,
    /// Values retained by one active frame.
    FrameValues,
    /// Active interpreter frames.
    Frames,
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
            Self::BindingCells => "binding cells",
            Self::PublicRoots => "public roots",
            Self::FrameValues => "active frame values",
            Self::Frames => "active frames",
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

/// JavaScript exception category currently constructible by this VM slice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExceptionKind {
    /// A lexical binding was read or written before initialization.
    ReferenceError,
}

/// One JavaScript abrupt completion with verified bytecode/source provenance.
#[derive(Clone, Debug)]
pub struct JsException {
    kind: ExceptionKind,
    message: JsString,
    function: FunctionTemplateId,
    pc: BytecodePc,
    source_name: String,
    source_span: SourceByteSpan,
}

impl JsException {
    pub(crate) fn reference_error(
        message: JsString,
        function: FunctionTemplateId,
        pc: BytecodePc,
        source_name: String,
        source_span: SourceByteSpan,
    ) -> Self {
        Self {
            kind: ExceptionKind::ReferenceError,
            message,
            function,
            pc,
            source_name,
            source_span,
        }
    }

    /// Returns the exception category.
    #[must_use]
    pub const fn kind(&self) -> ExceptionKind {
        self.kind
    }

    /// Returns the exact JavaScript message without its error-name prefix.
    #[must_use]
    pub const fn message(&self) -> &JsString {
        &self.message
    }

    /// Returns the graph-local function template.
    #[must_use]
    pub const fn function(&self) -> FunctionTemplateId {
        self.function
    }

    /// Returns the verified bytecode position.
    #[must_use]
    pub const fn pc(&self) -> BytecodePc {
        self.pc
    }

    /// Returns the retained source display name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the exact retained source span.
    #[must_use]
    pub const fn source_span(&self) -> SourceByteSpan {
        self.source_span
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
            Self::Exception(exception) => {
                let name = match exception.kind() {
                    ExceptionKind::ReferenceError => "ReferenceError",
                };
                write!(
                    formatter,
                    "{name}: {}",
                    exception
                        .message()
                        .to_utf8_lossy()
                        .unwrap_or_else(|_| "<message allocation failed>".to_owned())
                )
            }
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
