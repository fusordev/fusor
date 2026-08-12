//! Parked dynamic `import()` load requests and their host completion.
//!
//! `EvaluateImportCall` keeps its observable front half in the interpreter
//! (intrinsic Promise, specifier conversion, `options.with` attribute reads).
//! Once the import attributes are known, the runtime parks a
//! [`PendingDynamicImport`] record instead of loading anything itself: the
//! runtime performs no IO and no parsing. The host (the facade, or an
//! asynchronous driver completing loads on host turns) takes records from the
//! FIFO, resolves and loads the module graph, registers the compiled records,
//! and then completes the import through `Context::complete_dynamic_import`
//! (link + evaluate + fulfill with the namespace object) or
//! `Context::reject_dynamic_import` (load/resolution failure).
//!
//! The queue mirrors the `Atomics.waitAsync` waiter pattern: it is bounded by
//! `RuntimeLimits::max_pending_dynamic_imports`, the parked Promise is traced
//! as a GC root so the load cannot lose its settlement target, and records
//! leave the queue only through an explicit host call.

use super::{ModuleKey, ObjectId, RealmId, Runtime, RuntimeResource, check_execution_limit};
use crate::{ExecutionError, JsString};

pub(crate) struct PendingDynamicImportRecord {
    pub(crate) realm: RealmId,
    pub(crate) referrer: Option<ModuleKey>,
    pub(crate) specifier: JsString,
    pub(crate) attributes: Vec<(JsString, JsString)>,
    pub(crate) promise: ObjectId,
}

/// A parked dynamic `import()` awaiting host load completion.
///
/// The record is opaque: the host reads the referrer, specifier, and import
/// attributes to drive resolution and loading, then passes the record back to
/// `Context::complete_dynamic_import` or `Context::reject_dynamic_import`.
/// Dropping the record without completing it abandons the import Promise; it
/// stays pending until the realm is torn down.
pub struct PendingDynamicImport {
    pub(crate) record: PendingDynamicImportRecord,
}

impl PendingDynamicImport {
    /// Returns the canonical key of the module that initiated this import, if
    /// the `import()` call ran inside a module.
    #[must_use]
    pub const fn referrer(&self) -> Option<&ModuleKey> {
        self.record.referrer.as_ref()
    }

    /// Returns the specifier text as supplied to `import()` (lossy UTF-8).
    #[must_use]
    pub fn specifier(&self) -> String {
        self.record.specifier.to_utf8_lossy().unwrap_or_default()
    }

    /// Returns the import attributes, sorted by key (lossy UTF-8).
    ///
    /// The current front half rejects every non-empty attribute set before
    /// parking (`unsupported dynamic import attribute`), so this is empty
    /// until host-supported attribute keys are admitted.
    #[must_use]
    pub fn attributes(&self) -> Vec<(String, String)> {
        self.record
            .attributes
            .iter()
            .map(|(key, value)| {
                (
                    key.to_utf8_lossy().unwrap_or_default(),
                    value.to_utf8_lossy().unwrap_or_default(),
                )
            })
            .collect()
    }
}

impl std::fmt::Debug for PendingDynamicImport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingDynamicImport")
            .field("referrer", &self.record.referrer)
            .field("specifier", &self.specifier())
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Parks one dynamic `import()` load request, retaining its Promise as a
    /// GC root until the host completes it.
    pub(crate) fn park_dynamic_import(
        &mut self,
        realm: RealmId,
        referrer: Option<ModuleKey>,
        specifier: JsString,
        attributes: Vec<(JsString, JsString)>,
        promise: ObjectId,
    ) -> Result<(), ExecutionError> {
        check_execution_limit(
            RuntimeResource::DynamicImportLoads,
            self.limits.max_pending_dynamic_imports,
            super::usize_to_u64(self.pending_dynamic_imports.len()).saturating_add(1),
        )?;
        self.pending_dynamic_imports
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::DynamicImportLoads,
                additional: 1,
            })?;
        self.pending_dynamic_imports
            .push_back(PendingDynamicImportRecord {
                realm,
                referrer,
                specifier,
                attributes,
                promise,
            });
        self.collection_pending = true;
        Ok(())
    }

    /// Removes the oldest parked dynamic `import()` load request, if any.
    pub(crate) fn take_pending_dynamic_import(&mut self) -> Option<PendingDynamicImport> {
        self.pending_dynamic_imports
            .pop_front()
            .map(|record| PendingDynamicImport { record })
    }

    /// Returns the number of parked dynamic `import()` load requests.
    #[must_use]
    pub fn pending_dynamic_import_count(&self) -> usize {
        self.pending_dynamic_imports.len()
    }

    /// Looks up a registered module record by canonical key in `realm`.
    pub(crate) fn registered_module(
        &self,
        realm: RealmId,
        key: &ModuleKey,
    ) -> Option<super::ModuleRecordId> {
        self.realms.get(realm)?.module_registry.get(key).copied()
    }

    /// Returns the recorded evaluation error of the module registered under
    /// `key` in `realm`, if its evaluation failed (ECMA-262
    /// [[EvaluationError]]).
    #[must_use]
    pub fn module_evaluation_error(
        &self,
        realm: &super::Realm,
        key: &ModuleKey,
    ) -> Option<crate::ModuleError> {
        let module = self.registered_module(realm.0.id, key)?;
        self.modules.get(module)?.evaluation_error.clone()
    }
}
