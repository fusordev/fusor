//! Host-facing composition for the safe, pure-Rust `QuickJS` port.
//!
//! The lower-level runtime consumes only immutable verified bytecode. This
//! facade owns pipelines that must cross the isolated Oxc frontend, compiler,
//! final verifier, and runtime installation boundaries in that order.

#![forbid(unsafe_code)]

use std::{collections::HashSet, error::Error, fmt, sync::Arc};

use quickjs_bytecode::{
    BytecodeGraphVerificationLimits, CompilerBindingKind, CompilerBindingPolicy,
    CompilerInitializationPolicy, CompilerWritePolicy, FunctionGraphVerificationLimits,
    VerificationLimits, VerifiedBytecode,
};
use quickjs_compiler::{
    CompilationContext, CompilerError, LeafCompilationError, SourceTextSubstitution,
};
pub use quickjs_diagnostics::{
    ByteSpan, ColumnEncoding, Diagnostic, DiagnosticCode, DiagnosticCodeError, DiagnosticLabel,
    DiagnosticReport, DiagnosticSeverity, LineColumn, OriginalLocation, PrettyDiagnostic,
    PrettyDiagnosticError, PrettyDiagnosticReport, ResolvedLocation, ResolvedSpan, SourceError,
    SourceFile, SourceId, SourceMap, SourceMapError, SourceMapErrorKind, SourceMapMapping,
    SourceMapPosition, SourceRegistry, SourceSnippet, SourceSpan, render_pretty,
    render_pretty_report,
};
use quickjs_frontend::{
    CompilationGoal, DiagnosticStage, DirectEvalBinding as FrontendDirectEvalBinding,
    DirectEvalBindingKind as FrontendDirectEvalBindingKind,
    DirectEvalBindingLocation as FrontendDirectEvalBindingLocation,
    DirectEvalBindingScope as FrontendDirectEvalBindingScope,
    DirectEvalCapabilities as FrontendDirectEvalCapabilities, DirectEvalContext,
    DirectEvalPrivateName as FrontendDirectEvalPrivateName,
    DirectEvalScopeFrame as FrontendDirectEvalScopeFrame,
    DirectEvalScopeKind as FrontendDirectEvalScopeKind, DirectEvalScopeSnapshot,
    DirectEvalVariableEnvironment as FrontendDirectEvalVariableEnvironment, DynamicFunctionError,
    DynamicFunctionKind, DynamicFunctionSource, FrontendError, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, IndirectEvalGoal, PreparedDynamicFunctionSource, RegisteredFrontendError,
    SourceFragment, Span, with_dynamic_function_source_and_prepared, with_parsed_program,
    with_registered_program,
};
use quickjs_runtime::{
    Context, DirectEvalCallerBindingLocation, DirectEvalCallerBindingScope,
    DirectEvalCompileRequest, DirectEvalVariableEnvironment, DynamicFunctionCompileFailure,
    DynamicFunctionCompileRequest, DynamicFunctionCompiler, DynamicFunctionFamily,
    DynamicFunctionScriptError, ExecutionError, ExecutionLimits, Function, GlobalScriptError,
    IndirectEvalCompileRequest, InstallError, JsString, JsValue, RuntimeDiagnosticError,
};

/// Resource limits applied across Global Script parsing, compilation,
/// verification, and execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScriptLimits {
    frontend: FrontendLimits,
    bytecode: VerificationLimits,
    function_graph: FunctionGraphVerificationLimits,
    final_graph: BytecodeGraphVerificationLimits,
    execution: ExecutionLimits,
}

impl ScriptLimits {
    /// Replaces parser and semantic limits.
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

    /// Returns parser and semantic limits.
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

    const fn dynamic_function_limits(self) -> DynamicFunctionLimits {
        DynamicFunctionLimits {
            frontend: self.frontend,
            bytecode: self.bytecode,
            function_graph: self.function_graph,
            final_graph: self.final_graph,
            execution: self.execution,
        }
    }
}

/// Compiler stage that rejected an already parsed Global Script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptCompilerError {
    /// Storage planning or semantic preflight failed.
    Planning(CompilerError),
    /// Lowering or whole-graph verification failed.
    Lowering(LeafCompilationError),
}

impl fmt::Display for ScriptCompilerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planning(source) => source.fmt(formatter),
            Self::Lowering(source) => source.fmt(formatter),
        }
    }
}

impl Error for ScriptCompilerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Planning(source) => Some(source),
            Self::Lowering(source) => Some(source),
        }
    }
}

/// Failure of the Global Script host pipeline.
#[derive(Debug)]
pub enum ScriptEvaluationError {
    /// Parsing, the compatibility profile, or ECMAScript early errors failed.
    Frontend(FrontendError),
    /// The parsed Script could not become complete verified bytecode.
    Compiler(ScriptCompilerError),
    /// Realm installation or verified Script execution failed.
    Runtime(GlobalScriptError),
}

impl fmt::Display for ScriptEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(source) => source.fmt(formatter),
            Self::Compiler(source) => source.fmt(formatter),
            Self::Runtime(source) => source.fmt(formatter),
        }
    }
}

impl Error for ScriptEvaluationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frontend(source) => Some(source),
            Self::Compiler(source) => Some(source),
            Self::Runtime(source) => Some(source),
        }
    }
}

/// Exact stage that rejected a registered Global Script evaluation.
#[derive(Debug)]
#[non_exhaustive]
pub enum RegisteredScriptFailure {
    /// Registered source access, parsing, compatibility, or early errors.
    Frontend(RegisteredFrontendError),
    /// The parsed Script could not become complete verified bytecode.
    Compiler(ScriptCompilerError),
    /// Realm installation or verified Script execution failed.
    Runtime(GlobalScriptError),
}

impl fmt::Display for RegisteredScriptFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(error) => error.fmt(formatter),
            Self::Compiler(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
        }
    }
}

impl Error for RegisteredScriptFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frontend(error) => Some(error),
            Self::Compiler(error) => Some(error),
            Self::Runtime(error) => Some(error),
        }
    }
}

/// Failure of a registered Global Script pipeline with stable source identity.
#[derive(Debug)]
pub struct RegisteredScriptEvaluationError {
    source_id: SourceId,
    failure: RegisteredScriptFailure,
}

impl RegisteredScriptEvaluationError {
    fn new(source_id: SourceId, failure: RegisteredScriptFailure) -> Self {
        Self { source_id, failure }
    }

    /// Returns the registered generated source that was evaluated.
    #[must_use]
    pub const fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Returns the exact failing pipeline stage.
    #[must_use]
    pub const fn failure(&self) -> &RegisteredScriptFailure {
        &self.failure
    }

