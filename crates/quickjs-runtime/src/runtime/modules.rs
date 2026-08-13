//! Source-text module records, per-realm registry, linking, and evaluation.
//!
//! The runtime owns the module record lifecycle: the facade registers a record
//! (key, syntax record, verified bytecode authority) and then asks the runtime
//! to link and evaluate a root. The runtime performs no IO and no parsing; the
//! facade resolves specifiers, reads source text, parses, and compiles before
//! registering. The host loader boundary trait is designed so dynamic
//! `import()` can later emit a host load request event completed by an
//! explicit host call without rework.

use std::collections::HashMap;
use std::sync::Arc;

use quickjs_bytecode::{ModuleDeclarationRecord, VerifiedBytecode};
use quickjs_frontend::ModuleSyntaxRecord;

use super::{
    BindingCell, BindingCellId, FunctionId, InstalledCodeId, ModuleRecordId, ObjectId, RealmId,
    Runtime,
};

mod evaluation;
mod host;
mod import_meta;
mod linking;
mod namespace;

pub use evaluation::{ModuleEvaluationError, evaluate_module};
pub(crate) use evaluation::{
    async_module_execution_fulfilled, async_module_execution_rejected,
    gather_async_transitive_dependencies, module_is_evaluating_async, module_top_level_capability,
};
pub use host::{ImportMetaHook, ModuleLoader, ModuleResolveError, default_import_meta_resolve};
pub(crate) use import_meta::get_or_create_import_meta;
pub use linking::{ModuleLinkError, link_module};
pub(crate) use linking::{get_or_create_namespace, get_or_create_namespace_phase};
pub(crate) use namespace::{DeferredNamespaceEvaluationFailure, ModuleNamespaceState};

use std::fmt;

/// A host-canonicalized module key.
///
/// Two equal keys identify the same module record within a realm. The facade
/// controls canonicalization (e.g. URL normalization, attribute hashing).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ModuleKey(Arc<str>);

impl ModuleKey {
    /// Creates a module key from a canonical string.
    #[must_use]
    pub fn new(key: Arc<str>) -> Self {
        Self(key)
    }

    /// Returns the canonical string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModuleKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Module linking status per ECMA-262 16.2.1.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleStatus {
    New,
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
    /// The module or one in its strongly connected component threw during
    /// evaluation. `evaluation_error` holds the error.
    Errored,
}

/// The phase in which a module error occurred.
///
/// Link-time errors (unresolved imports, ambiguity) are SyntaxError-class
/// rejections. Evaluation-time errors carry the original JavaScript exception.
#[derive(Clone, Debug)]
pub enum ModuleErrorPhase {
    /// Linking / resolution failure.
    Link,
    /// Evaluation failure.
    Evaluate,
}

/// A typed module error retaining the spec phase.
#[derive(Clone, Debug)]
pub struct ModuleError {
    pub(crate) phase: ModuleErrorPhase,
    pub(crate) message: String,
    pub(crate) exception: Option<crate::JsException>,
    /// The original ECMAScript rejection value of an asynchronous evaluation
    /// failure (ECMA-262 [[EvaluationError]] of an async module). A heap value
    /// stays rooted by this public root until the error is dropped.
    pub(crate) rejection_value: Option<crate::JsValue>,
}

impl ModuleError {
    pub(crate) fn link(message: impl Into<String>) -> Self {
        Self {
            phase: ModuleErrorPhase::Link,
            message: message.into(),
            exception: None,
            rejection_value: None,
        }
    }

    pub(crate) fn evaluate(message: impl Into<String>) -> Self {
        Self {
            phase: ModuleErrorPhase::Evaluate,
            message: message.into(),
            exception: None,
            rejection_value: None,
        }
    }

    pub(crate) fn evaluate_exception(runtime: &Runtime, exception: crate::JsException) -> Self {
        let message = exception_message(runtime, &exception);
        Self {
            phase: ModuleErrorPhase::Evaluate,
            message,
            exception: Some(exception),
            rejection_value: None,
        }
    }

