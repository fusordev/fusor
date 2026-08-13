//! Module evaluation: InnerModuleEvaluation with cycle roots and async status.
//!
//! Iterative explicit-stack DFS implementing ECMA-262 16.6.1.4
//! (InnerModuleEvaluation): DFS indices, `[[CycleRoot]]` assignment at the
//! strongly-connected-component pop, `[[EvaluationError]]` propagation to every
//! module still on the DFS stack, and the Top-Level Await fields
//! ([[PendingAsyncDependencies]], [[AsyncEvaluationOrder]],
//! [[AsyncParentModules]], [[TopLevelCapability]]).

use super::{ModuleError, ModuleRecordId, ModuleStatus};
use crate::OrdinaryDynamicFunctionCompiler;
use crate::runtime::{ExecutionLimits, RealmId, Runtime, StoredValue};
use quickjs_bytecode::FunctionKind;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

/// An evaluation failure retaining the error phase.
#[derive(Debug)]
pub struct ModuleEvaluationError {
    pub(crate) error: ModuleError,
}

impl ModuleEvaluationError {
    pub(crate) fn new(error: ModuleError) -> Self {
        Self { error }
    }

    /// Returns the underlying module error.
    #[must_use]
    pub const fn error(&self) -> &ModuleError {
        &self.error
    }
}

impl fmt::Display for ModuleEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ModuleEvaluationError {}

enum EvalWorkItem {
    Enter(ModuleRecordId),
    /// Carries the evaluation list captured at enter time: by the time this
    /// module executes, gathered dependencies have left the `Linked` status
    /// and would be dropped by a recomputed list.
    Execute {
        module: ModuleRecordId,
        evaluation_list: Vec<ModuleRecordId>,
    },
}

