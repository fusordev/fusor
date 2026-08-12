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
    Execute(ModuleRecordId),
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
        ModuleStatus::Evaluated => return Ok(()),
        ModuleStatus::EvaluatingAsync => {
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
                work.push(EvalWorkItem::Execute(module));

                let deps = super::linking::module_dependencies(runtime, module);
                for dep in deps.into_iter().rev() {
                    if module_status(runtime, dep) == ModuleStatus::Linked {
                        work.push(EvalWorkItem::Enter(dep));
                    }
                }
            }
            EvalWorkItem::Execute(module) => {
                // Dependency bookkeeping in source order (ECMA-262 16.6.1.4
                // step 11.c): a dependency still on the DFS stack tightens
                // this module's ancestor index; a completed dependency
                // contributes its cycle root. Either way, an async dependency
                // becomes a pending async dependency of this module.
                let deps = super::linking::module_dependencies(runtime, module);
                for dep in deps {
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
                // cycle root and async evaluation order.
                let (dfs, ancestor) = runtime
                    .modules
                    .get(module)
                    .map_or((None, None), |r| (r.dfs_index, r.dfs_ancestor_index));
                if dfs.is_some() && dfs == ancestor {
                    let order = runtime
                        .modules
                        .get(module)
                        .and_then(|r| r.async_evaluation_order);
                    while let Some(popped) = stack.pop() {
                        if let Some(record) = runtime.modules.get_mut(popped) {
                            record.cycle_root = Some(module);
                            record.async_evaluation_order = order;
                            if record.status != ModuleStatus::Errored {
                                record.status = if order.is_some() {
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
        Ok(StoredValue::Object(_activation_promise)) => {
            // TODO(step D): attach fulfill/reject reactions to the activation
            // promise (PromiseReactionTarget::AsyncModule) that decrement
            // [[PendingAsyncDependencies]] of [[AsyncParentModules]], execute
            // newly unblocked parents, and settle [[TopLevelCapability]].
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
