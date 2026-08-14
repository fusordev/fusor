//! Overlay assembly (subproject 6, §9): the [`Overlay`] trait and the
//! `HostRuntime::builder()` assembly pipeline (Deno-`Extension` style).
//!
//! Assembly runs these steps, in order:
//!
//! 1. topological sort of the overlays by their declared dependencies,
//!    with cycle detection — a build-time error (no runtime tolerance,
//!    alpha semantics)
//! 2. the host core installation (the `Fusor` namespace, §5.4, and the
//!    process ops, §7.1), then op registration through the
//!    assembly [`OpRegistry`] and installation as `Fusor.ops.<name>`
//! 3. per-overlay init module graphs evaluated in dependency order; an
//!    init module may `import` any other overlay's init modules through
//!    their embedded virtual specifiers
//!
//! The assembled state is the snapshot input (§8): assembly plus init
//! evaluation serialize into a blob; loading the blob skips steps 1–3
//! (`HostRuntime::from_snapshot` lands with the snapshot slices).

mod core;

pub use core::CoreOverlay;

use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use fusor_bytecode::{
    BytecodeGraphVerificationLimits, FunctionGraphVerificationLimits, VerificationLimits,
    VerifiedBytecode,
};
use fusor_compiler::{CompilationContext, CompilerError, LeafCompilationError};
use fusor_frontend::{
    CompilationGoal, FrontendError, FrontendOptions, ModuleSyntaxRecord, StaticModuleRequest,
    with_parsed_program,
};
use fusor_runtime::{
    Context, ExecutionError, ExecutionLimits, ModuleError, ModuleKey, Realm, Runtime, RuntimeError,
    RuntimeLimits,
};

use crate::ops::{OpDeclarationConflict, OpRegistry, install_namespace, install_op, install_process};
use crate::r#loop::{HostLoop, HostLoopError};

/// One embedded ESM source contributed by an overlay's init phase (§9).
#[derive(Clone, Debug)]
pub struct OverlaySource {
    /// The virtual module specifier (for example `fusor:core/init.js`),
    /// resolved by init modules' `import` requests.
    pub specifier: String,
    /// The module source text (Module goal).
    pub text: &'static str,
}

/// A host feature assembled by the builder (§9).
///
/// Implementations are stateless feature declarations: ops to register,
/// embedded init sources to evaluate, and the ordering constraints between
/// overlays. The builder owns all mutable assembly state.
pub trait Overlay: 'static {
    /// The overlay's unique name, referenced by other overlays'
    /// [`dependencies`](Self::dependencies).
    fn name(&self) -> &'static str;

    /// Registers the overlay's ops into the assembly registry (§5.4).
    ///
    /// The builder checks the registry for same-name conflicts after every
    /// overlay registers and fails the build on the first conflict (fail
    /// closed).
    fn ops(&self, registry: &mut OpRegistry);

    /// The overlay's embedded init ESM sources.
    fn init_sources(&self) -> Vec<OverlaySource>;

    /// The entry module specifier whose graph the builder evaluates during
    /// assembly; the empty string means no init module.
    fn entry(&self) -> &'static str;

    /// The names of the overlays this overlay depends on, establishing the
    /// assembly evaluation order.
    fn dependencies(&self) -> &'static [&'static str];
}

/// An assembled host runtime: the engine [`Runtime`], one realm with the
/// host core installed, the overlays' ops installed as `Fusor.ops`, and
/// the overlays' init module graphs evaluated (§9).
///
/// Convert the assembled runtime into the event loop with
/// [`Self::into_loop`], or evaluate scripts directly through
/// [`Self::context`].
pub struct HostRuntime {
    runtime: Runtime,
    realm: Realm,
}

impl fmt::Debug for HostRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostRuntime")
            .field("realm", &self.realm)
            .finish_non_exhaustive()
    }
}

impl HostRuntime {
    /// Creates the assembly builder with no overlays (§9).
    #[must_use]
    pub fn builder() -> HostRuntimeBuilder {
        HostRuntimeBuilder::new()
    }

    /// Returns the context for this runtime's realm.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] when the realm disappeared.
    pub fn context(&mut self) -> Result<Context<'_>, ExecutionError> {
        Ok(self.runtime.context(&self.realm)?)
    }

    /// Wraps the assembled runtime into the host event loop (§6).
    ///
    /// # Errors
    ///
    /// Returns [`HostLoopError::AlreadyInstalled`] when another loop owns
    /// this thread.
    pub fn into_loop(self) -> Result<HostLoop, HostLoopError> {
        HostLoop::new(self.runtime, self.realm)
    }
}