    /// Records an asynchronous evaluation rejection, preserving the original
    /// rejection value for later `import()` settlement.
    pub(crate) fn evaluate_rejection(
        runtime: &mut Runtime,
        value: &crate::runtime::StoredValue,
    ) -> Result<Self, crate::ExecutionError> {
        Ok(Self {
            phase: ModuleErrorPhase::Evaluate,
            message: rejection_value_message(runtime, value),
            exception: None,
            rejection_value: Some(runtime.public_value(value.duplicate())?),
        })
    }

    /// Returns the original rejection value of an async evaluation failure.
    #[must_use]
    pub fn rejection_value(&self) -> Option<&crate::JsValue> {
        self.rejection_value.as_ref()
    }

    /// Returns the spec phase in which the error occurred.
    #[must_use]
    pub const fn phase(&self) -> &ModuleErrorPhase {
        &self.phase
    }

    /// Returns the human-readable error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the JavaScript exception for evaluation errors, when present.
    #[must_use]
    pub const fn exception(&self) -> Option<&crate::JsException> {
        self.exception.as_ref()
    }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.phase {
            ModuleErrorPhase::Link => {
                write!(formatter, "module link error: {}", self.message)
            }
            ModuleErrorPhase::Evaluate => {
                write!(formatter, "module evaluation error: {}", self.message)
            }
        }
    }
}

impl std::error::Error for ModuleError {}

/// Extracts a human-readable message from an escaping exception.
///
/// Engine errors carry their message directly. An explicit `throw` of an Error
/// object exposes its own `message` data property (a plain own-property read;
/// user getters are not invoked at this boundary).
fn exception_message(runtime: &Runtime, exception: &crate::JsException) -> String {
    if let Some(message) = exception.message()
        && let Ok(text) = message.to_utf8_lossy()
    {
        return text;
    }
    if let Some(value) = exception.thrown_value() {
        let reference = match value.stored() {
            Ok(crate::runtime::StoredValue::Object(object)) => {
                Some(crate::runtime::HeapReference::Object(*object))
            }
            Ok(crate::runtime::StoredValue::Function(function)) => {
                Some(crate::runtime::HeapReference::Function(*function))
            }
            _ => None,
        };
        if let Some(reference) = reference {
            let key = runtime.predefined_property_key(crate::PredefinedAtom::Message);
            if let Ok(record) = runtime.object_record(reference)
                && let Some(crate::object::OwnProperty::Data {
                    value: crate::runtime::StoredValue::String(text),
                    ..
                }) = record.own_property(&key)
                && let Ok(text) = text.to_utf8_lossy()
            {
                return text;
            }
        }
    }
    "module evaluation error".to_owned()
}

/// Extracts a human-readable message from an async evaluation rejection value.
///
/// Mirrors the explicit-`throw` branch of [`exception_message`]: an Error
/// object exposes its own `message` data property (user getters are not
/// invoked at this boundary).
fn rejection_value_message(runtime: &Runtime, value: &crate::runtime::StoredValue) -> String {
    let reference = match value {
        crate::runtime::StoredValue::Object(object) => {
            crate::runtime::HeapReference::Object(*object)
        }
        crate::runtime::StoredValue::Function(function) => {
            crate::runtime::HeapReference::Function(*function)
        }
        _ => return "module evaluation error".to_owned(),
    };
    let key = runtime.predefined_property_key(crate::PredefinedAtom::Message);
    if let Ok(record) = runtime.object_record(reference)
        && let Some(crate::object::OwnProperty::Data {
            value: crate::runtime::StoredValue::String(text),
            ..
        }) = record.own_property(&key)
        && let Ok(text) = text.to_utf8_lossy()
    {
        return text;
    }
    "module evaluation error".to_owned()
}