    /// Converts the failure into stable, source-map-resolved diagnostics ready
    /// for the shared Miette adapter.
    ///
    /// Frontend diagnostic batches use their first diagnostic as primary and
    /// retain the rest as related diagnostics. Compiler failures receive exact
    /// source labels when their typed error carries a span. Runtime exceptions
    /// retain their verified origin and caller stack as independently sourced
    /// related diagnostics.
    ///
    /// # Errors
    ///
    /// Returns a structured source, source-map, runtime-provenance, or internal
    /// stable-code conversion failure.
    pub fn diagnostic_report(
        &self,
        sources: &SourceRegistry,
    ) -> Result<DiagnosticReport, ScriptDiagnosticError> {
        match &self.failure {
            RegisteredScriptFailure::Frontend(error) => frontend_diagnostic_report(error, sources),
            RegisteredScriptFailure::Compiler(error) => {
                compiler_diagnostic_report(error, sources, &self.source_id)
            }
            RegisteredScriptFailure::Runtime(error) => {
                runtime_diagnostic_report(error, sources, &self.source_id)
            }
        }
    }
}

impl fmt::Display for RegisteredScriptEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.failure.fmt(formatter)
    }
}

impl Error for RegisteredScriptEvaluationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.failure)
    }
}

/// Failure while adapting a registered Script error to shared diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ScriptDiagnosticError {
    /// A stable engine-owned code failed validation.
    DiagnosticCode(DiagnosticCodeError),
    /// The registered source or a typed source span was invalid.
    Source(SourceError),
    /// Incoming source-map resolution failed.
    SourceMap(SourceMapError),
    /// Runtime frame provenance could not be validated.
    Runtime(RuntimeDiagnosticError),
    /// A frontend rejection unexpectedly contained no diagnostic.
    EmptyFrontendDiagnostics,
}

impl fmt::Display for ScriptDiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiagnosticCode(error) => write!(formatter, "invalid diagnostic code: {error}"),
            Self::Source(error) => write!(formatter, "invalid diagnostic source: {error}"),
            Self::SourceMap(error) => write!(formatter, "source-map resolution failed: {error}"),
            Self::Runtime(error) => write!(formatter, "runtime diagnostic failed: {error}"),
            Self::EmptyFrontendDiagnostics => {
                formatter.write_str("frontend rejection contained no diagnostics")
            }
        }
    }
}

impl Error for ScriptDiagnosticError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::DiagnosticCode(error) => Some(error),
            Self::Source(error) => Some(error),
            Self::SourceMap(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::EmptyFrontendDiagnostics => None,
        }
    }
}

impl From<DiagnosticCodeError> for ScriptDiagnosticError {
    fn from(error: DiagnosticCodeError) -> Self {
        Self::DiagnosticCode(error)
    }
}

impl From<SourceError> for ScriptDiagnosticError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<SourceMapError> for ScriptDiagnosticError {
    fn from(error: SourceMapError) -> Self {
        Self::SourceMap(error)
    }
}

impl From<RuntimeDiagnosticError> for ScriptDiagnosticError {
    fn from(error: RuntimeDiagnosticError) -> Self {
        Self::Runtime(error)
    }
}

fn shared_diagnostic(
    code: &'static str,
    message: impl Into<String>,
) -> Result<Diagnostic, ScriptDiagnosticError> {
    Ok(Diagnostic::new(
        DiagnosticCode::new(code)?,
        DiagnosticSeverity::Error,
        message,
    ))
}

fn frontend_diagnostic_report(
    error: &RegisteredFrontendError,
    sources: &SourceRegistry,
) -> Result<DiagnosticReport, ScriptDiagnosticError> {
    let RegisteredFrontendError::Diagnostics(diagnostics) = error else {
        return Ok(DiagnosticReport::new(shared_diagnostic(
            "quickjs::frontend::source_integration",
            error.to_string(),
        )?));
    };
    let mut diagnostics = diagnostics.diagnostics().iter();
    let primary = diagnostics
        .next()
        .ok_or(ScriptDiagnosticError::EmptyFrontendDiagnostics)?
        .clone();
    DiagnosticReport::new(primary)
        .with_related_diagnostics(diagnostics.cloned())
        .resolve_source_maps(sources)
        .map_err(Into::into)
}

fn source_label(
    sources: &SourceRegistry,
    source_id: &SourceId,
    span: Span,
    message: &'static str,
    primary: bool,
) -> Result<DiagnosticLabel, ScriptDiagnosticError> {
    let generated = sources.span(source_id, span.start as usize, span.end as usize)?;
    let resolved = sources.resolve_span(&generated)?;
    Ok(if primary {
        DiagnosticLabel::primary(resolved.display_span().clone(), Some(message.to_owned()))
    } else {
        DiagnosticLabel::secondary(resolved.display_span().clone(), Some(message.to_owned()))
    })
}

fn planning_diagnostic(
    error: &CompilerError,
    sources: &SourceRegistry,
    source_id: &SourceId,
) -> Result<Diagnostic, ScriptDiagnosticError> {
    let (code, span, help) = match error {
        CompilerError::Unsupported { span, .. } => (
            "quickjs::compiler::planning::unsupported",
            Some(*span),
            Some("the syntax parsed successfully but its runtime semantics are not admitted yet"),
        ),
        CompilerError::SemanticInvariant { span, .. } => (
            "quickjs::compiler::planning::semantic_invariant",
            *span,
            None,
        ),
        CompilerError::CapacityExceeded { .. } => {
            ("quickjs::compiler::planning::capacity_exceeded", None, None)
        }
    };
    let mut diagnostic = shared_diagnostic(code, error.to_string())?;
    if let Some(help) = help {
        diagnostic = diagnostic.with_help(help);
    }
    if let Some(span) = span {
        diagnostic = diagnostic.with_label(source_label(
            sources,
            source_id,
            span,
            "compiler planning rejected this syntax",
            true,
        )?);
    }
    Ok(diagnostic)
}