/// Evaluates a module graph starting from `root` (ECMA-262 `Evaluate()`).
///
/// A synchronous root is `Evaluated` on success. A root whose evaluation is
/// asynchronous (its own top level awaits, or it depends on an async module)
/// is left `EvaluatingAsync` with a pending [[TopLevelCapability]]; `Ok` is
/// returned either way and the capability settles through the async-module
/// continuation (wired in a later step).
pub fn evaluate_module(
    runtime: &mut Runtime,
    realm: RealmId,
    root: ModuleRecordId,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<(), ModuleError> {
    match module_status(runtime, root) {
        ModuleStatus::Evaluated => {
            // ECMA-262 Evaluate step 1: an evaluated (or evaluating-async)
            // module redirects to its cycle root, whose recorded evaluation
            // error — if any — rethrows even though this member itself
            // finished cleanly.
            if let Some(error) = cycle_root_evaluation_error(runtime, root) {
                return Err(error);
            }
            return Ok(());
        }
        ModuleStatus::EvaluatingAsync => {
            if let Some(error) = cycle_root_evaluation_error(runtime, root) {
                return Err(error);
            }
            return ensure_top_level_capability(runtime, realm, root);
        }
        ModuleStatus::Errored => return Err(module_evaluation_error(runtime, root)),
        ModuleStatus::Linked => {}
        status => {
            return Err(ModuleError::evaluate(format!(
                "module is in {status:?} status, expected Linked"
            )));
        }
    }

    let mut work: Vec<EvalWorkItem> = vec![EvalWorkItem::Enter(root)];
    // The spec `stack` of InnerModuleEvaluation: modules whose evaluation has
    // started but whose strongly connected component has not completed.
    let mut stack: Vec<ModuleRecordId> = Vec::new();
    let mut index: u32 = 0;

    while let Some(item) = work.pop() {
        match item {
            EvalWorkItem::Enter(module) => {
                match module_status(runtime, module) {
                    ModuleStatus::Evaluated
                    | ModuleStatus::Evaluating
                    | ModuleStatus::EvaluatingAsync => continue,
                    ModuleStatus::Errored => {
                        let error = module_evaluation_error(runtime, module);
                        return fail_evaluation(runtime, root, realm, &stack, error);
                    }
                    ModuleStatus::Linked => {}
                    status => {
                        let error = ModuleError::evaluate(format!(
                            "module is in {status:?} status, expected Linked"
                        ));
                        return fail_evaluation(runtime, root, realm, &stack, error);
                    }
                }
                set_module_status(runtime, module, ModuleStatus::Evaluating);
                if let Some(record) = runtime.modules.get_mut(module) {
                    record.dfs_index = Some(index);
                    record.dfs_ancestor_index = Some(index);
                    record.pending_async_dependencies = 0;
                }
                index += 1;
                stack.push(module);
                // ECMA-262 InnerModuleEvaluation step 8 recurses over the
                // evaluation list: eager-phase requests contribute the
                // requested module, defer-phase requests contribute only the
                // module's asynchronous transitive dependencies.
                let evaluation_list = module_evaluation_list(runtime, module);
                work.push(EvalWorkItem::Execute {
                    module,
                    evaluation_list: evaluation_list.clone(),
                });
                for dep in evaluation_list.into_iter().rev() {
                    if module_status(runtime, dep) == ModuleStatus::Linked {
                        work.push(EvalWorkItem::Enter(dep));
                    }
                }
            }
            EvalWorkItem::Execute {
                module,
                evaluation_list,
            } => {
                // Dependency bookkeeping in source order (ECMA-262 16.6.1.4
                // step 11.c): the bookkeeping iterates the evaluation list —
                // defer-phase dependencies are excluded (their evaluation is
                // deferred; their failures surface only through the deferred
                // namespace access). A dependency still on the DFS stack
                // tightens this module's ancestor index; a completed
                // dependency contributes its cycle root. Either way, an async
                // dependency becomes a pending async dependency of this
                // module.
                for dep in evaluation_list {
                    let async_dependency = if module_status(runtime, dep)
                        == ModuleStatus::Evaluating
                    {
                        let dep_ancestor = runtime
                            .modules
                            .get(dep)
                            .and_then(|r| r.dfs_ancestor_index)
                            .unwrap_or(u32::MAX);
                        if let Some(record) = runtime.modules.get_mut(module) {
                            let ancestor =
                                record.dfs_ancestor_index.unwrap_or(u32::MAX).min(dep_ancestor);
                            record.dfs_ancestor_index = Some(ancestor);
                        }
                        dep
                    } else {
                        let cycle_root = runtime
                            .modules
                            .get(dep)
                            .and_then(|r| r.cycle_root)
                            .unwrap_or(dep);
                        if module_status(runtime, cycle_root) == ModuleStatus::Errored {
                            let error = module_evaluation_error(runtime, cycle_root);
                            return fail_evaluation(runtime, root, realm, &stack, error);
                        }
                        cycle_root
                    };
                    let dep_is_async = runtime
                        .modules
                        .get(async_dependency)
                        .is_some_and(|r| r.async_evaluation_order.is_some());
                    if dep_is_async {
                        if let Some(record) = runtime.modules.get_mut(module) {
                            record.pending_async_dependencies += 1;
                        }
                        if let Some(record) = runtime.modules.get_mut(async_dependency) {
                            record.async_parent_modules.push(module);
                        }
                    }
                }

                let pending = runtime
                    .modules
                    .get(module)
                    .map_or(0, |r| r.pending_async_dependencies);
                let result = if pending > 0 || module_has_tla(runtime, module) {
                    let order = next_module_async_evaluation_order(runtime);
                    if let Some(record) = runtime.modules.get_mut(module) {
                        record.async_evaluation_order = Some(order);
                    }
                    if pending == 0 {
                        execute_async_module(runtime, module, limits, compiler)
                    } else {
                        // Execution resumes when the last pending async
                        // dependency fulfills (wired in a later step).
                        Ok(())
                    }
                } else {
                    execute_module_body(runtime, module, limits, compiler)
                };
                if let Err(error) = result {
                    return fail_evaluation(runtime, root, realm, &stack, error);
                }

                // Strongly-connected-component pop: this module is the cycle
                // root, so every module above it on the DFS stack shares its
                // cycle root. Each popped module keeps its own
                // [[AsyncEvaluationOrder]] (ECMA-262 16.6.1.4 step 16.d):
                // members that executed synchronously become Evaluated.
                let (dfs, ancestor) = runtime
                    .modules
                    .get(module)
                    .map_or((None, None), |r| (r.dfs_index, r.dfs_ancestor_index));
                if dfs.is_some() && dfs == ancestor {
                    while let Some(popped) = stack.pop() {
                        if let Some(record) = runtime.modules.get_mut(popped) {
                            record.cycle_root = Some(module);
                            if record.status != ModuleStatus::Errored {
                                record.status = if record.async_evaluation_order.is_some() {
                                    ModuleStatus::EvaluatingAsync
                                } else {
                                    ModuleStatus::Evaluated
                                };
                            }
                        }
                        if popped == module {
                            break;
                        }
                    }
                }
            }
        }
    }

    match module_status(runtime, root) {
        ModuleStatus::Errored => Err(module_evaluation_error(runtime, root)),
        ModuleStatus::EvaluatingAsync => ensure_top_level_capability(runtime, realm, root),
        ModuleStatus::Evaluated => {
            // Resolve a top-level capability allocated by an earlier
            // `Evaluate()` call; the spec resolves it with ~undefined~.
            if let Some(capability) = runtime
                .modules
                .get(root)
                .and_then(|r| r.top_level_capability)
            {
                crate::vm::fulfill_promise_host(runtime, capability, StoredValue::Undefined)
                    .map_err(|error| {
                        ModuleError::evaluate(format!(
                            "top-level capability resolution failed: {error}"
                        ))
                    })?;
            }
            Ok(())
        }
        status => Err(ModuleError::evaluate(format!(
            "module evaluation ended in unexpected {status:?} status"
        ))),
    }
}

/// Marks every module still on the DFS stack `Errored` with `error`, rejects
/// the root's [[TopLevelCapability]] when one exists, and returns the error.
fn fail_evaluation(
    runtime: &mut Runtime,
    root: ModuleRecordId,
    realm: RealmId,
    stack: &[ModuleRecordId],
    error: ModuleError,
) -> Result<(), ModuleError> {
    for &module in stack {
        if let Some(record) = runtime.modules.get_mut(module) {
            record.evaluation_error = Some(error.clone());
            record.status = ModuleStatus::Errored;
        }
    }
    let capability = runtime
        .modules
        .get(root)
        .and_then(|r| r.top_level_capability);
    if let Some(capability) = capability
        && let Ok(reason) = crate::vm::module_error_rejection_value(runtime, realm, &error)
    {
        // A settlement failure here is an engine-internal failure; the
        // module error is the primary result either way.
        let _ = crate::vm::reject_promise_host(runtime, capability, reason);
    }
    Err(error)
}

/// Allocates the root's [[TopLevelCapability]] promise if it does not exist.
fn ensure_top_level_capability(
    runtime: &mut Runtime,
    realm: RealmId,
    module: ModuleRecordId,
) -> Result<(), ModuleError> {
    let existing = runtime
        .modules
        .get(module)
        .and_then(|r| r.top_level_capability);
    if existing.is_some() {
        return Ok(());
    }
    let promise = runtime.allocate_intrinsic_promise(realm).map_err(|error| {
        ModuleError::evaluate(format!("top-level capability allocation failed: {error}"))
    })?;
    if let Some(record) = runtime.modules.get_mut(module) {
        record.top_level_capability = Some(promise);
    }
    Ok(())
}

/// ExecuteAsyncModule (ECMA-262 16.6.1.5): kicks the async module root.
///
/// The root frame runs as an async function activation and returns its
/// activation promise; suspension and resumption ride the existing
/// async-function job machinery.
fn execute_async_module(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<(), ModuleError> {
    let function = runtime
        .modules
        .get(module)
        .and_then(|r| r.root_function)
        .ok_or_else(|| ModuleError::evaluate("module root function not installed"))?;

    let result = crate::vm::execute_module_frame_internal(
        runtime,
        function,
        StoredValue::Undefined,
        limits,
        compiler,
    );
    match result {
        Ok(StoredValue::Object(activation_promise)) => {
            // ECMA-262 16.6.1.5 step 7: PerformPromiseThen on the activation
            // promise with the AsyncModuleExecutionFulfilled/Rejected closures.
            crate::vm::perform_targeted_promise_reactions_host(
                runtime,
                activation_promise,
                crate::object::PromiseReactionTarget::AsyncModule { module },
            )
            .map_err(|error| {
                ModuleError::evaluate(format!(
                    "async module reaction registration failed: {error}"
                ))
            })?;
            Ok(())
        }
        Ok(_) => Err(ModuleError::evaluate(
            "async module root did not return an activation promise (engine invariant)",
        )),
        Err(error) => Err(ModuleError::evaluate(format!(
            "async module root threw synchronously (engine invariant): {error}"
        ))),
    }
}

/// AsyncModuleExecutionFulfilled (ECMA-262 16.6.1.4.2).
///
/// Runs inside a Promise job with no interpreter frames active. Deferred
/// module bodies execute under default limits with the self-contained dynamic
/// function compiler (`None`): `begin_promise_job` does not carry the caller's
/// compiler through.
pub(crate) fn async_module_execution_fulfilled(
    runtime: &mut Runtime,
    module: ModuleRecordId,
) -> Result<(), crate::ExecutionError> {
    match module_status(runtime, module) {
        ModuleStatus::Evaluated | ModuleStatus::Errored => return Ok(()),
        status => debug_assert_eq!(status, ModuleStatus::EvaluatingAsync),
    }
    let capability = {
        let Some(record) = runtime.modules.get_mut(module) else {
            return Ok(());
        };
        record.async_evaluation_order = None;
        record.status = ModuleStatus::Evaluated;
        record.top_level_capability
    };
    if let Some(capability) = capability {
        crate::vm::fulfill_promise_host(runtime, capability, StoredValue::Undefined)?;
    }

    let mut exec_list = Vec::new();
    gather_available_ancestors(runtime, module, &mut exec_list);
    exec_list.sort_by_key(|&ancestor| {
        runtime
            .modules
            .get(ancestor)
            .and_then(|r| r.async_evaluation_order)
            .unwrap_or(u32::MAX)
    });
    for ancestor in exec_list {
        match module_status(runtime, ancestor) {
            ModuleStatus::Evaluated | ModuleStatus::Errored => continue,
            _ => {}
        }
        if module_has_tla(runtime, ancestor) {
            execute_async_module(runtime, ancestor, ExecutionLimits::default(), None).map_err(
                |_| crate::EngineFault::RuntimeInvariant {
                    message: "deferred async module kick failed after its dependencies fulfilled",
                },
            )?;
            continue;
        }
        match execute_module_body(runtime, ancestor, ExecutionLimits::default(), None) {
            Ok(()) => {
                let capability = {
                    let Some(record) = runtime.modules.get_mut(ancestor) else {
                        continue;
                    };
                    record.async_evaluation_order = None;
                    record.status = ModuleStatus::Evaluated;
                    record.top_level_capability
                };
                if let Some(capability) = capability {
                    crate::vm::fulfill_promise_host(runtime, capability, StoredValue::Undefined)?;
                }
            }
            Err(error) => {
                let realm = runtime
                    .modules
                    .get(ancestor)
                    .map(|r| r.realm)
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "executing deferred module body lost its record",
                    })?;
                let value = crate::vm::module_error_rejection_value(runtime, realm, &error)?;
                reject_async_module_tree(runtime, ancestor, &value, &error)?;
            }
        }
    }
    Ok(())
}

