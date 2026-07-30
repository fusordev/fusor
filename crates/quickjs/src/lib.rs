//! Host-facing composition for the safe, pure-Rust `QuickJS` port.
//!
//! The lower-level runtime consumes only immutable verified bytecode. This
//! facade owns pipelines that must cross the isolated Oxc frontend, compiler,
//! final verifier, and runtime installation boundaries in that order.

#![forbid(unsafe_code)]

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{
    BytecodeGraphVerificationLimits, FunctionGraphVerificationLimits, VerificationLimits,
};
use quickjs_compiler::{CompilationContext, CompilerError, LeafCompilationError};
use quickjs_frontend::{
    DynamicFunctionError, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits,
    PreparedDynamicFunctionSource, with_dynamic_function_source_and_prepared,
};
use quickjs_runtime::{Context, DynamicFunctionScriptError, ExecutionLimits, JsValue};

/// Resource limits applied across every ordinary dynamic-Function stage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicFunctionLimits {
    frontend: FrontendLimits,
    bytecode: VerificationLimits,
    function_graph: FunctionGraphVerificationLimits,
    final_graph: BytecodeGraphVerificationLimits,
    execution: ExecutionLimits,
}

impl DynamicFunctionLimits {
    /// Replaces isolated parser, semantic, and wrapper-preparation limits.
    #[must_use]
    pub const fn with_frontend(mut self, limits: FrontendLimits) -> Self {
        self.frontend = limits;
        self
    }

    /// Replaces per-template bytecode verification limits.
    #[must_use]
    pub const fn with_bytecode(mut self, limits: VerificationLimits) -> Self {
        self.bytecode = limits;
        self
    }

    /// Replaces aggregate staged function-graph limits.
    #[must_use]
    pub const fn with_function_graph(mut self, limits: FunctionGraphVerificationLimits) -> Self {
        self.function_graph = limits;
        self
    }

    /// Replaces complete metadata and final bytecode-graph limits.
    #[must_use]
    pub const fn with_final_graph(mut self, limits: BytecodeGraphVerificationLimits) -> Self {
        self.final_graph = limits;
        self
    }

    /// Replaces runtime instruction-fuel limits.
    #[must_use]
    pub const fn with_execution(mut self, limits: ExecutionLimits) -> Self {
        self.execution = limits;
        self
    }

    /// Returns isolated parser, semantic, and wrapper-preparation limits.
    #[must_use]
    pub const fn frontend(self) -> FrontendLimits {
        self.frontend
    }

    /// Returns per-template bytecode verification limits.
    #[must_use]
    pub const fn bytecode(self) -> VerificationLimits {
        self.bytecode
    }

    /// Returns aggregate staged function-graph limits.
    #[must_use]
    pub const fn function_graph(self) -> FunctionGraphVerificationLimits {
        self.function_graph
    }

    /// Returns complete metadata and final bytecode-graph limits.
    #[must_use]
    pub const fn final_graph(self) -> BytecodeGraphVerificationLimits {
        self.final_graph
    }

    /// Returns runtime instruction-fuel limits.
    #[must_use]
    pub const fn execution(self) -> ExecutionLimits {
        self.execution
    }
}

/// Compiler stage that rejected an already parsed dynamic-Function Script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicFunctionCompilerError {
    /// Storage planning or semantic preflight failed.
    Planning(CompilerError),
    /// Lowering or whole-graph verification failed.
    Lowering(LeafCompilationError),
}

impl fmt::Display for DynamicFunctionCompilerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planning(source) => source.fmt(formatter),
            Self::Lowering(source) => source.fmt(formatter),
        }
    }
}

impl Error for DynamicFunctionCompilerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Planning(source) => Some(source),
            Self::Lowering(source) => Some(source),
        }
    }
}

/// Failure of the ordinary dynamic-Function host pipeline.
#[derive(Debug)]
pub enum DynamicFunctionConstructionError {
    /// A generator or async constructor family remains deliberately disabled.
    UnsupportedKind {
        /// Rejected dynamic-function family.
        kind: DynamicFunctionKind,
    },
    /// Exact-wrapper preparation, parsing, or Oxc semantics failed.
    Frontend(DynamicFunctionError),
    /// The parsed Script could not become complete verified bytecode.
    Compiler {
        /// Exact compiler-stage failure.
        source: DynamicFunctionCompilerError,
        /// Exact wrapper and caller-fragment map.
        prepared: PreparedDynamicFunctionSource,
    },
    /// Realm installation or verified Script execution failed.
    Runtime {
        /// Exact installation or execution failure.
        source: DynamicFunctionScriptError,
        /// Exact wrapper and caller-fragment map.
        prepared: PreparedDynamicFunctionSource,
    },
}