fn lowering_code(error: &LeafCompilationError) -> &'static str {
    match error {
        LeafCompilationError::ForeignExecutable { .. } => {
            "quickjs::compiler::lowering::foreign_executable"
        }
        LeafCompilationError::InvalidExecutable { .. } => {
            "quickjs::compiler::lowering::invalid_executable"
        }
        LeafCompilationError::Unsupported { .. } => "quickjs::compiler::lowering::unsupported",
        LeafCompilationError::SemanticInvariant { .. } => {
            "quickjs::compiler::lowering::semantic_invariant"
        }
        LeafCompilationError::EvalDeclarationConflict { .. } => {
            "quickjs::compiler::lowering::eval_declaration_conflict"
        }
        LeafCompilationError::CapacityExceeded { .. } => {
            "quickjs::compiler::lowering::capacity_exceeded"
        }
        LeafCompilationError::CookedStringDecoding { .. } => {
            "quickjs::compiler::lowering::cooked_string"
        }
        LeafCompilationError::CompilerString { .. } => {
            "quickjs::compiler::lowering::compiler_string"
        }
        LeafCompilationError::CompilerBigInt { .. } => {
            "quickjs::compiler::lowering::compiler_bigint"
        }
        LeafCompilationError::CompilerTemplateObject { .. } => {
            "quickjs::compiler::lowering::template_object"
        }
        LeafCompilationError::RegExp { .. } => "quickjs::compiler::lowering::regexp",
        LeafCompilationError::BytecodeEncoding { .. } => {
            "quickjs::compiler::lowering::bytecode_encoding"
        }
        LeafCompilationError::BytecodeAssembly { .. } => {
            "quickjs::compiler::lowering::bytecode_assembly"
        }
        LeafCompilationError::BytecodeStackInvariant { .. } => {
            "quickjs::compiler::lowering::stack_invariant"
        }
        LeafCompilationError::BytecodeVerification { .. } => {
            "quickjs::compiler::lowering::bytecode_verification"
        }
        LeafCompilationError::FunctionGraphVerification { .. } => {
            "quickjs::compiler::lowering::function_graph_verification"
        }
        LeafCompilationError::BytecodeGraphVerification { .. } => {
            "quickjs::compiler::lowering::bytecode_graph_verification"
        }
    }
}

fn lowering_spans(error: &LeafCompilationError) -> (Option<Span>, Option<Span>) {
    match error {
        LeafCompilationError::Unsupported { span, .. }
        | LeafCompilationError::CookedStringDecoding { span, .. }
        | LeafCompilationError::CompilerString { span, .. }
        | LeafCompilationError::CompilerBigInt { span, .. }
        | LeafCompilationError::CompilerTemplateObject { span, .. }
        | LeafCompilationError::RegExp { span, .. }
        | LeafCompilationError::BytecodeEncoding { span, .. }
        | LeafCompilationError::BytecodeStackInvariant { span, .. }
        | LeafCompilationError::EvalDeclarationConflict { span, .. } => (Some(*span), None),
        LeafCompilationError::SemanticInvariant { span, .. }
        | LeafCompilationError::BytecodeAssembly { span, .. }
        | LeafCompilationError::FunctionGraphVerification { span, .. }
        | LeafCompilationError::BytecodeGraphVerification { span, .. } => (*span, None),
        LeafCompilationError::BytecodeVerification {
            span, related_span, ..
        } => (*span, *related_span),
        LeafCompilationError::ForeignExecutable { .. }
        | LeafCompilationError::InvalidExecutable { .. }
        | LeafCompilationError::CapacityExceeded { .. } => (None, None),
    }
}

fn lowering_diagnostic(
    error: &LeafCompilationError,
    sources: &SourceRegistry,
    source_id: &SourceId,
) -> Result<Diagnostic, ScriptDiagnosticError> {
    let mut diagnostic = shared_diagnostic(lowering_code(error), error.to_string())?;
    let (span, related_span) = lowering_spans(error);
    if let Some(span) = span {
        diagnostic = diagnostic.with_label(source_label(
            sources,
            source_id,
            span,
            "lowering failed here",
            true,
        )?);
    }
    if let Some(span) = related_span {
        diagnostic = diagnostic.with_label(source_label(
            sources,
            source_id,
            span,
            "related control-flow location",
            false,
        )?);
    }
    Ok(diagnostic)
}

fn compiler_diagnostic_report(
    error: &ScriptCompilerError,
    sources: &SourceRegistry,
    source_id: &SourceId,
) -> Result<DiagnosticReport, ScriptDiagnosticError> {
    let diagnostic = match error {
        ScriptCompilerError::Planning(error) => planning_diagnostic(error, sources, source_id)?,
        ScriptCompilerError::Lowering(error) => lowering_diagnostic(error, sources, source_id)?,
    };
    Ok(DiagnosticReport::new(diagnostic))
}

fn install_span(error: &InstallError) -> Option<quickjs_bytecode::SourceByteSpan> {
    match error {
        InstallError::UnsupportedOpcode { source_span, .. }
        | InstallError::GlobalDeclarationRejected { source_span, .. } => Some(*source_span),
        InstallError::LimitExceeded { .. }
        | InstallError::AllocationFailed { .. }
        | InstallError::String(_)
        | InstallError::BigInt(_)
        | InstallError::Atom(_)
        | InstallError::AuthorityInvariant { .. } => None,
    }
}

fn runtime_diagnostic_report(
    error: &GlobalScriptError,
    sources: &SourceRegistry,
    source_id: &SourceId,
) -> Result<DiagnosticReport, ScriptDiagnosticError> {
    let GlobalScriptError::Install(install) = error else {
        return error.to_diagnostic_report(sources).map_err(Into::into);
    };
    let mut diagnostic = install.to_diagnostic()?;
    if let Some(span) = install_span(install) {
        let generated = sources.span(source_id, span.start() as usize, span.end() as usize)?;
        let resolved = sources.resolve_span(&generated)?;
        diagnostic = diagnostic.with_label(DiagnosticLabel::primary(
            resolved.display_span().clone(),
            Some("installation failed here".to_owned()),
        ));
    }
    Ok(DiagnosticReport::new(diagnostic))
}

/// Parses, compiles, final-verifies, installs, and executes one host-loaded
/// ECMAScript Global Script.
///
/// The source is never rewritten as a function body. Its exact Script
/// completion is returned, and successfully instantiated global bindings stay
/// in `context`'s realm for later Script evaluations.
///
/// # Errors
///
/// Returns the exact failing frontend, compiler, installation, or execution
/// stage.
pub fn evaluate_script(
    context: &mut Context<'_>,
    source_text: &str,
    source_name: &str,
    limits: ScriptLimits,
) -> Result<JsValue, ScriptEvaluationError> {
    let source_name: Arc<str> = Arc::from(source_name);
    let compiled = with_parsed_program(
        source_text,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new()))
            .with_limits(limits.frontend),
        move |unit| {
            let compiler = CompilationContext::new_with_source_name(unit, source_name)
                .map_err(ScriptCompilerError::Planning)?;
            compiler
                .compile_global_script_with_all_limits(
                    limits.bytecode,
                    limits.function_graph,
                    limits.final_graph,
                )
                .map_err(ScriptCompilerError::Lowering)
        },
    )
    .map_err(ScriptEvaluationError::Frontend)?
    .map_err(ScriptEvaluationError::Compiler)?;
    let authority = Arc::new(compiled.verified_bytecode().clone());
    let dynamic_service: Arc<dyn DynamicFunctionCompiler> = Arc::new(
        OxcDynamicFunctionCompiler::new(limits.dynamic_function_limits()),
    );
    context
        .execute_global_script_with_dynamic_function_compiler(
            authority,
            limits.execution,
            &dynamic_service,
        )
        .map_err(ScriptEvaluationError::Runtime)
}