/// AsyncModuleExecutionRejected (ECMA-262 16.6.1.4.3): records `value` as the
/// module's evaluation error and propagates it through [[AsyncParentModules]].
pub(crate) fn async_module_execution_rejected(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    value: StoredValue,
) -> Result<(), crate::ExecutionError> {
    let error = ModuleError::evaluate_rejection(runtime, &value)?;
    reject_async_module_tree(runtime, module, &value, &error)
}

/// Iterative (explicit-stack, spec DFS order) error propagation: every module
/// still awaiting evaluation becomes Errored with `error` and its
/// [[TopLevelCapability]] rejects with `value`.
fn reject_async_module_tree(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    value: &StoredValue,
    error: &ModuleError,
) -> Result<(), crate::ExecutionError> {
    let mut stack = vec![module];
    while let Some(current) = stack.pop() {
        match module_status(runtime, current) {
            ModuleStatus::Evaluated | ModuleStatus::Errored => continue,
            _ => {}
        }
        let (capability, parents) = {
            let Some(record) = runtime.modules.get_mut(current) else {
                continue;
            };
            record.evaluation_error = Some(error.clone());
            record.status = ModuleStatus::Errored;
            record.async_evaluation_order = None;
            (
                record.top_level_capability,
                record.async_parent_modules.clone(),
            )
        };
        if let Some(capability) = capability {
            crate::vm::reject_promise_host(runtime, capability, value.duplicate())?;
        }
        for parent in parents.into_iter().rev() {
            stack.push(parent);
        }
    }
    Ok(())
}

