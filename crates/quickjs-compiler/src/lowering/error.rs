use std::{error::Error, fmt};

use quickjs_bytecode::{
    AssemblerError, BytecodePc, BytecodeVerificationError, CompilerBigIntError,
    CompilerStringError, CompilerTemplateObjectError, EncodeError, FunctionGraphVerificationError,
    VerificationError,
};
use quickjs_frontend::{OxcStringDecodeError, Span};

use crate::storage::ExecutableId;

pub(in crate::lowering) fn unsupported<T>(
    feature: UnsupportedLeafFeature,
    span: Span,
) -> Result<T, LeafCompilationError> {
    Err(LeafCompilationError::Unsupported { feature, span })
}

/// Syntax or storage behavior outside the currently executable compiler slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLeafFeature {
    /// The selected executable is not a synchronous ordinary function.
    NonOrdinaryFunction,
    /// A dynamic Function unit must compile only through its complete Script root.
    DynamicFunctionRequiresScriptRoot,
    /// An anonymous function needs exact inferred-name initialization.
    InferredFunctionName,
    /// The Oxc function form is neither a declaration nor function expression.
    UnsupportedFunctionForm,
    /// An object method or accessor is outside the admitted static,
    /// synchronous, identifier-or-literal-named profile.
    ObjectMethodOrAccessor,
    /// The selected function contains another executable body.
    NestedExecutable,
    /// Module-owned storage is outside this Script-only lowering slice.
    UnsupportedCompilationUnit,
    /// A statement requires unsupported control flow or scope entry behavior.
    UnsupportedBody,
    /// A declaration is not a simple `var`, `let`, or `const` binding.
    UnsupportedDeclaration,
    /// An expression requires method, optional, spread, or constructor calls,
    /// properties, non-identifier mutation, or another unsupported family.
    UnsupportedExpression,
    /// A literal requires a constant, atom, `BigInt`, or `RegExp` pool entry.
    UnsupportedLiteral,
    /// A binding cannot be represented by this frame layout.
    UnsupportedBinding,
    /// A destructuring pattern is outside the admitted array-pattern slice.
    UnsupportedPattern,
    /// Program-level bindings require the constructor realm's global environment.
    GlobalEnvironment,
    /// Sloppy direct eval requires its caller's variable environment.
    DirectEvalVariableEnvironment,
    /// A reference access or binding write policy is not supported.
    UnsupportedReference,
    /// An identifier remained unresolved after Oxc semantics.
    UnresolvedReference,
}

/// Failure to lower or verify one executable body or complete subtree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeafCompilationError {
    /// The executable selection was issued by another compilation context.
    ForeignExecutable {
        /// The foreign selection's plan-local identity.
        executable: ExecutableId,
    },
    /// A context-issued executable no longer resolves in its immutable plan.
    InvalidExecutable {
        /// The rejected plan-local identity.
        executable: ExecutableId,
    },
    /// The selected source requires behavior outside this lowering slice.
    Unsupported {
        /// The unsupported behavior.
        feature: UnsupportedLeafFeature,
        /// Exact source span requiring it.
        span: Span,
    },
    /// Retained Oxc semantics or compiler identities violated an invariant.
    SemanticInvariant {
        /// Stable invariant label.
        invariant: &'static str,
        /// Related source span, when available.
        span: Option<Span>,
    },
    /// A dense bytecode domain exceeded its encoded width.
    CapacityExceeded {
        /// Stable capacity-domain label.
        domain: &'static str,
    },
    /// Oxc's retained cooked-string transport encoding was malformed.
    CookedStringDecoding {
        /// Exact source span of the affected literal.
        span: Span,
        /// Exact decoder failure.
        source: OxcStringDecodeError,
    },
    /// A cooked string could not be frozen as an exact compiler value.
    CompilerString {
        /// Exact source span of the affected literal.
        span: Span,
        /// Exact string-construction failure.
        source: CompilerStringError,
    },
    /// A parsed `BigInt` literal did not produce a canonical decimal payload.
    CompilerBigInt {
        /// Exact literal span.
        span: Span,
        /// Exact decimal-payload validation failure.
        source: CompilerBigIntError,
    },
    /// A tagged template did not produce a valid site-object payload.
    CompilerTemplateObject {
        /// Exact tagged-template span.
        span: Span,
        /// Exact site-object validation failure.
        source: CompilerTemplateObjectError,
    },
    /// `RegExp` literal grammar or executable lowering failed.
    RegExp {
        /// Exact literal span.
        span: Span,
        /// Exact project-owned `RegExp` compiler failure.
        source: quickjs_regexp::CompileError,
    },
    /// A typed final instruction could not be encoded.
    BytecodeEncoding {
        /// Source span responsible for the instruction.
        span: Span,
        /// Exact encoder failure.
        source: EncodeError,
    },
    /// Symbolic labels or branch relaxation could not produce final bytecode.
    BytecodeAssembly {
        /// Related instruction or compiler-owned label span, when available.
        span: Option<Span>,
        /// Exact assembler failure.
        source: AssemblerError,
    },
    /// A reachable compiler-owned statement anchor had the wrong entry stack.
    BytecodeStackInvariant {
        /// Source span that owns the statement anchor.
        span: Span,
        /// Final relocated bytecode position of the anchor.
        pc: BytecodePc,
        /// Compiler-required operand-stack depth.
        expected: u32,
        /// Verified reachable operand-stack depth.
        actual: u32,
    },
    /// The emitted body failed staged control-flow verification.
    BytecodeVerification {
        /// Exact instruction span for the verifier PC, when the error has one.
        span: Option<Span>,
        /// Exact join-target span for a two-position verifier failure.
        related_span: Option<Span>,
        /// Exact verifier failure.
        source: VerificationError,
    },
    /// The complete compiler function graph failed cross-function checks.
    FunctionGraphVerification {
        /// Source function span for a graph-local failure, when available.
        span: Option<Span>,
        /// Exact aggregate or cross-function verifier failure.
        source: FunctionGraphVerificationError,
    },
    /// Complete runtime metadata failed final bytecode verification.
    BytecodeGraphVerification {
        /// Source function span for a graph-local failure, when available.
        span: Option<Span>,
        /// Exact final-verifier failure.
        source: BytecodeVerificationError,
    },
}

