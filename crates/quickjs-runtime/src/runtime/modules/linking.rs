//! Module linking: ResolveExport and InitializeEnvironment.
//!
//! Implements the synchronous subset of ECMA-262 16.2.1.6 (InnerModuleLinking)
//! and 16.2.1.7 (InitializeEnvironment) using an explicit-stack DFS.

use std::collections::HashSet;

use quickjs_bytecode::{CompilerInitializationPolicy, ModuleBindingOrigin, ModuleImportName};

use super::{BindingCell, ModuleError, ModuleRecordId, ModuleStatus, ResolvedExport};
use crate::runtime::{BindingCellId, RealmId, Runtime, SlotValue, StoredValue, usize_to_u64};

use std::fmt;

/// A linking failure retaining the error phase.
#[derive(Debug)]
pub struct ModuleLinkError {
    pub(crate) error: ModuleError,
}

impl ModuleLinkError {
    pub(crate) fn new(error: ModuleError) -> Self {
        Self { error }
    }

    /// Returns the underlying module error.
    #[must_use]
    pub const fn error(&self) -> &ModuleError {
        &self.error
    }
}

impl fmt::Display for ModuleLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ModuleLinkError {}

/// The compiler names the synthetic `export default` cell `*default*`; that
/// name is not a valid identifier, so it can never collide with a declared
/// binding.
const SYNTHETIC_DEFAULT_BINDING_NAME: &[u8] = b"*default*";

enum LinkWorkItem {
    Enter(ModuleRecordId),
    Initialize(ModuleRecordId),
}

/// Creates the module environment for `module`: one cell per declaration-record
/// binding, installs its code, and instantiates hoisted module functions.
///
/// This is the first linking phase. Per ECMA-262 `ResolveExport` returns a
/// *binding name*, not a cell, so a module in a cycle may need to resolve an
/// export of a module whose environment does not exist yet. Creating every
/// environment in the graph before resolving any import keeps cell forwarding
/// (this port's representation of indirect bindings) well defined for cycles.
fn create_module_environment(
    runtime: &mut Runtime,
    module: ModuleRecordId,
) -> Result<(), ModuleError> {
    let (authority, realm) = {
        let record = runtime.modules.get(module).expect("module exists");
        if record.installed_code.is_some() {
            return Ok(());
        }
        (record.authority.clone(), record.realm)
    };
    let decl = authority
        .module()
        .ok_or_else(|| ModuleError::link("module root carries no declaration record"))?
        .clone();
    let bindings_count = decl.bindings().len();

    crate::runtime::check_execution_limit(
        crate::RuntimeResource::BindingCells,
        runtime.limits.max_binding_cells,
        usize_to_u64(runtime.cells.len()).saturating_add(usize_to_u64(bindings_count)),
    )
    .map_err(|error| ModuleError::link(format!("cell limit: {error}")))?;
    runtime
        .cells
        .try_reserve(bindings_count)
        .map_err(|_| ModuleError::link("cell allocation failed"))?;

    let mut cells = Vec::with_capacity(bindings_count);
    for binding in decl.bindings() {
        let cell = runtime
            .cells
            .try_insert(BindingCell {
                value: initial_cell_value(binding),
                forward: None,
            })
            .map_err(|_| ModuleError::link("cell insertion failed"))?;
        cells.push(cell);
    }
    runtime
        .modules
        .get_mut(module)
        .expect("module exists")
        .environment = cells.clone();
    runtime.collection_pending = true;

    let (code_id, root_fn) = runtime
        .install_module_root(realm, authority.clone(), &cells)
        .map_err(|error| ModuleError::link(format!("install: {error}")))?;
    {
        let record = runtime.modules.get_mut(module).expect("module exists");
        record.installed_code = Some(code_id);
        record.root_function = Some(root_fn);
    }

    for (index, binding) in decl.bindings().iter().enumerate() {
        let Some(initializer) = binding.initializer() else {
            continue;
        };
        let parent_environment = runtime
            .functions
            .get(root_fn)
            .ok_or_else(|| ModuleError::link("module root function is stale"))?
            .bytecode()
            .map_err(|error| ModuleError::link(format!("root: {error}")))?
            .environment
            .clone();
        let function = runtime
            .create_module_closure(
                realm,
                code_id,
                &authority,
                quickjs_bytecode::FunctionTemplateId::new(initializer),
                &cells,
                &parent_environment,
            )
            .map_err(|error| ModuleError::link(format!("closure: {error}")))?;
        let resolved = BindingCell::resolve_forward(runtime, cells[index])
            .map_err(|error| ModuleError::link(format!("resolve: {error}")))?;
        runtime
            .cells
            .get_mut(resolved)
            .ok_or_else(|| ModuleError::link("cell stale"))?
            .value = SlotValue::Value(StoredValue::Function(function));
        runtime.collection_pending = true;
    }
    Ok(())
}

