//! Host-facing composition for the safe, pure-Rust `QuickJS` port.
//!
//! The lower-level runtime consumes only immutable verified bytecode. This
//! facade owns pipelines that must cross the isolated Oxc frontend, compiler,
//! final verifier, and runtime installation boundaries in that order.

#![forbid(unsafe_code)]

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{
    BytecodeGraphVerificationLimits, FunctionGraphVerificationLimits, VerificationLimits,
    VerifiedBytecode,
};
use quickjs_compiler::{CompilationContext, CompilerError, LeafCompilationError};
use quickjs_frontend::{
    DiagnosticStage, DynamicFunctionError, DynamicFunctionKind, DynamicFunctionSource,
    FrontendLimits, PreparedDynamicFunctionSource, SourceFragment,
    with_dynamic_function_source_and_prepared,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, DynamicFunctionCompileRequest, DynamicFunctionCompiler,
    DynamicFunctionFamily, DynamicFunctionScriptError, ExecutionError, ExecutionLimits, Function,
    JsString, JsValue,
};

/// Resource limits applied across every supported dynamic-function stage.
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

/// Oxc-backed compiler service for runtime-created synchronous functions.
///
/// The service is immutable and carries only explicit resource limits. Each
/// request is parsed in the isolated frontend, compiled as the exact dynamic
/// Function Script wrapper, and returns only complete verified bytecode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OxcDynamicFunctionCompiler {
    limits: DynamicFunctionLimits,
}

impl OxcDynamicFunctionCompiler {
    /// Creates a dynamic-function compiler with explicit limits.
    #[must_use]
    pub const fn new(limits: DynamicFunctionLimits) -> Self {
        Self { limits }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicFunctionEngineStage {
    SourceConversion,
    SyntaxMessageConversion,
    Frontend(DiagnosticStage),
    CompilerPlanning,
    CompilerLowering,
    UnsupportedKind,
    UnexpectedRuntime,
}

impl fmt::Display for DynamicFunctionEngineStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceConversion => formatter.write_str("source-conversion"),
            Self::SyntaxMessageConversion => formatter.write_str("syntax-message-conversion"),
            Self::Frontend(stage) => write!(formatter, "frontend-{stage}"),
            Self::CompilerPlanning => formatter.write_str("compiler-planning"),
            Self::CompilerLowering => formatter.write_str("compiler-lowering"),
            Self::UnsupportedKind => formatter.write_str("dynamic-function-kind"),
            Self::UnexpectedRuntime => formatter.write_str("unexpected-runtime"),
        }
    }
}

#[derive(Debug)]
struct OxcDynamicFunctionEngineError {
    stage: DynamicFunctionEngineStage,
    detail: Arc<str>,
    source: Option<Arc<dyn Error + Send + Sync>>,
}

impl fmt::Display for OxcDynamicFunctionEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.stage, self.detail)
    }
}

impl Error for OxcDynamicFunctionEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn Error + 'static))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeSourceFragment {
    Parameter(usize),
    Body,
}

impl fmt::Display for RuntimeSourceFragment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parameter(index) => write!(formatter, "parameter fragment {index}"),
            Self::Body => formatter.write_str("body fragment"),
        }
    }
}