/// Parses, compiles, final-verifies, installs, and executes one registered
/// ECMAScript Global Script.
///
/// Source text and display identity come from `sources`; any incoming source
/// map registered on `source_id` remains available to
/// [`RegisteredScriptEvaluationError::diagnostic_report`]. Successfully
/// instantiated global bindings stay in `context`'s realm for later Script
/// evaluations.
///
/// # Errors
///
/// Returns a [`RegisteredScriptEvaluationError`] retaining the registered
/// source identity and the exact failing frontend, compiler, installation, or
/// execution stage.
pub fn evaluate_registered_script(
    context: &mut Context<'_>,
    sources: &SourceRegistry,
    source_id: &SourceId,
    limits: ScriptLimits,
) -> Result<JsValue, RegisteredScriptEvaluationError> {
    let source_name = match sources.source(source_id) {
        Ok(source) => Arc::<str>::from(source.display_name()),
        Err(error) => {
            let failure = RegisteredScriptFailure::Frontend(RegisteredFrontendError::Source(
                quickjs_frontend::FrontendSourceError::Registry(error),
            ));
            return Err(RegisteredScriptEvaluationError::new(
                source_id.clone(),
                failure,
            ));
        }
    };
    let compiled = with_registered_program(
        sources,
        source_id,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new()))
            .with_limits(limits.frontend),
        move |unit| {
            let compiler = CompilationContext::new_with_source_name(unit, source_name)
                .map_err(ScriptCompilerError::Planning)?;
            compiler
                .compile_global_script_with_all_limits(
                    limits.bytecode,
                    limits.function_graph,
                    limits.final_graph,
                )
                .map_err(ScriptCompilerError::Lowering)
        },
    )
    .map_err(|error| {
        RegisteredScriptEvaluationError::new(
            source_id.clone(),
            RegisteredScriptFailure::Frontend(error),
        )
    })?
    .map_err(|error| {
        RegisteredScriptEvaluationError::new(
            source_id.clone(),
            RegisteredScriptFailure::Compiler(error),
        )
    })?;
    let authority = Arc::new(compiled.verified_bytecode().clone());
    let dynamic_service: Arc<dyn DynamicFunctionCompiler> = Arc::new(
        OxcDynamicFunctionCompiler::new(limits.dynamic_function_limits()),
    );
    context
        .execute_global_script_with_dynamic_function_compiler(
            authority,
            limits.execution,
            &dynamic_service,
        )
        .map_err(|error| {
            RegisteredScriptEvaluationError::new(
                source_id.clone(),
                RegisteredScriptFailure::Runtime(error),
            )
        })
}

// ---- Module evaluation ----

/// A module source loaded by the host.
#[derive(Clone, Debug)]
pub struct LoadedModuleSource {
    /// Canonical key for this module within the realm.
    pub key: quickjs_runtime::ModuleKey,
    /// Module source text.
    pub source: String,
    /// Display name for diagnostics.
    pub display_name: String,
}

/// A host-side error loading module source.
#[derive(Debug)]
pub struct ModuleSourceError {
    message: String,
}

impl ModuleSourceError {
    /// Creates a load error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModuleSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "module load error: {}", self.message)
    }
}

impl Error for ModuleSourceError {}

/// Host loader for module source text and resolution.
///
/// The facade calls `load_module` for each unique specifier it encounters while
/// gathering the module graph. The returned [`LoadedModuleSource`] provides the
/// canonical key (for deduplication), source text, and display name.
pub trait ModuleSourceLoader {
    /// Loads a module by specifier, resolving relative to the referrer.
    ///
    /// # Errors
    ///
    /// Returns a [`ModuleSourceError`] when the module cannot be found or loaded.
    fn load_module(
        &mut self,
        specifier: &str,
        referrer: Option<&str>,
    ) -> Result<LoadedModuleSource, ModuleSourceError>;
}

/// A compiled module entry: syntax record + verified bytecode.
struct CompiledModule {
    key: quickjs_runtime::ModuleKey,
    syntax_record: quickjs_frontend::ModuleSyntaxRecord,
    authority: Arc<VerifiedBytecode>,
}

/// Failure of the Module host pipeline.
#[derive(Debug)]
pub enum ModuleEvaluationError {
    /// Host loader failed to resolve or load a module.
    Loader(ModuleSourceError),
    /// Parsing, compatibility, or early errors.
    Frontend(FrontendError),
    /// Compilation or verification failure.
    Compiler(ScriptCompilerError),
    /// Linking or evaluation failure.
    Runtime(quickjs_runtime::ModuleError),
    /// Host-job execution failure while settling parked dynamic `import()`
    /// loads.
    Execution(ExecutionError),
}

impl fmt::Display for ModuleEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader(error) => error.fmt(formatter),
            Self::Frontend(error) => error.fmt(formatter),
            Self::Compiler(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::Execution(error) => error.fmt(formatter),
        }
    }
}

impl Error for ModuleEvaluationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Loader(error) => Some(error),
            Self::Frontend(error) => Some(error),
            Self::Compiler(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Execution(error) => Some(error),
        }
    }
}

impl From<ModuleSourceError> for ModuleEvaluationError {
    fn from(error: ModuleSourceError) -> Self {
        Self::Loader(error)
    }
}

impl From<FrontendError> for ModuleEvaluationError {
    fn from(error: FrontendError) -> Self {
        Self::Frontend(error)
    }
}

impl From<ScriptCompilerError> for ModuleEvaluationError {
    fn from(error: ScriptCompilerError) -> Self {
        Self::Compiler(error)
    }
}

impl From<quickjs_runtime::ModuleError> for ModuleEvaluationError {
    fn from(error: quickjs_runtime::ModuleError) -> Self {
        Self::Runtime(error)
    }
}

impl From<ExecutionError> for ModuleEvaluationError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