/// The overlay assembly builder (§9):
/// `HostRuntime::builder().with_overlay(one).with_overlay(two).build()`.
#[derive(Default)]
pub struct HostRuntimeBuilder {
    overlays: Vec<Box<dyn Overlay>>,
}

impl fmt::Debug for HostRuntimeBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostRuntimeBuilder")
            .field("overlays", &self.overlays.len())
            .finish()
    }
}

impl HostRuntimeBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one overlay to the assembly.
    pub fn with_overlay(&mut self, overlay: impl Overlay) -> &mut Self {
        self.overlays.push(Box::new(overlay));
        self
    }

    /// Assembles the host runtime (§9): sorts the overlays, installs the
    /// host core and the registered ops, and evaluates the init module
    /// graphs in dependency order.
    ///
    /// # Errors
    ///
    /// Fails closed on any assembly defect — dependency cycles, unknown
    /// dependencies, duplicate overlay names or init sources, op-name
    /// conflicts, and init module compile/link/evaluate failures — leaving
    /// no half-assembled runtime behind.
    pub fn build(&mut self) -> Result<HostRuntime, HostBuildError> {
        let order = sort_overlays(&self.overlays)?;
        let mut runtime =
            Runtime::try_new(RuntimeLimits::default()).map_err(HostBuildError::Runtime)?;
        let realm = runtime.create_realm().map_err(HostBuildError::Runtime)?;
        {
            let mut context = runtime
                .context(&realm)
                .map_err(|error| HostBuildError::Execution(error.into()))?;
            // Host core: the Fusor namespace (§5.4) and the process ops
            // (§7.1) are fixed parts of every host assembly, independent
            // of the overlays.
            install_namespace(&mut context).map_err(HostBuildError::Execution)?;
            install_process(&mut context).map_err(HostBuildError::Execution)?;
            // Overlay ops (§9 step 2): register in dependency order, fail
            // closed on the first same-name conflict, then install.
            let mut registry = OpRegistry::new();
            for &index in &order {
                self.overlays[index].ops(&mut registry);
                if let Some(conflict) = registry.take_conflict() {
                    return Err(HostBuildError::OpConflict(conflict));
                }
            }
            for (declaration, glue) in registry.registrations() {
                install_op(&mut context, declaration, glue).map_err(HostBuildError::Execution)?;
            }
            // Init ESM (§9 step 3): one graph per overlay, in dependency
            // order; init modules may import any overlay's sources.
            let sources = collect_init_sources(&order, &self.overlays)?;
            for &index in &order {
                let overlay = &self.overlays[index];
                let entry = overlay.entry();
                if entry.is_empty() {
                    continue;
                }
                evaluate_init_graph(&mut context, overlay.name(), &sources, entry).map_err(
                    |error| HostBuildError::InitModule {
                        overlay: overlay.name(),
                        error,
                    },
                )?;
            }
        }
        Ok(HostRuntime { runtime, realm })
    }
}

/// Overlay assembly failures (§9: all defects are build-time errors; alpha
/// semantics allow no runtime tolerance).
#[derive(Debug)]
pub enum HostBuildError {
    /// The engine runtime or realm could not be created.
    Runtime(RuntimeError),
    /// The host core (namespace, process object) or an op could not be
    /// installed.
    Execution(ExecutionError),
    /// Two overlays share one name.
    DuplicateOverlay {
        /// The duplicated name.
        name: &'static str,
    },
    /// An overlay depends on an overlay that is not registered.
    UnknownDependency {
        /// The depending overlay.
        overlay: &'static str,
        /// The missing dependency.
        dependency: &'static str,
    },
    /// The overlay dependency graph contains a cycle.
    DependencyCycle {
        /// The overlays that form the cycle, in registration order.
        cycle: Vec<&'static str>,
    },
    /// Two overlays registered the same op name (§5.4).
    OpConflict(OpDeclarationConflict),
    /// Two overlays provided the same init source specifier.
    DuplicateInitSource {
        /// The duplicated virtual specifier.
        specifier: String,
    },
    /// An overlay's init module graph failed to compile, link, or evaluate.
    InitModule {
        /// The owning overlay.
        overlay: &'static str,
        /// The init failure.
        error: InitModuleError,
    },
}

