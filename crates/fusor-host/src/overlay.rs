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
//! 3. per-overlay init scripts (Global Scripts, §8.4 — no ESM) evaluated
//!    in dependency order; later scripts read earlier overlays' effects
//!    through `globalThis`
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

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::{CompilationContext, CompilerError, LeafCompilationError};
use fusor_frontend::{CompilationGoal, FrontendError, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{
    Context, ExecutionError, ExecutionLimits, GlobalScriptError, Realm, Runtime, RuntimeError,
    RuntimeLimits,
};

use crate::ops::{OpDeclarationConflict, OpRegistry, install_namespace, install_op, install_process};
use crate::r#loop::{HostLoop, HostLoopError};

/// One embedded init script contributed by an overlay's init phase (§9,
/// §8.4): a Global Script — no ESM. The specifier names the source
/// location reported by diagnostics and stack frames.
#[derive(Clone, Debug)]
pub struct OverlaySource {
    /// The virtual source name (for example `fusor:core/init.js`),
    /// reported as the script's location by diagnostics and stack
    /// frames.
    pub specifier: String,
    /// The script source text (Global Script goal).
    pub text: &'static str,
}

/// A host feature assembled by the builder (§9).
///
/// Implementations are stateless feature declarations: ops to register,
/// embedded init scripts to evaluate, and the ordering constraints between
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

    /// The overlay's embedded init scripts (§8.4): Global Scripts
    /// evaluated in declaration order when the overlay is assembled.
    fn init_sources(&self) -> Vec<OverlaySource>;

    /// The names of the overlays this overlay depends on, establishing the
    /// assembly evaluation order.
    fn dependencies(&self) -> &'static [&'static str];
}

/// An assembled host runtime: the engine [`Runtime`], one realm with the
/// host core installed, the overlays' ops installed as `Fusor.ops`, and
/// the overlays' init scripts evaluated (§9, §8.4).
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

    /// Returns mutable access to the engine [`Runtime`], for hosts that
    /// need engine-level services this wrapper does not expose (for
    /// example installing a debugger hook before evaluation starts).
    ///
    /// The borrow lasts as long as the returned reference; no JavaScript
    /// runs while this wrapper is borrowed.
    pub fn runtime_mut(&mut self) -> &mut Runtime {
        &mut self.runtime
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
    /// host core and the registered ops, and evaluates the init scripts
    /// in dependency order (§8.4).
    ///
    /// # Errors
    ///
    /// Fails closed on any assembly defect — dependency cycles, unknown
    /// dependencies, duplicate overlay names or init sources, op-name
    /// conflicts, and init script compile/evaluate failures — leaving
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
            // Init scripts (§9 step 3, §8.4): every overlay's sources
            // evaluate in dependency order — no imports; later scripts
            // read earlier overlays' effects through `globalThis`.
            collect_init_sources(&order, &self.overlays)?;
            for &index in &order {
                let overlay = &self.overlays[index];
                evaluate_init_scripts(&mut context, &overlay.init_sources()).map_err(
                    |error| HostBuildError::InitScript {
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
    /// An overlay's init scripts failed to compile or evaluate.
    InitScript {
        /// The owning overlay.
        overlay: &'static str,
        /// The init failure.
        error: InitScriptError,
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
            Self::InitScript { overlay, error } => {
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
            Self::InitScript { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// One overlay's init-script failure during assembly (§9 step 3).
#[derive(Debug)]
pub enum InitScriptError {
    /// An init source failed to parse (frontend early errors).
    Frontend(FrontendError),
    /// Storage planning failed for an init source.
    Planning(CompilerError),
    /// Lowering or verification failed for an init source.
    Lowering(LeafCompilationError),
    /// The init script's execution failed (installation or a JavaScript
    /// exception).
    Execution(GlobalScriptError),
}

impl fmt::Display for InitScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(error) => error.fmt(formatter),
            Self::Planning(error) => error.fmt(formatter),
            Self::Lowering(error) => error.fmt(formatter),
            Self::Execution(error) => error.fmt(formatter),
        }
    }
}

impl Error for InitScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frontend(error) => Some(error),
            Self::Planning(error) => Some(error),
            Self::Lowering(error) => Some(error),
            Self::Execution(error) => Some(error),
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

/// The closure-crossable compile failure: `with_parsed_program` requires
/// the callback's result to be `Send`, which the full [`InitScriptError`]
/// is not (frontend errors carry arena references).
enum CompileFailure {
    Planning(CompilerError),
    Lowering(LeafCompilationError),
}

/// Compiles one init source (Global Script goal, §8.4) against the
/// default verification limits, with the specifier as the source name so
/// diagnostics and stack frames report the virtual location.
fn compile_init_script(text: &str, specifier: &str) -> Result<Arc<VerifiedBytecode>, InitScriptError> {
    let source_name: Arc<str> = Arc::from(specifier);
    with_parsed_program(
        text,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        move |unit| {
            let compiler = CompilationContext::new_with_source_name(unit, source_name)
                .map_err(CompileFailure::Planning)?;
            compiler
                .compile_global_script(VerificationLimits::default())
                .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                .map_err(CompileFailure::Lowering)
        },
    )
    .map_err(InitScriptError::Frontend)?
    .map_err(|failure| match failure {
        CompileFailure::Planning(error) => InitScriptError::Planning(error),
        CompileFailure::Lowering(error) => InitScriptError::Lowering(error),
    })
}

/// Evaluates one overlay's init scripts in declaration order (§9 step 3,
/// §8.4): each source compiles as a Global Script and executes against
/// the shared realm — later scripts read earlier effects through
/// `globalThis`.
fn evaluate_init_scripts(
    context: &mut Context<'_>,
    sources: &[OverlaySource],
) -> Result<(), InitScriptError> {
    for source in sources {
        let authority = compile_init_script(source.text, &source.specifier)?;
        context
            .execute_global_script(authority, ExecutionLimits::default())
            .map_err(InitScriptError::Execution)?;
    }
    Ok(())
}