fn compile_module_source(
    source_text: &str,
    source_name: &str,
    limits: ScriptLimits,
    key: quickjs_runtime::ModuleKey,
) -> Result<CompiledModule, ModuleEvaluationError> {
    let source_name: Arc<str> = Arc::from(source_name);
    let compiled = with_parsed_program(
        source_text,
        FrontendOptions::for_goal(CompilationGoal::Module).with_limits(limits.frontend),
        move |unit| {
            let syntax_record = unit.module_syntax().clone();
            let compiler = CompilationContext::new_with_source_name(unit, source_name)
                .map_err(ScriptCompilerError::Planning)?;
            let tree = compiler
                .compile_module_with_all_limits(
                    limits.bytecode,
                    limits.function_graph,
                    limits.final_graph,
                )
                .map_err(ScriptCompilerError::Lowering)?;
            Ok::<_, ScriptCompilerError>((syntax_record, tree))
        },
    )
    .map_err(ModuleEvaluationError::Frontend)?
    .map_err(ModuleEvaluationError::Compiler)?;

    let (syntax_record, tree) = compiled;
    let authority = Arc::new(tree.verified_bytecode().clone());

    // Reject import attributes for now (JSON/text modules are a later step)
    for request in syntax_record.requests() {
        if request.attributes().is_some() {
            return Err(ModuleEvaluationError::Loader(ModuleSourceError::new(format!(
                "import attributes are not yet supported (request '{}')",
                String::from_utf8_lossy(
                    &request
                        .specifier()
                        .code_units()
                        .iter()
                        .copied()
                        .map(u32::from)
                        .filter_map(char::from_u32)
                        .collect::<String>()
                        .into_bytes(),
                ),
            ))));
        }
    }

    Ok(CompiledModule {
        key,
        syntax_record,
        authority,
    })
}

/// Parses, compiles, links, and evaluates an ECMAScript Module graph.
///
/// The root source is compiled as a Module goal. The `loader` provides source
/// text for each static import/re-export specifier encountered. All modules in
/// the graph are registered, linked, and evaluated synchronously.
///
/// Returns `undefined` (module completion is discarded per spec). To observe
/// module state, use `context.module_namespace` or evaluate a follow-up Script.
///
/// # Errors
///
/// Returns the exact failing frontend, compiler, loader, linking, or
/// evaluation stage.
pub fn evaluate_module(
    context: &mut Context<'_>,
    root_source: &str,
    root_name: &str,
    loader: &mut dyn ModuleSourceLoader,
    limits: ScriptLimits,
) -> Result<JsValue, ModuleEvaluationError> {
    // Compile root
    let root_key = quickjs_runtime::ModuleKey::new(Arc::from(root_name));
    let root_compiled = compile_module_source(root_source, root_name, limits, root_key.clone())?;

    // Register root
    context.register_module(
        root_compiled.key.clone(),
        root_compiled.syntax_record.clone(),
        root_compiled.authority.clone(),
    )?;

    // BFS: gather all dependencies
    let mut queue: Vec<(quickjs_runtime::ModuleKey, quickjs_frontend::ModuleSyntaxRecord)> =
        vec![(root_compiled.key.clone(), root_compiled.syntax_record.clone())];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(root_compiled.key.as_str().to_owned());

    while let Some((referrer_key, syntax)) = queue.pop() {
        for request in syntax.requests() {
            let specifier: String = request
                .specifier()
                .code_units()
                .iter()
                .copied()
                .map(u32::from)
                .filter_map(char::from_u32)
                .collect();
            let loaded = loader.load_module(&specifier, Some(referrer_key.as_str()))?;
            if !seen.insert(loaded.key.as_str().to_owned()) {
                // Cycle/diamond edge to an already-registered record: record
                // the (referrer, specifier) edge and skip re-registration.
                context.register_module_dependency(&referrer_key, &specifier, &loaded.key)?;
                continue;
            }
            let compiled = compile_module_source(
                &loaded.source,
                &loaded.display_name,
                limits,
                loaded.key.clone(),
            )?;
            context.register_module(
                compiled.key.clone(),
                compiled.syntax_record.clone(),
                compiled.authority.clone(),
            )?;
            // HostResolveImportedModule: record the (referrer, specifier)
            // edge now that both records are registered.
            context.register_module_dependency(&referrer_key, &specifier, &loaded.key)?;
            queue.push((compiled.key, compiled.syntax_record));
        }
    }

    // Link
    context.link_module(&root_compiled.key)?;

    // Evaluate (with a dynamic-function compiler so `eval`/`Function` work
    // inside module code).
    let dynamic_service: Arc<dyn DynamicFunctionCompiler> = Arc::new(
        OxcDynamicFunctionCompiler::new(limits.dynamic_function_limits()),
    );
    context.evaluate_module_with_dynamic_function_compiler(
        &root_compiled.key,
        limits.execution,
        &dynamic_service,
    )?;

    Ok(context.undefined_value())
}

/// Loads, registers, and compiles the graph requested by one parked dynamic
/// `import()`, returning the resolved root key.
///
/// Modules already registered in the realm (static graph, earlier imports)
/// are reused rather than re-registered, preserving registry dedup and
/// single-evaluation semantics.
fn gather_dynamic_import_graph(
    context: &mut Context<'_>,
    loader: &mut dyn ModuleSourceLoader,
    import: &quickjs_runtime::PendingDynamicImport,
    limits: ScriptLimits,
) -> Result<quickjs_runtime::ModuleKey, ModuleEvaluationError> {
    let specifier = import.specifier();
    let referrer = import.referrer().map(|key| key.as_str().to_owned());
    let loaded = loader.load_module(&specifier, referrer.as_deref())?;
    let root_key = loaded.key.clone();
    if context.has_module(&root_key) {
        // The graph root is already registered; record the referring module's
        // (referrer, specifier) edge and reuse the existing record.
        if let Some(referrer_key) = import.referrer() {
            context.register_module_dependency(referrer_key, &specifier, &root_key)?;
        }
        return Ok(root_key);
    }

    let root_compiled =
        compile_module_source(&loaded.source, &loaded.display_name, limits, root_key.clone())?;
    context.register_module(
        root_compiled.key.clone(),
        root_compiled.syntax_record.clone(),
        root_compiled.authority.clone(),
    )?;
    // A module-level import() records the (referrer, specifier) edge on the
    // referring module; a script-level import() has no referrer record, so no
    // edge is needed.
    if let Some(referrer_key) = import.referrer() {
        context.register_module_dependency(referrer_key, &specifier, &root_key)?;
    }

    // BFS: gather all dependencies, as in `evaluate_module`.
    let mut queue: Vec<(quickjs_runtime::ModuleKey, quickjs_frontend::ModuleSyntaxRecord)> =
        vec![(root_compiled.key.clone(), root_compiled.syntax_record.clone())];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(root_compiled.key.as_str().to_owned());

    while let Some((referrer_key, syntax)) = queue.pop() {
        for request in syntax.requests() {
            let specifier: String = request
                .specifier()
                .code_units()
                .iter()
                .copied()
                .map(u32::from)
                .filter_map(char::from_u32)
                .collect();
            let loaded = loader.load_module(&specifier, Some(referrer_key.as_str()))?;
            if context.has_module(&loaded.key) || !seen.insert(loaded.key.as_str().to_owned()) {
                // Cycle/diamond edge to an already-registered record: record
                // the (referrer, specifier) edge and skip re-registration.
                context.register_module_dependency(&referrer_key, &specifier, &loaded.key)?;
                continue;
            }
            let compiled = compile_module_source(
                &loaded.source,
                &loaded.display_name,
                limits,
                loaded.key.clone(),
            )?;
            context.register_module(
                compiled.key.clone(),
                compiled.syntax_record.clone(),
                compiled.authority.clone(),
            )?;
            // HostResolveImportedModule: record the (referrer, specifier)
            // edge now that both records are registered.
            context.register_module_dependency(&referrer_key, &specifier, &loaded.key)?;
            queue.push((compiled.key, compiled.syntax_record));
        }
    }
    Ok(root_key)
}