impl DynamicFunctionConstructionError {
    /// Returns the prepared wrapper whenever construction reached that stage.
    #[must_use]
    pub const fn prepared_source(&self) -> Option<&PreparedDynamicFunctionSource> {
        match self {
            Self::UnsupportedKind { .. } => None,
            Self::Frontend(source) => source.prepared_source(),
            Self::Compiler { prepared, .. } | Self::Runtime { prepared, .. } => Some(prepared),
        }
    }
}

impl fmt::Display for DynamicFunctionConstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedKind { kind } => {
                write!(formatter, "dynamic {kind} construction is not implemented")
            }
            Self::Frontend(source) => source.fmt(formatter),
            Self::Compiler { source, .. } => source.fmt(formatter),
            Self::Runtime { source, .. } => source.fmt(formatter),
        }
    }
}

impl Error for DynamicFunctionConstructionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnsupportedKind { .. } => None,
            Self::Frontend(source) => Some(source),
            Self::Compiler { source, .. } => Some(source),
            Self::Runtime { source, .. } => Some(source),
        }
    }
}

/// Exact Script completion returned by the dynamic-Function host pipeline.
///
/// QuickJS-compatible wrapper escape means the value is not necessarily a
/// function. The prepared source remains attached for diagnostics and future
/// function-source reflection.
#[derive(Debug)]
pub struct DynamicFunctionCompletion {
    value: JsValue,
    prepared: PreparedDynamicFunctionSource,
}

impl DynamicFunctionCompletion {
    /// Returns the exact generated-Script completion value.
    #[must_use]
    pub const fn value(&self) -> &JsValue {
        &self.value
    }

    /// Returns the exact generated wrapper and caller-fragment map.
    #[must_use]
    pub const fn prepared_source(&self) -> &PreparedDynamicFunctionSource {
        &self.prepared
    }

    /// Consumes the result and returns its Script completion.
    #[must_use]
    pub fn into_value(self) -> JsValue {
        self.value
    }

    /// Consumes the result without discarding either owned artifact.
    #[must_use]
    pub fn into_parts(self) -> (JsValue, PreparedDynamicFunctionSource) {
        (self.value, self.prepared)
    }
}

/// Constructs and executes one ordinary dynamic `Function` wrapper.
///
/// Inputs are already coerced UTF-8 source fragments. The complete exact
/// wrapper is parsed in an isolated Oxc arena, lowered as a Script root,
/// final-verified as one whole [`quickjs_bytecode::VerifiedBytecode`] graph,
/// and installed in `context`'s realm. It never receives or captures a caller
/// lexical frame and never uses eval bytecode. Generator and async families
/// remain fail closed.
///
/// The return type is the Script completion rather than `Function`: compatible
/// source can escape the synthetic wrapper and produce another object.
///
/// # Errors
///
/// Returns the exact failing stage. Every error after successful wrapper
/// preparation retains the generated source and fragment map.
#[allow(
    clippy::result_large_err,
    reason = "post-preparation failures intentionally retain the exact generated wrapper and map"
)]
pub fn construct_dynamic_function(
    context: &mut Context<'_>,
    source: DynamicFunctionSource<'_>,
    limits: DynamicFunctionLimits,
) -> Result<DynamicFunctionCompletion, DynamicFunctionConstructionError> {
    if source.kind() != DynamicFunctionKind::Function {
        return Err(DynamicFunctionConstructionError::UnsupportedKind {
            kind: source.kind(),
        });
    }

    let compiled = with_dynamic_function_source_and_prepared(
        source,
        limits.frontend,
        move |unit, _prepared| {
            let compiler =
                CompilationContext::new_with_source_name(unit, Arc::from("<dynamic Function>"))
                    .map_err(DynamicFunctionCompilerError::Planning)?;
            compiler
                .compile_dynamic_function_script_with_all_limits(
                    limits.bytecode,
                    limits.function_graph,
                    limits.final_graph,
                )
                .map_err(DynamicFunctionCompilerError::Lowering)
        },
    )
    .map_err(DynamicFunctionConstructionError::Frontend)?;

    let (compiled, prepared) = compiled;
    let tree = match compiled {
        Ok(tree) => tree,
        Err(source) => {
            return Err(DynamicFunctionConstructionError::Compiler { source, prepared });
        }
    };
    let authority = Arc::new(tree.verified_bytecode().clone());
    let value = match context.execute_dynamic_function_script(authority, limits.execution) {
        Ok(value) => value,
        Err(source) => {
            return Err(DynamicFunctionConstructionError::Runtime { source, prepared });
        }
    };

    Ok(DynamicFunctionCompletion { value, prepared })
}