/// Links a module graph starting from `root`.
///
/// Implements `Link` / `InnerModuleLinking` with an explicit-stack DFS. Every
/// module environment in the graph is created before any import is resolved,
/// so cyclic graphs resolve exports against existing cells.
pub fn link_module(
    runtime: &mut Runtime,
    _realm: RealmId,
    root: ModuleRecordId,
) -> Result<(), ModuleError> {
    let mut stack: Vec<LinkWorkItem> = vec![LinkWorkItem::Enter(root)];
    let mut dfs_counter: u32 = 0;
    let mut visited: Vec<ModuleRecordId> = Vec::new();

    while let Some(item) = stack.pop() {
        match item {
            LinkWorkItem::Enter(module) => {
                let status = module_status(runtime, module);
                match status {
                    ModuleStatus::New | ModuleStatus::Unlinked => {}
                    _ => continue,
                }
                set_module_status(runtime, module, ModuleStatus::Linking);
                let dfs_index = dfs_counter;
                dfs_counter += 1;
                set_dfs(runtime, module, Some(dfs_index), Some(dfs_index));
                visited.push(module);
                stack.push(LinkWorkItem::Initialize(module));

                for dep in module_dependencies(runtime, module) {
                    match module_status(runtime, dep) {
                        ModuleStatus::New | ModuleStatus::Unlinked => {
                            stack.push(LinkWorkItem::Enter(dep));
                        }
                        ModuleStatus::Linking => {
                            let dep_dfs = module_dfs(runtime, dep).unwrap_or(u32::MAX);
                            let ancestor = module_anc(runtime, module);
                            set_dfs_anc(runtime, module, ancestor.min(dep_dfs));
                        }
                        _ => {}
                    }
                }
            }
            LinkWorkItem::Initialize(module) => {
                if let Err(error) = create_module_environment(runtime, module) {
                    unlink_all(runtime, &visited);
                    return Err(error);
                }
            }
        }
    }

    for &module in &visited {
        if let Err(error) = resolve_module_imports(runtime, module) {
            unlink_all(runtime, &visited);
            return Err(error);
        }
    }
    for &module in &visited {
        set_module_status(runtime, module, ModuleStatus::Linked);
    }
    Ok(())
}

/// Resets every module touched by a failed link back to `Unlinked`.
fn unlink_all(runtime: &mut Runtime, modules: &[ModuleRecordId]) {
    for &module in modules {
        set_module_status(runtime, module, ModuleStatus::Unlinked);
        set_dfs(runtime, module, None, None);
    }
}

/// ECMA-262 `ResolveExport` result: a resolved binding, `null`, or the
/// `ambiguous` sentinel. Ambiguity is a *value*, not an error: namespace
/// enumeration omits ambiguous names, while an explicit import binding of one
/// is a link-time `SyntaxError`.
enum ExportResolution {
    Resolved(ResolvedExport),
    Null,
    Ambiguous,
}