/// Drives parked dynamic `import()` loads to quiescence.
///
/// Contract: call this after [`evaluate_script`] or [`evaluate_module`] (and
/// again whenever a pump round may have parked new loads — the loop already
/// covers reactions and freshly evaluated modules that call `import()`).
/// Each round takes the oldest parked load from the runtime queue, resolves
/// and loads its graph through `loader`, registers the compiled records,
/// completes the import (link + evaluate + Promise settlement), and drains
/// queued Promise reaction jobs before taking the next load. Load,
/// resolution, and compile failures reject the import Promise; they never
/// throw out of this function. The function returns `Ok` once the queue is
/// empty.
///
/// # Errors
///
/// Returns a [`ModuleEvaluationError`] only for internal runtime failures
/// while settling imports or draining jobs.
pub fn pump_dynamic_imports(
    context: &mut Context<'_>,
    loader: &mut dyn ModuleSourceLoader,
    limits: ScriptLimits,
) -> Result<(), ModuleEvaluationError> {
    let dynamic_service: Arc<dyn DynamicFunctionCompiler> = Arc::new(
        OxcDynamicFunctionCompiler::new(limits.dynamic_function_limits()),
    );
    loop {
        // Drain first: reactions queued by an earlier settlement (or by a
        // rejection that never parked, such as unsupported attributes) run
        // before the next load, and may themselves park new imports.
        context.drain_host_jobs(limits.execution, Some(&dynamic_service))?;
        let Some(import) = context.take_pending_dynamic_import() else {
            return Ok(());
        };
        match gather_dynamic_import_graph(context, loader, &import, limits) {
            Ok(root_key) => context.complete_dynamic_import(
                import,
                &root_key,
                limits.execution,
                Some(&dynamic_service),
            )?,
            Err(error) => context.reject_dynamic_import(import, &error.to_string())?,
        }
    }
}

// ---- Dynamic function support continues ----

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
    IndirectEval,
    DirectEval,
}

impl fmt::Display for RuntimeSourceFragment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parameter(index) => write!(formatter, "parameter fragment {index}"),
            Self::Body => formatter.write_str("body fragment"),
            Self::IndirectEval => formatter.write_str("indirect eval source"),
            Self::DirectEval => formatter.write_str("direct eval source"),
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
            DynamicFunctionFamily::AsyncFunction => DynamicFunctionKind::AsyncFunction,
            DynamicFunctionFamily::AsyncGeneratorFunction => {
                DynamicFunctionKind::AsyncGeneratorFunction
            }
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

    fn compile_indirect_eval(
        &self,
        source: IndirectEvalCompileRequest,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        compile_indirect_eval_source(source.source(), self.limits)
    }

    fn compile_direct_eval(
        &self,
        source: DirectEvalCompileRequest,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        compile_direct_eval_source(&source, self.limits)
    }
}

fn compile_indirect_eval_source(
    source: &JsString,
    limits: DynamicFunctionLimits,
) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
    let source = js_eval_source_to_utf8(source, RuntimeSourceFragment::IndirectEval)?;
    let compiled = with_parsed_program(
        &source.text,
        FrontendOptions::for_goal(CompilationGoal::IndirectEval(IndirectEvalGoal::new()))
            .with_limits(limits.frontend),
        |unit| {
            let compiler = CompilationContext::new_with_source_name_and_substitutions(
                unit,
                Arc::from("<eval>"),
                Arc::clone(&source.substitutions),
            )
            .map_err(DynamicFunctionCompilerError::Planning)?;
            compiler
                .compile_indirect_eval_script_with_all_limits(
                    limits.bytecode,
                    limits.function_graph,
                    limits.final_graph,
                )
                .map_err(DynamicFunctionCompilerError::Lowering)
        },
    )
    .map_err(map_eval_frontend_error)?
    .map_err(|source| {
        let stage = match &source {
            DynamicFunctionCompilerError::Planning(_) => {
                DynamicFunctionEngineStage::CompilerPlanning
            }
            DynamicFunctionCompilerError::Lowering(_) => {
                DynamicFunctionEngineStage::CompilerLowering
            }
        };
        engine_failure_with_source(stage, source.to_string(), source)
    })?;
    Ok(Arc::new(compiled.verified_bytecode().clone()))
}

#[derive(Clone, Copy)]
enum FrontendDirectEvalCallerEntry {
    Binding(usize),
    PrivateName(usize),
}

struct FrontendDirectEvalEntries<'names> {
    bindings: Vec<FrontendDirectEvalBinding<'names>>,
    private_names: Vec<FrontendDirectEvalPrivateName<'names>>,
    order: Vec<FrontendDirectEvalCallerEntry>,
}

fn frontend_direct_eval_entries<'names>(
    request: &DirectEvalCompileRequest,
    names: &'names [String],
) -> Result<FrontendDirectEvalEntries<'names>, DynamicFunctionCompileFailure> {
    let count = request.bindings().len();
    let reserve_failure = |domain, error| {
        engine_failure_with_source(
            DynamicFunctionEngineStage::SourceConversion,
            format!("could not reserve {count} direct-eval {domain}"),
            error,
        )
    };
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(count)
        .map_err(|error| reserve_failure("binding descriptors", error))?;
    let mut private_names = Vec::new();
    private_names
        .try_reserve_exact(count)
        .map_err(|error| reserve_failure("private-name descriptors", error))?;
    let mut order = Vec::new();
    order
        .try_reserve_exact(count)
        .map_err(|error| reserve_failure("caller entries", error))?;
    for (binding, name) in request.bindings().iter().zip(names) {
        let location = frontend_direct_eval_location(binding.location())?;
        if binding.policy().kind() == CompilerBindingKind::ClassPrivateName {
            let index = private_names.len();
            private_names.push(FrontendDirectEvalPrivateName::new(name, location));
            order.push(FrontendDirectEvalCallerEntry::PrivateName(index));
        } else {
            let (kind, is_lexical, is_const) = frontend_direct_eval_policy(binding.policy())?;
            let index = bindings.len();
            bindings.push(
                FrontendDirectEvalBinding::new(name, kind, is_lexical, is_const, location)
                    .with_scope(frontend_direct_eval_scope(binding.scope())),
            );
            order.push(FrontendDirectEvalCallerEntry::Binding(index));
        }
    }
    Ok(FrontendDirectEvalEntries {
        bindings,
        private_names,
        order,
    })
}