/// A resolved export target (ECMA-262 `ResolvedBinding`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum ResolvedExport {
    /// A binding cell in the module's environment ([[BindingName]] is a name).
    Binding {
        module: ModuleRecordId,
        cell: BindingCellId,
    },
    /// The module namespace object of `module` (ResolveExport of an
    /// `export * as name` entry; [[BindingName]] is ~namespace~).
    Namespace { module: ModuleRecordId },
}

/// One source-text module record.
///
/// Carries the owned syntax record, verified bytecode authority, linking
/// status, DFS indices, persisted module environment, and namespace object.
pub(crate) struct SourceTextModuleRecord {
    pub(crate) realm: RealmId,
    pub(crate) key: ModuleKey,
    pub(crate) syntax_record: ModuleSyntaxRecord,
    pub(crate) authority: Arc<VerifiedBytecode>,
    pub(crate) status: ModuleStatus,
    /// Host-recorded resolution edges (ECMA-262 `HostResolveImportedModule`):
    /// maps each raw import specifier text of this module to the module record
    /// the host resolved it to at load time.
    pub(crate) resolved_dependencies: HashMap<String, ModuleRecordId>,
    pub(crate) dfs_index: Option<u32>,
    pub(crate) dfs_ancestor_index: Option<u32>,
    pub(crate) cycle_root: Option<ModuleRecordId>,
    pub(crate) evaluation_error: Option<ModuleError>,
    /// Modules whose async evaluation this module waits on
    /// (ECMA-262 [[PendingAsyncDependencies]]).
    pub(crate) pending_async_dependencies: u32,
    /// ECMA-262 [[AsyncEvaluationOrder]]: `None` is ~empty~.
    pub(crate) async_evaluation_order: Option<u32>,
    /// Modules waiting on this module's async evaluation
    /// (ECMA-262 [[AsyncParentModules]]).
    pub(crate) async_parent_modules: Vec<ModuleRecordId>,
    /// ECMA-262 [[TopLevelCapability]] promise for `Evaluate()`.
    pub(crate) top_level_capability: Option<ObjectId>,
    /// Persisted module environment: one cell per declaration-record binding
    /// in declaration order.
    pub(crate) environment: Vec<BindingCellId>,
    /// Lazily materialized namespace object.
    pub(crate) namespace_object: Option<ObjectId>,
    /// Lazily materialized deferred namespace object (ECMA-262
    /// [[DeferredNamespace]], cached separately from [[Namespace]]).
    pub(crate) deferred_namespace: Option<ObjectId>,
    /// Lazily materialized `import.meta` object.
    pub(crate) meta_object: Option<ObjectId>,
    /// Installed code and root function for execution (set during linking).
    pub(crate) installed_code: Option<InstalledCodeId>,
    pub(crate) root_function: Option<FunctionId>,
}

impl SourceTextModuleRecord {
    pub(crate) fn new(
        realm: RealmId,
        key: ModuleKey,
        syntax_record: ModuleSyntaxRecord,
        authority: Arc<VerifiedBytecode>,
    ) -> Self {
        Self {
            realm,
            key,
            syntax_record,
            authority,
            status: ModuleStatus::New,
            resolved_dependencies: HashMap::new(),
            dfs_index: None,
            dfs_ancestor_index: None,
            cycle_root: None,
            evaluation_error: None,
            pending_async_dependencies: 0,
            async_evaluation_order: None,
            async_parent_modules: Vec::new(),
            top_level_capability: None,
            environment: Vec::new(),
            namespace_object: None,
            deferred_namespace: None,
            meta_object: None,
            installed_code: None,
            root_function: None,
        }
    }

    /// Returns the module declaration record from the verified bytecode.
    pub(crate) fn declaration_record(&self) -> &ModuleDeclarationRecord {
        self.authority
            .module()
            .expect("Module root carries a declaration record")
    }
}