/// Resolves an export of a module to a binding cell (ResolveExport).
fn resolve_export(
    runtime: &Runtime,
    module: ModuleRecordId,
    export_name: &[u8],
    resolve_set: &mut Vec<(ModuleRecordId, Vec<u8>)>,
) -> Result<ExportResolution, ModuleError> {
    if resolve_set
        .iter()
        .any(|(m, name)| *m == module && name.as_slice() == export_name)
    {
        return Ok(ExportResolution::Null);
    }
    resolve_set.push((module, export_name.to_vec()));

    let syntax = runtime
        .modules
        .get(module)
        .expect("module exists")
        .syntax_record
        .clone();

    for entry in syntax.export_entries() {
        let export_matches = match entry.export_name() {
            quickjs_frontend::ModuleExportName::Name(name) => {
                name_units_eq_utf8(name.code_units(), export_name)
            }
            quickjs_frontend::ModuleExportName::Default(_) => export_name == b"default",
            _ => false,
        };
        if !export_matches {
            continue;
        }
        match entry.role() {
            quickjs_frontend::ModuleExportEntryRole::Local => {
                let local_bytes = match entry.local_name() {
                    quickjs_frontend::ModuleExportLocalName::Name(name) => {
                        units_to_utf8(name.code_units())
                    }
                    quickjs_frontend::ModuleExportLocalName::SyntheticDefault => {
                        SYNTHETIC_DEFAULT_BINDING_NAME.to_vec()
                    }
                    quickjs_frontend::ModuleExportLocalName::Null => {
                        resolve_set.pop();
                        return Ok(ExportResolution::Null);
                    }
                    _ => {
                        resolve_set.pop();
                        return Ok(ExportResolution::Null);
                    }
                };
                let result = resolve_local_export(runtime, module, &local_bytes);
                resolve_set.pop();
                return Ok(match result? {
                    Some(r) => ExportResolution::Resolved(r),
                    None => ExportResolution::Null,
                });
            }
            quickjs_frontend::ModuleExportEntryRole::Indirect => {
                let request_idx = match entry.request() {
                    Some(idx) => idx.as_usize() as u32,
                    None => {
                        resolve_set.pop();
                        return Ok(ExportResolution::Null);
                    }
                };
                let dep = resolve_request(runtime, module, request_idx)?;
                // `export * as name from "mod"`: the export resolves to the
                // dependency's namespace object (ECMA-262 ResolveExport,
                // indirect entry with [[ImportName]] ~all~).
                if matches!(
                    entry.import_name(),
                    quickjs_frontend::ModuleExportImportName::All
                ) {
                    resolve_set.pop();
                    return Ok(ExportResolution::Resolved(ResolvedExport::Namespace {
                        module: dep,
                    }));
                }
                let import_bytes = match entry.import_name() {
                    quickjs_frontend::ModuleExportImportName::Name(name) => {
                        units_to_utf8(name.code_units())
                    }
                    quickjs_frontend::ModuleExportImportName::Default(_) => b"default".to_vec(),
                    _ => {
                        resolve_set.pop();
                        return Ok(ExportResolution::Null);
                    }
                };
                let result = resolve_export(runtime, dep, &import_bytes, resolve_set)?;
                resolve_set.pop();
                return Ok(result);
            }
            quickjs_frontend::ModuleExportEntryRole::Star => continue,
            _ => continue,
        }
    }

    // Star re-exports (export * from) - excludes "default"
    if export_name != b"default" {
        let mut found: Option<ResolvedExport> = None;
        for entry in syntax.export_entries() {
            if entry.role() != quickjs_frontend::ModuleExportEntryRole::Star {
                continue;
            }
            let request_idx = match entry.request() {
                Some(idx) => idx.as_usize() as u32,
                None => continue,
            };
            let dep = resolve_request(runtime, module, request_idx)?;
            match resolve_export(runtime, dep, export_name, resolve_set)? {
                ExportResolution::Ambiguous => {
                    resolve_set.pop();
                    return Ok(ExportResolution::Ambiguous);
                }
                ExportResolution::Null => {}
                ExportResolution::Resolved(r) => match found {
                    None => found = Some(r),
                    // Two star paths reaching the *same* binding stay
                    // unambiguous (ECMA-262 ResolveExport, star-export
                    // resolution dedup): same module + binding name, or the
                    // same module's namespace.
                    Some(f) if same_resolved_binding(&f, &r) => {}
                    Some(_) => {
                        resolve_set.pop();
                        return Ok(ExportResolution::Ambiguous);
                    }
                },
            }
        }
        resolve_set.pop();
        return Ok(match found {
            Some(r) => ExportResolution::Resolved(r),
            None => ExportResolution::Null,
        });
    }

    resolve_set.pop();
    Ok(ExportResolution::Null)
}