fn compile_direct_eval_source(
    request: &DirectEvalCompileRequest,
    limits: DynamicFunctionLimits,
) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
    let source = js_eval_source_to_utf8(request.source(), RuntimeSourceFragment::DirectEval)?;
    let mut binding_names = Vec::new();
    binding_names
        .try_reserve_exact(request.bindings().len())
        .map_err(|error| {
            engine_failure_with_source(
                DynamicFunctionEngineStage::SourceConversion,
                format!(
                    "could not reserve {} direct-eval binding names",
                    request.bindings().len()
                ),
                error,
            )
        })?;
    for binding in request.bindings() {
        binding_names.push(js_string_to_utf8(
            binding.name(),
            RuntimeSourceFragment::DirectEval,
        )?);
    }
    let entries = frontend_direct_eval_entries(request, &binding_names)?;
    // One data-only frame per caller entry preserves the runtime snapshot's
    // exact external-environment index while keeping ordinary bindings and
    // PrivateEnvironment names in distinct typed collections.
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(entries.order.len())
        .map_err(|error| {
            engine_failure_with_source(
                DynamicFunctionEngineStage::SourceConversion,
                format!(
                    "could not reserve {} direct-eval scope frames",
                    entries.order.len()
                ),
                error,
            )
        })?;
    for &entry in &entries.order {
        match entry {
            FrontendDirectEvalCallerEntry::Binding(index) => {
                frames.push(FrontendDirectEvalScopeFrame::new(
                    FrontendDirectEvalScopeKind::Pseudo,
                    std::slice::from_ref(&entries.bindings[index]),
                    &[],
                ));
            }
            FrontendDirectEvalCallerEntry::PrivateName(index) => {
                frames.push(FrontendDirectEvalScopeFrame::new(
                    FrontendDirectEvalScopeKind::Class,
                    &[],
                    std::slice::from_ref(&entries.private_names[index]),
                ));
            }
        }
    }
    let capabilities = FrontendDirectEvalCapabilities::new()
        .with_strict(request.is_strict())
        .with_new_target(request.allows_new_target())
        .with_super_property(request.allows_super_property())
        .with_super_call(request.allows_super_call())
        .with_instance_elements(request.has_instance_elements())
        .with_arguments_allowed(request.allows_arguments());
    let context = DirectEvalContext::new(capabilities, DirectEvalScopeSnapshot::new(&frames))
        .with_variable_environment(frontend_direct_eval_variable_environment(
            request.variable_environment(),
        ));
    let compiled = with_parsed_program(
        &source.text,
        FrontendOptions::for_goal(CompilationGoal::DirectEval(context))
            .with_limits(limits.frontend),
        |unit| {
            let compiler = CompilationContext::new_with_source_name_and_substitutions(
                unit,
                Arc::from("<eval>"),
                Arc::clone(&source.substitutions),
            )
            .map_err(DynamicFunctionCompilerError::Planning)?;
            compiler
                .compile_direct_eval_script_with_all_limits(
                    limits.bytecode,
                    limits.function_graph,
                    limits.final_graph,
                )
                .map_err(DynamicFunctionCompilerError::Lowering)
        },
    )
    .map_err(map_eval_frontend_error)?
    .map_err(map_direct_eval_compiler_error)?;
    Ok(Arc::new(compiled.verified_bytecode().clone()))
}

fn map_direct_eval_compiler_error(
    source: DynamicFunctionCompilerError,
) -> DynamicFunctionCompileFailure {
    if let DynamicFunctionCompilerError::Lowering(LeafCompilationError::EvalDeclarationConflict {
        name,
        ..
    }) = &source
    {
        let message = format!("Identifier '{name}' has already been declared");
        return match JsString::from_utf8(&message) {
            Ok(message) => DynamicFunctionCompileFailure::Syntax { message },
            Err(error) => engine_failure_with_source(
                DynamicFunctionEngineStage::SyntaxMessageConversion,
                "could not retain the eval declaration-conflict diagnostic as a JavaScript string",
                error,
            ),
        };
    }
    let stage = match &source {
        DynamicFunctionCompilerError::Planning(_) => DynamicFunctionEngineStage::CompilerPlanning,
        DynamicFunctionCompilerError::Lowering(_) => DynamicFunctionEngineStage::CompilerLowering,
    };
    engine_failure_with_source(stage, source.to_string(), source)
}

const fn frontend_direct_eval_scope(
    scope: DirectEvalCallerBindingScope,
) -> FrontendDirectEvalBindingScope {
    match scope {
        DirectEvalCallerBindingScope::Lexical => FrontendDirectEvalBindingScope::Lexical,
        DirectEvalCallerBindingScope::Variable => FrontendDirectEvalBindingScope::Variable,
        DirectEvalCallerBindingScope::Outer => FrontendDirectEvalBindingScope::Outer,
    }
}

const fn frontend_direct_eval_variable_environment(
    environment: DirectEvalVariableEnvironment,
) -> FrontendDirectEvalVariableEnvironment {
    match environment {
        DirectEvalVariableEnvironment::Global => FrontendDirectEvalVariableEnvironment::Global,
        DirectEvalVariableEnvironment::Function => FrontendDirectEvalVariableEnvironment::Function,
        DirectEvalVariableEnvironment::FunctionParameterInitializer => {
            FrontendDirectEvalVariableEnvironment::FunctionParameterInitializer
        }
    }
}

fn frontend_direct_eval_location(
    location: DirectEvalCallerBindingLocation,
) -> Result<FrontendDirectEvalBindingLocation, DynamicFunctionCompileFailure> {
    let checked = |domain, index| {
        u16::try_from(index).map_err(|_| {
            engine_failure(
                DynamicFunctionEngineStage::SourceConversion,
                format!("direct-eval caller {domain} index is not representable"),
            )
        })
    };
    Ok(match location {
        DirectEvalCallerBindingLocation::Argument(index) => {
            FrontendDirectEvalBindingLocation::Argument {
                index: checked("argument", index)?,
            }
        }
        DirectEvalCallerBindingLocation::Local(index) => FrontendDirectEvalBindingLocation::Local {
            index: checked("local", index)?,
        },
        DirectEvalCallerBindingLocation::Closure(index) => {
            FrontendDirectEvalBindingLocation::Closure {
                index: checked("closure", index)?,
            }
        }
        DirectEvalCallerBindingLocation::EvalVariable { depth, index } => {
            FrontendDirectEvalBindingLocation::EvalVariable {
                depth: checked("eval-variable environment", depth)?,
                index: checked("eval-variable binding", index)?,
            }
        }
    })
}

