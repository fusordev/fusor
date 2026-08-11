//! Module evaluation: InnerModuleEvaluation (synchronous subset).
//!
//! Iterative explicit-stack DFS with cycle handling and `[[EvaluationError]]`
//! propagation through the strongly-connected component.

use super::{ModuleError, ModuleRecordId, ModuleStatus};
use crate::runtime::{ExecutionLimits, RealmId, Runtime, StoredValue};
use std::fmt;

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

/// Evaluates a module graph starting from `root`.
pub fn evaluate_module(
    runtime: &mut Runtime,
    _realm: RealmId,
    root: ModuleRecordId,
    limits: ExecutionLimits,
) -> Result<(), ModuleError> {
    let mut stack: Vec<EvalWorkItem> = vec![EvalWorkItem::Enter(root)];

    while let Some(item) = stack.pop() {
        match item {
            EvalWorkItem::Enter(module) => {
                let status = module_status(runtime, module);
                match status {
                    ModuleStatus::Evaluated => continue,
                    ModuleStatus::Errored => {
                        let error = runtime
                            .modules
                            .get(module)
                            .and_then(|r| r.evaluation_error.clone());
                        return Err(error.unwrap_or_else(|| {
                            ModuleError::evaluate("module was in errored state")
                        }));
                    }
                    ModuleStatus::Evaluating => continue,
                    ModuleStatus::Linked => {}
                    _ => {
                        return Err(ModuleError::evaluate(format!(
                            "module is in {:?} status, expected Linked",
                            status
                        )));
                    }
                }
                set_module_status(runtime, module, ModuleStatus::Evaluating);
                stack.push(EvalWorkItem::Execute(module));

                let deps = super::linking::module_dependencies(runtime, module);
                for dep in deps.into_iter().rev() {
                    let dep_status = module_status(runtime, dep);
                    if dep_status == ModuleStatus::Linked {
                        stack.push(EvalWorkItem::Enter(dep));
                    }
                }
            }
            EvalWorkItem::Execute(module) => {
                match execute_module_body(runtime, module, limits) {
                    Ok(()) => {
                        set_module_status(runtime, module, ModuleStatus::Evaluated);
                    }
                    Err(error) => {
                        let cycle_root = runtime
                            .modules
                            .get(module)
                            .and_then(|r| r.cycle_root)
                            .unwrap_or(module);
                        if let Some(record) = runtime.modules.get_mut(cycle_root) {
                            record.evaluation_error = Some(error.clone());
                            record.status = ModuleStatus::Errored;
                        }
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok(())
}

fn execute_module_body(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    limits: ExecutionLimits,
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
    );
    match result {
        Ok(_completion) => Ok(()),
        Err(crate::ExecutionError::Exception(exception)) => {
            Err(ModuleError::evaluate_exception(runtime, exception))
        }
        Err(e) => Err(ModuleError::evaluate(format!("execution error: {e}"))),
    }
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