fn same_resolved_binding(left: &ResolvedExport, right: &ResolvedExport) -> bool {
    match (left, right) {
        (
            ResolvedExport::Binding {
                module: left_module,
                cell: left_cell,
            },
            ResolvedExport::Binding {
                module: right_module,
                cell: right_cell,
            },
        ) => left_module == right_module && left_cell == right_cell,
        (
            ResolvedExport::Namespace {
                module: left_module,
            },
            ResolvedExport::Namespace {
                module: right_module,
            },
        ) => left_module == right_module,
        _ => false,
    }
}

fn resolve_local_export(
    runtime: &Runtime,
    module: ModuleRecordId,
    local_name: &[u8],
) -> Result<Option<ResolvedExport>, ModuleError> {
    let record = runtime.modules.get(module).expect("module exists");
    let decl = record.declaration_record().clone();
    let installed = runtime
        .code
        .get(record.installed_code.expect("code installed"))
        .expect("code exists");
    let root_idx = usize::try_from(record.authority.root_id().get()).unwrap();
    let template = &installed.templates[root_idx];
    for (i, binding) in decl.bindings().iter().enumerate() {
        let name_match = template
            .atoms
            .get(binding.name().get() as usize)
            .and_then(|a| a.description())
            .is_some_and(|name| {
                let units: Vec<u16> = name.code_units().collect();
                name_units_eq_utf8(&units, local_name)
            });
        if name_match {
            if let Some(&cell) = record.environment.get(i) {
                return Ok(Some(ResolvedExport::Binding { module, cell }));
            }
        }
    }
    Ok(None)
}

pub(crate) fn resolve_request(
    runtime: &Runtime,
    module: ModuleRecordId,
    request_index: u32,
) -> Result<ModuleRecordId, ModuleError> {
    let record = runtime.modules.get(module).expect("module exists");
    let request = record
        .syntax_record
        .requests()
        .get(request_index as usize)
        .ok_or_else(|| ModuleError::link("request index out of range"))?;
    let specifier = units_to_utf8(request.specifier().code_units());
    let specifier = String::from_utf8_lossy(&specifier);
    // HostResolveImportedModule: the host recorded the resolution edge for
    // this (referrer, specifier) pair at load time; a missing edge means the
    // host never resolved this request.
    let dep = record
        .resolved_dependencies
        .get(specifier.as_ref())
        .copied()
        .ok_or_else(|| {
            ModuleError::link(format!(
                "dependency '{specifier}' has no registered resolution edge"
            ))
        })?;
    Ok(dep)
}

/// Gets the module's resolved export names (for namespace objects).
pub(crate) fn module_export_names(
    runtime: &Runtime,
    module: ModuleRecordId,
) -> Result<Vec<(Vec<u8>, ResolvedExport)>, ModuleError> {
    let mut export_star_set = Vec::new();
    module_export_names_inner(runtime, module, &mut export_star_set)
}