/// GatherAvailableAncestors (ECMA-262 16.6.1.4.1): collects the modules whose
/// last pending async dependency just fulfilled, in spec depth-first order.
fn gather_available_ancestors(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    exec_list: &mut Vec<ModuleRecordId>,
) {
    // Explicit parent-list frames mirror the spec's recursion: a newly
    // unblocked ancestor without top-level await gathers its own ancestors.
    let mut frames = vec![(async_parent_modules(runtime, module), 0_usize)];
    loop {
        let Some((parents, index)) = frames.last_mut() else {
            break;
        };
        let Some(ancestor) = parents.get(*index).copied() else {
            frames.pop();
            continue;
        };
        *index += 1;
        if exec_list.contains(&ancestor) {
            continue;
        }
        let cycle_root = runtime
            .modules
            .get(ancestor)
            .and_then(|r| r.cycle_root)
            .unwrap_or(ancestor);
        if module_status(runtime, cycle_root) == ModuleStatus::Errored {
            continue;
        }
        let Some(record) = runtime.modules.get_mut(ancestor) else {
            continue;
        };
        record.pending_async_dependencies -= 1;
        if record.pending_async_dependencies == 0 {
            exec_list.push(ancestor);
            if !module_has_tla(runtime, ancestor) {
                let grandparents = async_parent_modules(runtime, ancestor);
                frames.push((grandparents, 0));
            }
        }
    }
}