impl DynamicFunctionCompiler for OxcDynamicFunctionCompiler {
    fn compile(
        &self,
        source: DynamicFunctionCompileRequest,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        let kind = match source.family() {
            DynamicFunctionFamily::Function => DynamicFunctionKind::Function,
            DynamicFunctionFamily::GeneratorFunction => DynamicFunctionKind::GeneratorFunction,
        };
        let mut parameter_texts = Vec::new();
        parameter_texts
            .try_reserve_exact(source.parameters().len())
            .map_err(|error| {
                engine_failure_with_source(
                    DynamicFunctionEngineStage::SourceConversion,
                    format!(
                        "could not reserve {} converted parameter fragments",
                        source.parameters().len()
                    ),
                    error,
                )
            })?;
        for (index, parameter) in source.parameters().iter().enumerate() {
            parameter_texts.push(js_string_to_utf8(
                parameter,
                RuntimeSourceFragment::Parameter(index),
            )?);
        }
        let body_text = js_string_to_utf8(source.body(), RuntimeSourceFragment::Body)?;

        let mut parameters = Vec::new();
        parameters
            .try_reserve_exact(parameter_texts.len())
            .map_err(|error| {
                engine_failure_with_source(
                    DynamicFunctionEngineStage::SourceConversion,
                    format!(
                        "could not reserve {} frontend parameter fragments",
                        parameter_texts.len()
                    ),
                    error,
                )
            })?;
        parameters.extend(
            parameter_texts
                .iter()
                .map(|text| SourceFragment::new(text.as_str())),
        );
        let source = DynamicFunctionSource::new(kind, &parameters, SourceFragment::new(&body_text));
        compile_dynamic_function_source(source, self.limits)
            .map(|compiled| compiled.authority)
            .map_err(map_service_compilation_error)
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

/// Failure of the dynamic-function host pipeline.
#[derive(Debug)]
pub enum DynamicFunctionConstructionError {
    /// An asynchronous constructor family remains deliberately disabled.
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

struct CompiledDynamicFunctionSource {
    authority: Arc<VerifiedBytecode>,
    prepared: PreparedDynamicFunctionSource,
}

/// Constructs and executes one supported dynamic-function wrapper.
///
/// Inputs are already coerced UTF-8 source fragments. The complete exact
/// wrapper is parsed in an isolated Oxc arena, lowered as a Script root,
/// final-verified as one whole [`quickjs_bytecode::VerifiedBytecode`] graph,
/// and installed in `context`'s realm. It never receives or captures a caller
/// lexical frame and never uses eval bytecode. Asynchronous families remain
/// fail closed.
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
    let compiled = compile_dynamic_function_source(source, limits)?;
    let CompiledDynamicFunctionSource {
        authority,
        prepared,
    } = compiled;
    let dynamic_service: Arc<dyn DynamicFunctionCompiler> =
        Arc::new(OxcDynamicFunctionCompiler::new(limits));
    let value = match context.execute_dynamic_function_script_with_dynamic_function_compiler(
        authority,
        limits.execution,
        &dynamic_service,
    ) {
        Ok(value) => value,
        Err(source) => {
            return Err(DynamicFunctionConstructionError::Runtime { source, prepared });
        }
    };

    Ok(DynamicFunctionCompletion { value, prepared })
}

/// Calls a runtime function with the published Oxc dynamic-function compiler
/// available to every nested `%Function%` and `%GeneratorFunction%` invocation.
///
/// One immutable [`Arc`] service is shared for the complete iterative
/// interpreter session. Generated functions compile in the native
/// constructor's home realm and never receive the caller's lexical frame.
///
/// # Errors
///
/// Returns ordinary execution failures plus catchable dynamic-source
/// `SyntaxError`s and typed fail-closed compiler or resource failures.
pub fn call_with_dynamic_function_support(
    context: &mut Context<'_>,
    function: &Function,
    arguments: &[JsValue],
    limits: DynamicFunctionLimits,
) -> Result<JsValue, ExecutionError> {
    let dynamic_service: Arc<dyn DynamicFunctionCompiler> =
        Arc::new(OxcDynamicFunctionCompiler::new(limits));
    context.call_with_dynamic_function_compiler(
        function,
        arguments,
        limits.execution,
        &dynamic_service,
    )
}

#[allow(
    clippy::result_large_err,
    reason = "post-preparation failures intentionally retain the exact generated wrapper and map"
)]
fn compile_dynamic_function_source(
    source: DynamicFunctionSource<'_>,
    limits: DynamicFunctionLimits,
) -> Result<CompiledDynamicFunctionSource, DynamicFunctionConstructionError> {
    if !matches!(
        source.kind(),
        DynamicFunctionKind::Function | DynamicFunctionKind::GeneratorFunction
    ) {
        return Err(DynamicFunctionConstructionError::UnsupportedKind {
            kind: source.kind(),
        });
    }

    let compiled = with_dynamic_function_source_and_prepared(
        source,
        limits.frontend,
        move |unit, _prepared| {
            let source_name: Arc<str> = match source.kind() {
                DynamicFunctionKind::Function => Arc::from("<dynamic Function>"),
                DynamicFunctionKind::GeneratorFunction => Arc::from("<dynamic GeneratorFunction>"),
                DynamicFunctionKind::AsyncFunction
                | DynamicFunctionKind::AsyncGeneratorFunction => {
                    return Err(DynamicFunctionCompilerError::Planning(
                        CompilerError::Unsupported {
                            feature: quickjs_compiler::UnsupportedFeature::DynamicFunctionKind(
                                source.kind(),
                            ),
                            span: unit.program().span,
                        },
                    ));
                }
            };
            let compiler = CompilationContext::new_with_source_name(unit, source_name)
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
    Ok(CompiledDynamicFunctionSource {
        authority,
        prepared,
    })
}

fn map_service_compilation_error(
    error: DynamicFunctionConstructionError,
) -> DynamicFunctionCompileFailure {
    match error {
        DynamicFunctionConstructionError::UnsupportedKind { kind } => engine_failure(
            DynamicFunctionEngineStage::UnsupportedKind,
            format!("ordinary compiler received unsupported {kind} source"),
        ),
        DynamicFunctionConstructionError::Frontend(source)
            if matches!(
                source.stage(),
                DiagnosticStage::Parser | DiagnosticStage::Profile | DiagnosticStage::Semantic
            ) =>
        {
            let Some(message) = source
                .diagnostics()
                .first()
                .map(|diagnostic| diagnostic.message.as_str())
            else {
                return engine_failure_with_source(
                    DynamicFunctionEngineStage::Frontend(source.stage()),
                    "front end rejected dynamic source without a normalized diagnostic",
                    source,
                );
            };
            match JsString::from_utf8(message) {
                Ok(message) => DynamicFunctionCompileFailure::Syntax { message },
                Err(error) => engine_failure_with_source(
                    DynamicFunctionEngineStage::SyntaxMessageConversion,
                    "could not retain the normalized syntax diagnostic as a JavaScript string",
                    error,
                ),
            }
        }
        DynamicFunctionConstructionError::Frontend(source) => {
            let stage = DynamicFunctionEngineStage::Frontend(source.stage());
            let detail = source.diagnostics().first().map_or_else(
                || source.to_string(),
                |diagnostic| diagnostic.message.clone(),
            );
            engine_failure_with_source(stage, detail, source)
        }
        DynamicFunctionConstructionError::Compiler {
            source,
            prepared: _,
        } => {
            let stage = match &source {
                DynamicFunctionCompilerError::Planning(_) => {
                    DynamicFunctionEngineStage::CompilerPlanning
                }
                DynamicFunctionCompilerError::Lowering(_) => {
                    DynamicFunctionEngineStage::CompilerLowering
                }
            };
            engine_failure_with_source(stage, source.to_string(), source)
        }
        DynamicFunctionConstructionError::Runtime {
            source,
            prepared: _,
        } => engine_failure(
            DynamicFunctionEngineStage::UnexpectedRuntime,
            format!("compile-only path reached runtime failure: {source}"),
        ),
    }
}

fn js_string_to_utf8(
    source: &JsString,
    fragment: RuntimeSourceFragment,
) -> Result<String, DynamicFunctionCompileFailure> {
    let code_unit_count = usize::try_from(source.len()).map_err(|error| {
        engine_failure_with_source(
            DynamicFunctionEngineStage::SourceConversion,
            format!("{fragment} length does not fit the host address space"),
            error,
        )
    })?;
    let capacity = code_unit_count.checked_mul(3).ok_or_else(|| {
        engine_failure(
            DynamicFunctionEngineStage::SourceConversion,
            format!("{fragment} UTF-8 capacity overflowed"),
        )
    })?;
    let mut text = String::new();
    text.try_reserve_exact(capacity).map_err(|error| {
        engine_failure_with_source(
            DynamicFunctionEngineStage::SourceConversion,
            format!("could not reserve {capacity} UTF-8 bytes for {fragment}"),
            error,
        )
    })?;

    let mut offset = 0_u32;
    let mut units = source.code_units().peekable();
    while let Some(unit) = units.next() {
        let (scalar, width) = if (0xd800..=0xdbff).contains(&unit) {
            let Some(&low) = units.peek() else {
                return Err(lone_surrogate(fragment, offset, unit));
            };
            if !(0xdc00..=0xdfff).contains(&low) {
                return Err(lone_surrogate(fragment, offset, unit));
            }
            let _ = units.next();
            (
                0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
                2,
            )
        } else if (0xdc00..=0xdfff).contains(&unit) {
            return Err(lone_surrogate(fragment, offset, unit));
        } else {
            (u32::from(unit), 1)
        };
        let Some(character) = char::from_u32(scalar) else {
            return Err(engine_failure(
                DynamicFunctionEngineStage::SourceConversion,
                format!("{fragment} produced invalid Unicode scalar U+{scalar:04X}"),
            ));
        };
        text.push(character);
        offset += width;
    }
    Ok(text)
}

fn lone_surrogate(
    fragment: RuntimeSourceFragment,
    offset: u32,
    surrogate: u16,
) -> DynamicFunctionCompileFailure {
    engine_failure(
        DynamicFunctionEngineStage::SourceConversion,
        format!(
            "{fragment} contains lone UTF-16 surrogate U+{surrogate:04X} at code unit {offset}"
        ),
    )
}

fn engine_failure(
    stage: DynamicFunctionEngineStage,
    detail: impl Into<Arc<str>>,
) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(OxcDynamicFunctionEngineError {
            stage,
            detail: detail.into(),
            source: None,
        }),
    }
}

fn engine_failure_with_source(
    stage: DynamicFunctionEngineStage,
    detail: impl Into<Arc<str>>,
    source: impl Error + Send + Sync + 'static,
) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(OxcDynamicFunctionEngineError {
            stage,
            detail: detail.into(),
            source: Some(Arc::new(source)),
        }),
    }
}