/// ECMA-262 `GetExportedNames`: `export_star_set` makes star re-export cycles
/// (including a module star-exporting itself) contribute no names instead of
/// recursing without bound.
fn module_export_names_inner(
    runtime: &Runtime,
    module: ModuleRecordId,
    export_star_set: &mut Vec<ModuleRecordId>,
) -> Result<Vec<(Vec<u8>, ResolvedExport)>, ModuleError> {
    if export_star_set.contains(&module) {
        return Ok(Vec::new());
    }
    export_star_set.push(module);
    let syntax = runtime
        .modules
        .get(module)
        .expect("module exists")
        .syntax_record
        .clone();
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for entry in syntax.export_entries() {
        let export_name = match entry.export_name() {
            quickjs_frontend::ModuleExportName::Name(name) => units_to_utf8(name.code_units()),
            quickjs_frontend::ModuleExportName::Default(_) => b"default".to_vec(),
            quickjs_frontend::ModuleExportName::Null => continue,
            _ => continue,
        };
        if entry.role() == quickjs_frontend::ModuleExportEntryRole::Star {
            continue;
        }
        if !seen.insert(export_name.clone()) {
            continue;
        }
        // Resolve by the *export* name: a local entry's local binding name
        // (e.g. the synthetic `default` binding) is not itself an export.
        let mut rs = Vec::new();
        if let ExportResolution::Resolved(r) =
            resolve_export(runtime, module, &export_name, &mut rs)?
        {
            result.push((export_name, r));
        }
    }

    // Star re-exports (all except "default"): names that resolve to a single
    // binding through this module join the namespace; ambiguous and null
    // resolutions are omitted (ECMA-262 GetExportedNames star handling).
    let mut star_candidates = Vec::new();
    for entry in syntax.export_entries() {
        if entry.role() != quickjs_frontend::ModuleExportEntryRole::Star {
            continue;
        }
        let request_idx = match entry.request() {
            Some(idx) => idx.as_usize() as u32,
            None => continue,
        };
        let dep = resolve_request(runtime, module, request_idx)?;
        let dep_names = module_export_names_inner(runtime, dep, export_star_set)?;
        for (name, _) in dep_names {
            if name != b"default" {
                star_candidates.push(name);
            }
        }
    }
    for name in star_candidates {
        if !seen.insert(name.clone()) {
            continue;
        }
        let mut rs = Vec::new();
        if let ExportResolution::Resolved(r) = resolve_export(runtime, module, &name, &mut rs)? {
            result.push((name, r));
        }
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

/// Returns the dependencies of a module (for DFS traversal).
pub(crate) fn module_dependencies(
    runtime: &Runtime,
    module: ModuleRecordId,
) -> Vec<ModuleRecordId> {
    let record = match runtime.modules.get(module) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let syntax = record.syntax_record.clone();
    let mut deps = Vec::new();
    let mut seen = HashSet::new();
    for (i, _) in syntax.requests().iter().enumerate() {
        if let Ok(dep) = resolve_request(runtime, module, i as u32) {
            if seen.insert(dep) {
                deps.push(dep);
            }
        }
    }
    deps
}

// --- Import resolution (second linking phase) ---

/// Resolves and forwards every import binding of `module`.
///
/// Runs after every module environment in the graph exists. Named and default
/// imports forward their cell to the exporter's cell (this port's
/// representation of an indirect binding); namespace imports receive the
/// exporter's namespace object.
fn resolve_module_imports(
    runtime: &mut Runtime,
    module: ModuleRecordId,
) -> Result<(), ModuleError> {
    let decl = {
        let record = runtime.modules.get(module).expect("module exists");
        record
            .authority
            .module()
            .ok_or_else(|| ModuleError::link("module root carries no declaration record"))?
            .clone()
    };
    let cells = runtime
        .modules
        .get(module)
        .expect("module exists")
        .environment
        .clone();

    for (index, binding) in decl.bindings().iter().enumerate() {
        let Some(import) = binding.import() else {
            continue;
        };
        let cell = cells
            .get(index)
            .copied()
            .ok_or_else(|| ModuleError::link("module environment is missing a binding cell"))?;
        match binding.origin() {
            ModuleBindingOrigin::Import => {
                resolve_and_forward_import(runtime, module, import, cell)?;
            }
            ModuleBindingOrigin::Namespace => {
                create_namespace_import(runtime, module, import, cell)?;
            }
            ModuleBindingOrigin::Local => {}
        }
    }

    // ECMA-262 InitializeEnvironment validates every indirect export entry:
    // `export { x } from "mod"` is a link-time SyntaxError when `x` resolves
    // to null or ambiguous, even when nothing imports it.
    let syntax = runtime
        .modules
        .get(module)
        .expect("module exists")
        .syntax_record
        .clone();
    for entry in syntax.export_entries() {
        if entry.role() != quickjs_frontend::ModuleExportEntryRole::Indirect {
            continue;
        }
        let export_name = match entry.export_name() {
            quickjs_frontend::ModuleExportName::Name(name) => units_to_utf8(name.code_units()),
            quickjs_frontend::ModuleExportName::Default(_) => b"default".to_vec(),
            _ => continue,
        };
        let mut rs = Vec::new();
        match resolve_export(runtime, module, &export_name, &mut rs)? {
            ExportResolution::Resolved(_) => {}
            ExportResolution::Null => {
                return Err(ModuleError::link(format!(
                    "unresolved export '{}'",
                    String::from_utf8_lossy(&export_name)
                )));
            }
            ExportResolution::Ambiguous => {
                return Err(ModuleError::link(format!(
                    "ambiguous export '{}'",
                    String::from_utf8_lossy(&export_name)
                )));
            }
        }
    }
    Ok(())
}

fn initial_cell_value(binding: &quickjs_bytecode::ModuleBindingDescriptor) -> SlotValue {
    match binding.policy().initialization() {
        CompilerInitializationPolicy::UndefinedAtInstantiation => {
            SlotValue::Value(StoredValue::Undefined)
        }
        _ => SlotValue::Uninitialized,
    }
}

fn resolve_and_forward_import(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    import: &ModuleImportName,
    import_cell: BindingCellId,
) -> Result<(), ModuleError> {
    if import.is_namespace() {
        return Ok(()); // handled separately
    }
    let request_idx = import.request();
    let export_name = if import.is_default() {
        b"default".to_vec()
    } else if let Some(atom) = import.named_atom() {
        let record = runtime.modules.get(module).expect("module exists");
        let installed = runtime
            .code
            .get(record.installed_code.expect("code installed"))
            .expect("code exists");
        let root_idx = usize::try_from(record.authority.root_id().get()).unwrap();
        let template = &installed.templates[root_idx];
        let name = template
            .atoms
            .get(atom.get() as usize)
            .and_then(|a| a.description())
            .ok_or_else(|| ModuleError::link("import atom not found"))?;
        let units: Vec<u16> = name.code_units().collect();
        let local_name = units_to_utf8(&units);
        // The bytecode import descriptor carries the *local* binding atom (see
        // the compiler's module import lowering). For an aliased import
        // (`import { remote as local }`) the export name is ECMA-262
        // ImportEntry [[ImportName]], recovered from the frontend import entry.
        record
            .syntax_record
            .import_entries()
            .iter()
            .find(|entry| {
                entry.request().as_usize() as u32 == request_idx
                    && entry
                        .local_name()
                        .equals_utf8(&String::from_utf8_lossy(&local_name))
            })
            .map_or(local_name.clone(), |entry| match entry.import_name() {
                quickjs_frontend::ModuleImportName::Name(name) => units_to_utf8(name.code_units()),
                quickjs_frontend::ModuleImportName::Default(_) => b"default".to_vec(),
                _ => local_name.clone(),
            })
    } else {
        return Err(ModuleError::link("import has no name"));
    };

    let dep = resolve_request(runtime, module, request_idx)?;
    let mut rs = Vec::new();
    let resolved = match resolve_export(runtime, dep, &export_name, &mut rs)? {
        ExportResolution::Resolved(r) => r,
        ExportResolution::Null => {
            return Err(ModuleError::link(format!(
                "unresolved import '{}'",
                String::from_utf8_lossy(&export_name)
            )));
        }
        ExportResolution::Ambiguous => {
            return Err(ModuleError::link(format!(
                "ambiguous export '{}'",
                String::from_utf8_lossy(&export_name)
            )));
        }
    };

    match resolved {
        ResolvedExport::Binding { cell, .. } => {
            runtime
                .cells
                .get_mut(import_cell)
                .ok_or_else(|| ModuleError::link("import cell stale"))?
                .forward = Some(cell);
        }
        ResolvedExport::Namespace { module: dep } => {
            // `export * as name` re-exported and then imported by name: the
            // binding is initialized once with the dependency's namespace
            // object and is not live (ECMA-262 InitializeEnvironment,
            // namespace-object resolution).
            let namespace = get_or_create_namespace(runtime, dep)?;
            runtime
                .cells
                .get_mut(import_cell)
                .ok_or_else(|| ModuleError::link("import cell stale"))?
                .value = SlotValue::Value(StoredValue::Object(namespace));
            runtime.collection_pending = true;
        }
    }
    Ok(())
}

fn create_namespace_import(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    import: &ModuleImportName,
    cell: BindingCellId,
) -> Result<(), ModuleError> {
    let request_idx = import.request();
    let dep = resolve_request(runtime, module, request_idx)?;
    let ns = get_or_create_namespace_phase(runtime, dep, import.is_deferred_namespace())?;
    runtime
        .cells
        .get_mut(cell)
        .ok_or_else(|| ModuleError::link("namespace cell stale"))?
        .value = SlotValue::Value(StoredValue::Object(ns));
    runtime.collection_pending = true;
    Ok(())
}

pub(crate) fn get_or_create_namespace(
    runtime: &mut Runtime,
    module: ModuleRecordId,
) -> Result<crate::runtime::ObjectId, ModuleError> {
    get_or_create_namespace_phase(runtime, module, false)
}

/// `GetModuleNamespace(module, phase)` with a per-phase cache and phase-aware
/// `@@toStringTag`: deferred namespaces report `"Deferred Module"` and start
/// with `[[Deferred]]` set.
pub(crate) fn get_or_create_namespace_phase(
    runtime: &mut Runtime,
    module: ModuleRecordId,
    deferred: bool,
) -> Result<crate::runtime::ObjectId, ModuleError> {
    let cached = if deferred {
        runtime
            .modules
            .get(module)
            .and_then(|r| r.deferred_namespace)
    } else {
        runtime.modules.get(module).and_then(|r| r.namespace_object)
    };
    if let Some(ns) = cached {
        return Ok(ns);
    }
    let exports = module_export_names(runtime, module)?;
    let mut ns_exports: Vec<(Box<[u8]>, super::namespace::NamespaceExport)> =
        Vec::with_capacity(exports.len());
    let mut namespace_deps: Vec<ModuleRecordId> = Vec::new();
    for (name, resolution) in exports {
        let export = match resolution {
            ResolvedExport::Binding { module, cell } => {
                super::namespace::NamespaceExport::Binding { module, cell }
            }
            ResolvedExport::Namespace { module: dep } => {
                namespace_deps.push(dep);
                super::namespace::NamespaceExport::Namespace { module: dep }
            }
        };
        ns_exports.push((name.into_boxed_slice(), export));
    }

    let to_string_tag_key =
        runtime.predefined_symbol_property_key(crate::PredefinedAtom::SymbolToStringTag);
    let mut record = crate::object::ObjectRecord::empty(None);
    // The record carries one placeholder data property per export, in sorted
    // order, so the ordinary `[[OwnPropertyKeys]]` machinery reports exactly
    // the export names. Reads and writes are intercepted by the namespace
    // exotic branches before these placeholder values are observed, and the
    // non-configurable layout makes `delete` refuse them per ECMA-262 10.4.6.6.
    for (name, _) in &ns_exports {
        let name = crate::string::JsString::from_utf8(
            std::str::from_utf8(name).map_err(|_| ModuleError::link("export name is not UTF-8"))?,
        )
        .map_err(|_| ModuleError::link("export name string failed"))?;
        let key = runtime
            .property_key_from_string(&name)
            .map_err(|_| ModuleError::link("export name atom failed"))?;
        record
            .append_data(
                key,
                crate::PropertyLayout::data(true, true, false),
                StoredValue::Undefined,
            )
            .map_err(|_| ModuleError::link("namespace key shape failed"))?;
        runtime.object_properties = runtime.object_properties.saturating_add(1);
    }
    let _ = record.append_data(
        to_string_tag_key,
        crate::PropertyLayout::data(false, false, false),
        StoredValue::String(
            crate::string::JsString::from_utf8(if deferred {
                "Deferred Module"
            } else {
                "Module"
            })
            .map_err(|_| ModuleError::link("string creation failed"))?,
        ),
    );
    // A module namespace exotic object is always non-extensible (ECMA-262
    // 10.4.6.7/10.4.6.8); the ordinary machinery then reports [[IsExtensible]]
    // false, [[PreventExtensions]] true, and [[SetPrototypeOf]] rejected, and
    // new symbol properties are refused by [[DefineOwnProperty]].
    record.prevent_extensions();

    runtime
        .objects
        .try_reserve(1)
        .map_err(|_| ModuleError::link("namespace alloc failed"))?;

    let object = runtime
        .insert_heap_object(crate::object::HeapObject::module_namespace(
            record,
            super::namespace::ModuleNamespaceState {
                module,
                exports: ns_exports,
                deferred,
            },
        ))
        .map_err(|_| ModuleError::link("namespace insert failed"))?;

    if deferred {
        runtime
            .modules
            .get_mut(module)
            .expect("module exists")
            .deferred_namespace = Some(object);
    } else {
        runtime
            .modules
            .get_mut(module)
            .expect("module exists")
            .namespace_object = Some(object);
    }
    // Realize re-exported namespaces only after installing this one, so
    // self-references and namespace re-export cycles terminate against the
    // already-installed object (ECMA-262 GetModuleNamespace idempotence).
    for dep in namespace_deps {
        get_or_create_namespace(runtime, dep)?;
    }
    Ok(object)
}

// --- Helpers ---

fn module_status(runtime: &Runtime, module: ModuleRecordId) -> ModuleStatus {
    runtime
        .modules
        .get(module)
        .map(|r| r.status)
        .unwrap_or(ModuleStatus::New)
}

fn set_module_status(runtime: &mut Runtime, module: ModuleRecordId, status: ModuleStatus) {
    if let Some(r) = runtime.modules.get_mut(module) {
        r.status = status;
    }
}

fn module_dfs(runtime: &Runtime, module: ModuleRecordId) -> Option<u32> {
    runtime.modules.get(module).and_then(|r| r.dfs_index)
}

fn module_anc(runtime: &Runtime, module: ModuleRecordId) -> u32 {
    runtime
        .modules
        .get(module)
        .and_then(|r| r.dfs_ancestor_index)
        .unwrap_or(u32::MAX)
}

fn set_dfs(runtime: &mut Runtime, module: ModuleRecordId, idx: Option<u32>, anc: Option<u32>) {
    if let Some(r) = runtime.modules.get_mut(module) {
        r.dfs_index = idx;
        r.dfs_ancestor_index = anc;
    }
}

fn set_dfs_anc(runtime: &mut Runtime, module: ModuleRecordId, anc: u32) {
    if let Some(r) = runtime.modules.get_mut(module) {
        r.dfs_ancestor_index = Some(anc);
    }
}

fn name_units_eq_utf8(units: &[u16], utf8_bytes: &[u8]) -> bool {
    let s = match std::str::from_utf8(utf8_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };
    s.encode_utf16().collect::<Vec<u16>>().as_slice() == units
}

fn units_to_utf8(units: &[u16]) -> Vec<u8> {
    let mut s = String::new();
    for u in units.iter().copied().map(u32::from) {
        if let Some(c) = char::from_u32(u) {
            s.push(c);
        }
    }
    s.into_bytes()
}

fn collect_units(units: impl Iterator<Item = u16>) -> Vec<u16> {
    units.collect()
}