fn frontend_direct_eval_policy(
    policy: CompilerBindingPolicy,
) -> Result<(FrontendDirectEvalBindingKind, bool, bool), DynamicFunctionCompileFailure> {
    let binding = match policy.kind() {
        CompilerBindingKind::Parameter | CompilerBindingKind::Var => {
            (FrontendDirectEvalBindingKind::Normal, false, false)
        }
        CompilerBindingKind::Let => (FrontendDirectEvalBindingKind::Normal, true, false),
        CompilerBindingKind::Const | CompilerBindingKind::ClassName => {
            (FrontendDirectEvalBindingKind::Normal, true, true)
        }
        CompilerBindingKind::Function => (
            if policy.initialization() == CompilerInitializationPolicy::FunctionAtScopeEntry {
                FrontendDirectEvalBindingKind::NewFunctionDeclaration
            } else {
                FrontendDirectEvalBindingKind::FunctionDeclaration
            },
            policy.initialization() == CompilerInitializationPolicy::FunctionAtScopeEntry,
            false,
        ),
        CompilerBindingKind::FunctionName => (
            FrontendDirectEvalBindingKind::FunctionName,
            false,
            policy.writes() == CompilerWritePolicy::Immutable,
        ),
        CompilerBindingKind::Catch => (FrontendDirectEvalBindingKind::Catch, true, false),
        CompilerBindingKind::WithObject => (FrontendDirectEvalBindingKind::WithObject, true, true),
        CompilerBindingKind::ClassFieldKey
        | CompilerBindingKind::ClassInstanceInitializer
        | CompilerBindingKind::ClassPrivateName
        | CompilerBindingKind::ClassStaticReceiver
        | CompilerBindingKind::GlobalReference => {
            return Err(engine_failure(
                DynamicFunctionEngineStage::SourceConversion,
                "compiler-internal and Realm-global bindings must not enter a direct-eval caller snapshot",
            ));
        }
    };
    Ok(binding)
}

fn map_eval_frontend_error(source: FrontendError) -> DynamicFunctionCompileFailure {
    if matches!(
        source.stage(),
        DiagnosticStage::Parser | DiagnosticStage::Profile | DiagnosticStage::Semantic
    ) {
        let Some(message) = source
            .diagnostics()
            .first()
            .map(|diagnostic| diagnostic.message.as_str())
        else {
            return engine_failure_with_source(
                DynamicFunctionEngineStage::Frontend(source.stage()),
                "front end rejected eval source without a normalized diagnostic",
                source,
            );
        };
        return match JsString::from_utf8(message) {
            Ok(message) => DynamicFunctionCompileFailure::Syntax { message },
            Err(error) => engine_failure_with_source(
                DynamicFunctionEngineStage::SyntaxMessageConversion,
                "could not retain the normalized eval syntax diagnostic as a JavaScript string",
                error,
            ),
        };
    }
    let stage = DynamicFunctionEngineStage::Frontend(source.stage());
    let detail = source.diagnostics().first().map_or_else(
        || source.to_string(),
        |diagnostic| diagnostic.message.clone(),
    );
    engine_failure_with_source(stage, detail, source)
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
        DynamicFunctionKind::Function
            | DynamicFunctionKind::GeneratorFunction
            | DynamicFunctionKind::AsyncFunction
            | DynamicFunctionKind::AsyncGeneratorFunction
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
                DynamicFunctionKind::AsyncFunction => Arc::from("<dynamic AsyncFunction>"),
                DynamicFunctionKind::AsyncGeneratorFunction => {
                    Arc::from("<dynamic AsyncGeneratorFunction>")
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

struct Utf8RuntimeSource {
    text: String,
    substitutions: Arc<[SourceTextSubstitution]>,
}

fn js_string_to_utf8(
    source: &JsString,
    fragment: RuntimeSourceFragment,
) -> Result<String, DynamicFunctionCompileFailure> {
    let encoded = encode_runtime_source(source, fragment, false)?;
    debug_assert!(encoded.substitutions.is_empty());
    Ok(encoded.text)
}

fn js_eval_source_to_utf8(
    source: &JsString,
    fragment: RuntimeSourceFragment,
) -> Result<Utf8RuntimeSource, DynamicFunctionCompileFailure> {
    encode_runtime_source(source, fragment, true)
}

fn encode_runtime_source(
    source: &JsString,
    fragment: RuntimeSourceFragment,
    preserve_lone_surrogates: bool,
) -> Result<Utf8RuntimeSource, DynamicFunctionCompileFailure> {
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

    let mut substitutions = Vec::new();
    let mut offset = 0_u32;
    let mut units = source.code_units().peekable();
    while let Some(unit) = units.next() {
        let paired_low = (0xd800..=0xdbff)
            .contains(&unit)
            .then(|| {
                units
                    .peek()
                    .copied()
                    .filter(|low| (0xdc00..=0xdfff).contains(low))
            })
            .flatten();
        let (scalar, width) = if let Some(low) = paired_low {
            let _ = units.next();
            (
                0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
                2,
            )
        } else if (0xd800..=0xdfff).contains(&unit) {
            if !preserve_lone_surrogates {
                return Err(lone_surrogate(fragment, offset, unit));
            }
            substitutions.try_reserve(1).map_err(|error| {
                engine_failure_with_source(
                    DynamicFunctionEngineStage::SourceConversion,
                    format!("could not reserve a UTF-16 substitution for {fragment}"),
                    error,
                )
            })?;
            let start = u32::try_from(text.len()).map_err(|error| {
                engine_failure_with_source(
                    DynamicFunctionEngineStage::SourceConversion,
                    format!("{fragment} parser byte offset does not fit u32"),
                    error,
                )
            })?;
            // A noncharacter is a lexically inert scalar placeholder. Exact
            // source position, not scalar identity, selects the UTF-16 value
            // restored by the compiler.
            text.push('\u{fdd0}');
            let end = u32::try_from(text.len()).map_err(|error| {
                engine_failure_with_source(
                    DynamicFunctionEngineStage::SourceConversion,
                    format!("{fragment} parser byte offset does not fit u32"),
                    error,
                )
            })?;
            substitutions.push(SourceTextSubstitution::new(
                Span::new(start, end),
                Arc::from([unit]),
            ));
            offset = offset.saturating_add(1);
            continue;
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
    Ok(Utf8RuntimeSource {
        text,
        substitutions: Arc::from(substitutions),
    })
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