impl fmt::Display for HostBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::Execution(error) => error.fmt(formatter),
            Self::DuplicateOverlay { name } => {
                write!(formatter, "overlay '{name}' is registered more than once")
            }
            Self::UnknownDependency {
                overlay,
                dependency,
            } => write!(
                formatter,
                "overlay '{overlay}' depends on unknown overlay '{dependency}'"
            ),
            Self::DependencyCycle { cycle } => {
                write!(formatter, "overlay dependency cycle: {}", cycle.join(" -> "))
            }
            Self::OpConflict(conflict) => conflict.fmt(formatter),
            Self::DuplicateInitSource { specifier } => write!(
                formatter,
                "init source '{specifier}' is provided by more than one overlay"
            ),
            Self::InitModule { overlay, error } => {
                write!(formatter, "overlay '{overlay}' init failed: {error}")
            }
        }
    }
}

impl Error for HostBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Execution(error) => Some(error),
            Self::OpConflict(conflict) => Some(conflict),
            Self::InitModule { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// One init module graph's failure during assembly (§9 step 3).
#[derive(Debug)]
pub enum InitModuleError {
    /// The init source failed to parse (frontend early errors).
    Frontend(FrontendError),
    /// Storage planning failed for the init source.
    Planning(CompilerError),
    /// Lowering or verification failed for the init source.
    Lowering(LeafCompilationError),
    /// An `import` request resolved to no embedded source (fail closed).
    Unresolved {
        /// The importing module's specifier.
        referrer: String,
        /// The unresolvable request.
        specifier: String,
    },
    /// The overlay's entry specifier is not among its init sources.
    EntryNotProvided {
        /// The owning overlay.
        overlay: &'static str,
        /// The missing entry.
        entry: &'static str,
    },
    /// Module registration, linking, or evaluation failed.
    Module(ModuleError),
}

impl From<ModuleError> for InitModuleError {
    fn from(error: ModuleError) -> Self {
        Self::Module(error)
    }
}

impl fmt::Display for InitModuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(error) => error.fmt(formatter),
            Self::Planning(error) => error.fmt(formatter),
            Self::Lowering(error) => error.fmt(formatter),
            Self::Unresolved {
                referrer,
                specifier,
            } => write!(
                formatter,
                "module '{referrer}' imports '{specifier}', which no overlay provides"
            ),
            Self::EntryNotProvided { overlay, entry } => write!(
                formatter,
                "overlay '{overlay}' declares init entry '{entry}' but provides no source for it"
            ),
            Self::Module(error) => error.fmt(formatter),
        }
    }
}

impl Error for InitModuleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frontend(error) => Some(error),
            Self::Planning(error) => Some(error),
            Self::Lowering(error) => Some(error),
            Self::Module(error) => Some(error),
            _ => None,
        }
    }
}

/// Sorts the overlays by their declared dependencies (Kahn's algorithm,
/// seeded in registration order so the result is deterministic), rejecting
/// duplicate names, unknown dependencies, and cycles.
fn sort_overlays(overlays: &[Box<dyn Overlay>]) -> Result<Vec<usize>, HostBuildError> {
    let mut positions: HashMap<&'static str, usize> = HashMap::new();
    for (index, overlay) in overlays.iter().enumerate() {
        let name = overlay.name();
        if positions.insert(name, index).is_some() {
            return Err(HostBuildError::DuplicateOverlay { name });
        }
    }
    let mut indegree = vec![0usize; overlays.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); overlays.len()];
    for (index, overlay) in overlays.iter().enumerate() {
        for &dependency in overlay.dependencies() {
            let Some(&dependency_index) = positions.get(dependency) else {
                return Err(HostBuildError::UnknownDependency {
                    overlay: overlay.name(),
                    dependency,
                });
            };
            indegree[index] += 1;
            dependents[dependency_index].push(index);
        }
    }
    let mut order = Vec::with_capacity(overlays.len());
    let mut ready: VecDeque<usize> = (0..overlays.len())
        .filter(|&index| indegree[index] == 0)
        .collect();
    while let Some(index) = ready.pop_front() {
        order.push(index);
        for &dependent in &dependents[index] {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }
    if order.len() != overlays.len() {
        let cycle = (0..overlays.len())
            .filter(|&index| indegree[index] > 0)
            .map(|index| overlays[index].name())
            .collect();
        return Err(HostBuildError::DependencyCycle { cycle });
    }
    Ok(order)
}