fn async_parent_modules(runtime: &Runtime, module: ModuleRecordId) -> Vec<ModuleRecordId> {
    runtime
        .modules
        .get(module)
        .map(|r| r.async_parent_modules.clone())
        .unwrap_or_default()
}

/// Whether the module's evaluation is still pending asynchronous completion.
pub(crate) fn module_is_evaluating_async(runtime: &Runtime, module: ModuleRecordId) -> bool {
    module_status(runtime, module) == ModuleStatus::EvaluatingAsync
}

/// Returns the module's [[TopLevelCapability]] promise, when allocated.
pub(crate) fn module_top_level_capability(
    runtime: &Runtime,
    module: ModuleRecordId,
) -> Option<crate::runtime::ObjectId> {
    runtime
        .modules
        .get(module)
        .and_then(|r| r.top_level_capability)
}

/// Builds ECMA-262 InnerModuleEvaluation's `evaluationList` for `module`:
/// eager-phase requests contribute the requested module itself; defer-phase
/// requests contribute the module's asynchronous transitive dependencies
/// (GatherAsynchronousTransitiveDependencies) instead, leaving the deferred
/// module and its synchronous subtree unevaluated.
pub(crate) fn module_evaluation_list(
    runtime: &Runtime,
    module: ModuleRecordId,
) -> Vec<ModuleRecordId> {
    let mut list = Vec::new();
    let mut seen = HashSet::new();
    let Some(record) = runtime.modules.get(module) else {
        return list;
    };
    let syntax = record.syntax_record.clone();
    for (index, request) in syntax.requests().iter().enumerate() {
        let Ok(dep) = super::linking::resolve_request(runtime, module, index as u32) else {
            continue;
        };
        if request.is_deferred() {
            let mut gathered = Vec::new();
            let mut gather_seen = Vec::new();
            gather_async_transitive_dependencies(runtime, dep, &mut gather_seen, &mut gathered);
            for additional in gathered {
                if seen.insert(additional) {
                    list.push(additional);
                }
            }
        } else if seen.insert(dep) {
            list.push(dep);
        }
    }
    list
}

/// ECMA-262 `IsModuleSCCEvaluated`: a module whose strongly connected
/// component has a cycle root counts as evaluated only once that root has
/// reached ~evaluated~ — a member of an in-flight async cycle does not.
pub(crate) fn is_module_scc_evaluated(runtime: &Runtime, module: ModuleRecordId) -> bool {
    let Some(record) = runtime.modules.get(module) else {
        return false;
    };
    if let Some(cycle_root) = record.cycle_root {
        return runtime
            .modules
            .get(cycle_root)
            .is_some_and(|root| root.status == ModuleStatus::Evaluated);
    }
    record.status == ModuleStatus::Evaluated
}