impl fmt::Display for LeafCompilationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignExecutable { executable } => {
                write!(formatter, "foreign executable {}", executable.index())
            }
            Self::InvalidExecutable { executable } => {
                write!(formatter, "invalid executable {}", executable.index())
            }
            Self::Unsupported { feature, span } => {
                write!(
                    formatter,
                    "unsupported leaf feature {feature:?} at {span:?}"
                )
            }
            Self::SemanticInvariant { invariant, span } => {
                write!(formatter, "compiler invariant `{invariant}` failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                Ok(())
            }
            Self::CapacityExceeded { domain } => {
                write!(formatter, "compiler capacity exceeded for {domain}")
            }
            Self::CookedStringDecoding { span, source } => {
                write!(
                    formatter,
                    "cooked string decoding failed at {span:?}: {source}"
                )
            }
            Self::CompilerString { span, source } => {
                write!(formatter, "compiler string failed at {span:?}: {source}")
            }
            Self::CompilerBigInt { span, source } => {
                write!(formatter, "compiler BigInt failed at {span:?}: {source}")
            }
            Self::CompilerTemplateObject { span, source } => {
                write!(
                    formatter,
                    "compiler template object failed at {span:?}: {source}"
                )
            }
            Self::RegExp { span, source } => {
                write!(formatter, "regular expression failed at {span:?}: {source}")
            }
            Self::BytecodeEncoding { span, source } => {
                write!(formatter, "bytecode encoding failed at {span:?}: {source}")
            }
            Self::BytecodeAssembly { span, source } => {
                write!(formatter, "bytecode assembly failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                write!(formatter, ": {source}")
            }
            Self::BytecodeStackInvariant {
                span,
                pc,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "compiler stack invariant failed at {span:?} (PC {pc}): \
                     expected depth {expected}, got {actual}"
                )
            }
            Self::BytecodeVerification {
                span,
                related_span,
                source,
            } => {
                write!(formatter, "{source}")?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                if let Some(related_span) = related_span {
                    write!(formatter, " (related source {related_span:?})")?;
                }
                Ok(())
            }
            Self::FunctionGraphVerification { span, source } => {
                source.fmt(formatter)?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                Ok(())
            }
            Self::BytecodeGraphVerification { span, source } => {
                source.fmt(formatter)?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for LeafCompilationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BytecodeEncoding { source, .. } => Some(source),
            Self::BytecodeAssembly { source, .. } => Some(source),
            Self::BytecodeVerification { source, .. } => Some(source),
            Self::FunctionGraphVerification { source, .. } => Some(source),
            Self::BytecodeGraphVerification { source, .. } => Some(source),
            Self::CookedStringDecoding { source, .. } => Some(source),
            Self::CompilerString { source, .. } => Some(source),
            Self::CompilerBigInt { source, .. } => Some(source),
            Self::CompilerTemplateObject { source, .. } => Some(source),
            Self::RegExp { source, .. } => Some(source),
            Self::ForeignExecutable { .. }
            | Self::InvalidExecutable { .. }
            | Self::Unsupported { .. }
            | Self::SemanticInvariant { .. }
            | Self::CapacityExceeded { .. }
            | Self::BytecodeStackInvariant { .. } => None,
        }
    }
}