/// Collects every overlay's init sources into one specifier-keyed map in
/// dependency order, rejecting duplicate specifiers (fail closed).
fn collect_init_sources(
    order: &[usize],
    overlays: &[Box<dyn Overlay>],
) -> Result<HashMap<String, &'static str>, HostBuildError> {
    let mut sources = HashMap::new();
    for &index in order {
        for source in overlays[index].init_sources() {
            if sources.insert(source.specifier.clone(), source.text).is_some() {
                return Err(HostBuildError::DuplicateInitSource {
                    specifier: source.specifier,
                });
            }
        }
    }
    Ok(sources)
}

/// One compiled init module, ready for registration.
struct CompiledInitModule {
    syntax: ModuleSyntaxRecord,
    authority: Arc<VerifiedBytecode>,
}

/// The closure-crossable compile failure: `with_parsed_program` requires
/// the callback's result to be `Send`, which the full [`InitModuleError`]
/// is not (frontend errors carry arena references).
enum CompileFailure {
    Planning(CompilerError),
    Lowering(LeafCompilationError),
}

/// Compiles one init source (Module goal) against the default verification
/// limits.
fn compile_init_module(text: &str, specifier: &str) -> Result<CompiledInitModule, InitModuleError> {
    let source_name: Arc<str> = Arc::from(specifier);
    let (syntax, tree) = with_parsed_program(
        text,
        FrontendOptions::for_goal(CompilationGoal::Module),
        move |unit| {
            let syntax = unit.module_syntax().clone();
            let compiler = CompilationContext::new_with_source_name(unit, source_name)
                .map_err(CompileFailure::Planning)?;
            let tree = compiler
                .compile_module_with_all_limits(
                    VerificationLimits::default(),
                    FunctionGraphVerificationLimits::default(),
                    BytecodeGraphVerificationLimits::default(),
                )
                .map_err(CompileFailure::Lowering)?;
            Ok((syntax, tree))
        },
    )
    .map_err(InitModuleError::Frontend)?
    .map_err(|failure| match failure {
        CompileFailure::Planning(error) => InitModuleError::Planning(error),
        CompileFailure::Lowering(error) => InitModuleError::Lowering(error),
    })?;
    Ok(CompiledInitModule {
        syntax,
        authority: Arc::new(tree.verified_bytecode().clone()),
    })
}

/// Decodes a static module request's specifier to a UTF-8 `String`.
fn decode_request_specifier(request: &StaticModuleRequest) -> String {
    request
        .specifier()
        .code_units()
        .iter()
        .copied()
        .map(u32::from)
        .filter_map(char::from_u32)
        .collect()
}

/// Registers, links, and evaluates one overlay's init module graph,
/// resolving every request against the embedded sources (§9 step 3).
///
/// Modules registered by an earlier overlay's graph are reused (ECMA-262
/// [[Evaluation]] runs at most once per realm); the virtual module key is
/// the specifier text itself.
fn evaluate_init_graph(
    context: &mut Context<'_>,
    overlay_name: &'static str,
    sources: &HashMap<String, &'static str>,
    entry: &'static str,
) -> Result<(), InitModuleError> {
    let Some(&entry_text) = sources.get(entry) else {
        return Err(InitModuleError::EntryNotProvided {
            overlay: overlay_name,
            entry,
        });
    };
    let entry_key = ModuleKey::new(Arc::from(entry));
    let mut queue: Vec<(ModuleKey, ModuleSyntaxRecord)> = Vec::new();
    if !context.has_module(&entry_key) {
        let compiled = compile_init_module(entry_text, entry)?;
        context.register_module(entry_key.clone(), compiled.syntax.clone(), compiled.authority)?;
        queue.push((entry_key.clone(), compiled.syntax));
    }
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(entry.to_owned());
    while let Some((referrer, syntax)) = queue.pop() {
        for request in syntax.requests() {
            let specifier = decode_request_specifier(request);
            let Some(&dependency_text) = sources.get(&specifier) else {
                return Err(InitModuleError::Unresolved {
                    referrer: referrer.as_str().to_owned(),
                    specifier,
                });
            };
            let dependency_key = ModuleKey::new(Arc::from(specifier.as_str()));
            if seen.insert(specifier.clone()) && !context.has_module(&dependency_key) {
                let compiled = compile_init_module(dependency_text, &specifier)?;
                context.register_module(
                    dependency_key.clone(),
                    compiled.syntax.clone(),
                    compiled.authority,
                )?;
                queue.push((dependency_key.clone(), compiled.syntax));
            }
            context.register_module_dependency(&referrer, &specifier, &dependency_key)?;
        }
    }
    context.link_module(&entry_key)?;
    context.evaluate_module(&entry_key, ExecutionLimits::default())?;
    Ok(())
}