/// ECMA-262 `GatherAsynchronousTransitiveDependencies`: the post-order list of
/// unevaluated modules with top-level await reachable from `module` without
/// crossing an already-evaluated (or evaluating) branch, where "evaluated" is
/// the cycle-root-aware `IsModuleSCCEvaluated` predicate. The walk covers both
/// phases of static requests.
pub(crate) fn gather_async_transitive_dependencies(
    runtime: &Runtime,
    module: ModuleRecordId,
    seen: &mut Vec<ModuleRecordId>,
    out: &mut Vec<ModuleRecordId>,
) {
    if seen.contains(&module) {
        return;
    }
    seen.push(module);
    let Some(record) = runtime.modules.get(module) else {
        return;
    };
    if record.status == ModuleStatus::Evaluating
        || record.status == ModuleStatus::Errored
        || is_module_scc_evaluated(runtime, module)
    {
        return;
    }
    if module_has_tla(runtime, module) {
        out.push(module);
        return;
    }
    for dep in super::linking::module_dependencies(runtime, module) {
        gather_async_transitive_dependencies(runtime, dep, seen, out);
    }
}

/// ECMA-262 `ReadyForSyncExecution`: whether `module` can be evaluated to
/// completion synchronously right now — itself, and every module reachable
/// through its static requests, must not be evaluating, evaluating
/// asynchronously, or carry top-level await, and must not belong to an
/// unevaluated strongly connected component. Already-evaluated (or errored,
/// whose stored error rethrows) modules are ready.
pub(crate) fn ready_for_sync_execution(
    runtime: &Runtime,
    module: ModuleRecordId,
    seen: &mut Vec<ModuleRecordId>,
) -> bool {
    if seen.contains(&module) {
        return true;
    }
    seen.push(module);
    let Some(record) = runtime.modules.get(module) else {
        return false;
    };
    if is_module_scc_evaluated(runtime, module) || record.status == ModuleStatus::Errored {
        return true;
    }
    match record.status {
        ModuleStatus::Evaluating | ModuleStatus::EvaluatingAsync => return false,
        ModuleStatus::Linked | ModuleStatus::Evaluated => {}
        _ => return false,
    }
    if module_has_tla(runtime, module) {
        return false;
    }
    for dep in super::linking::module_dependencies(runtime, module) {
        if !ready_for_sync_execution(runtime, dep, seen) {
            return false;
        }
    }
    true
}

fn execute_module_body(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<(), ModuleError> {
    let function = runtime
        .modules
        .get(module)
        .and_then(|r| r.root_function)
        .ok_or_else(|| ModuleError::evaluate("module root function not installed"))?;

    let result = crate::vm::execute_module_frame_internal(
        runtime,
        function,
        StoredValue::Undefined,
        limits,
        compiler,
    );
    match result {
        Ok(_completion) => Ok(()),
        Err(crate::ExecutionError::Exception(exception)) => {
            Err(ModuleError::evaluate_exception(runtime, exception))
        }
        Err(e) => Err(ModuleError::evaluate(format!("execution error: {e}"))),
    }
}

/// [[HasTLA]]: the compiler marks a top-level-await module root by compiling
/// it as an async function (see `FunctionKind` of the root function header).
fn module_has_tla(runtime: &Runtime, module: ModuleRecordId) -> bool {
    runtime.modules.get(module).is_some_and(|record| {
        record
            .authority
            .root()
            .function()
            .control_flow()
            .function_header()
            .kind()
            == FunctionKind::Async
    })
}

/// IncrementModuleAsyncEvaluationCount.
fn next_module_async_evaluation_order(runtime: &mut Runtime) -> u32 {
    runtime.module_async_evaluation_count += 1;
    runtime.module_async_evaluation_count
}

fn module_evaluation_error(runtime: &Runtime, module: ModuleRecordId) -> ModuleError {
    runtime
        .modules
        .get(module)
        .and_then(|r| r.evaluation_error.clone())
        .unwrap_or_else(|| ModuleError::evaluate("module was in errored state"))
}

/// Returns the evaluation error recorded on `module`'s strongly connected
/// component root, if any (ECMA-262 Evaluate's cycle-root redirection for an
/// evaluated member of an errored cycle).
fn cycle_root_evaluation_error(runtime: &Runtime, module: ModuleRecordId) -> Option<ModuleError> {
    let record = runtime.modules.get(module)?;
    let root = record.cycle_root.unwrap_or(module);
    runtime
        .modules
        .get(root)
        .and_then(|root| root.evaluation_error.clone())
}

fn module_status(runtime: &Runtime, module: ModuleRecordId) -> ModuleStatus {
    runtime
        .modules
        .get(module)
        .map(|r| r.status)
        .unwrap_or(ModuleStatus::New)
}

fn set_module_status(runtime: &mut Runtime, module: ModuleRecordId, status: ModuleStatus) {
    if let Some(record) = runtime.modules.get_mut(module) {
        record.status = status;
    }
}
