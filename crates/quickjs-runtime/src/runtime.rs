/*
 * JavaScript runtime and closure ownership derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

use std::{
    collections::HashSet,
    sync::{Arc, Weak},
};

use quickjs_bytecode::{
    CompilerCapturedBinding, CompilerConstant, CompilerConstantValue, CompilerExecutableKind,
    FinalOpcode, FunctionTemplateId, VerifiedBytecode,
};

use crate::{
    Atom, AtomLimits, AtomTable, AtomUsage, DynamicFunctionScriptError, ExecutionLimits, Function,
    HandleError, HandleKind, InstallError, JsNumber, JsString, JsValue, PropertyKey,
    PropertyLayout, RuntimeError, RuntimeResource,
    arena::{Arena, RuntimeIdentity},
    ids::{BindingCellId, FunctionId, InstalledCodeId, ObjectId, RealmId},
    object::{HeapObject, ObjectRecord},
    value::{HeapReference, PrimitiveValue, ReleaseMailbox, RootTarget, SlotValue, StoredValue},
};

const DEFAULT_MAX_REALMS: u64 = 65_535;
const DEFAULT_MAX_INSTALLED_CODE: u64 = 65_535;
const DEFAULT_MAX_INSTALLED_TEMPLATES: u64 = 1_048_576;
const DEFAULT_MAX_INSTALLED_ATOMS: u64 = 1_048_576;
const DEFAULT_MAX_INSTALLED_CONSTANTS: u64 = 1_048_576;
const DEFAULT_MAX_HEAP_FUNCTIONS: u64 = 1_048_576;
const DEFAULT_MAX_HEAP_OBJECTS: u64 = 1_048_576;
const DEFAULT_MAX_OBJECT_PROPERTIES: u64 = 16_777_216;
const DEFAULT_MAX_BINDING_CELLS: u64 = 1_048_576;
const DEFAULT_MAX_PUBLIC_ROOTS: u64 = 1_048_576;
const DEFAULT_MAX_ACTIVE_FRAMES: u32 = 1_024;
const DEFAULT_MAX_ACTIVE_FRAME_VALUES: u64 = 16_777_216;

/// Inclusive logical ceilings for one JavaScript runtime.
///
/// Immutable string backing still follows Rust's global allocator policy. The
/// installed string and atom counts are enforced, but this initial VM does not
/// claim a complete byte-accurate heap limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeLimits {
    atom_limits: AtomLimits,
    max_realms: u64,
    max_installed_code: u64,
    max_installed_templates: u64,
    max_installed_atoms: u64,
    max_installed_constants: u64,
    pub(crate) max_heap_functions: u64,
    pub(crate) max_heap_objects: u64,
    pub(crate) max_object_properties: u64,
    pub(crate) max_binding_cells: u64,
    max_public_roots: u64,
    pub(crate) max_active_frames: u32,
    pub(crate) max_active_frame_values: u64,
}

impl RuntimeLimits {
    /// Replaces the runtime-local atom-table ceilings.
    #[must_use]
    pub const fn with_atom_limits(mut self, atom_limits: AtomLimits) -> Self {
        self.atom_limits = atom_limits;
        self
    }

    /// Replaces the maximum live realm count.
    #[must_use]
    pub const fn with_max_realms(mut self, maximum: u64) -> Self {
        self.max_realms = maximum;
        self
    }

    /// Replaces the maximum installed verified-code instance count.
    #[must_use]
    pub const fn with_max_installed_code(mut self, maximum: u64) -> Self {
        self.max_installed_code = maximum;
        self
    }

    /// Replaces the maximum aggregate installed template count.
    #[must_use]
    pub const fn with_max_installed_templates(mut self, maximum: u64) -> Self {
        self.max_installed_templates = maximum;
        self
    }

    /// Replaces the maximum aggregate installed atom count.
    #[must_use]
    pub const fn with_max_installed_atoms(mut self, maximum: u64) -> Self {
        self.max_installed_atoms = maximum;
        self
    }

    /// Replaces the maximum aggregate installed constant count.
    #[must_use]
    pub const fn with_max_installed_constants(mut self, maximum: u64) -> Self {
        self.max_installed_constants = maximum;
        self
    }

    /// Replaces the maximum live function-object count.
    #[must_use]
    pub const fn with_max_heap_functions(mut self, maximum: u64) -> Self {
        self.max_heap_functions = maximum;
        self
    }

    /// Replaces the maximum live ordinary-object count.
    #[must_use]
    pub const fn with_max_heap_objects(mut self, maximum: u64) -> Self {
        self.max_heap_objects = maximum;
        self
    }

    /// Replaces the maximum aggregate own-property slot count.
    #[must_use]
    pub const fn with_max_object_properties(mut self, maximum: u64) -> Self {
        self.max_object_properties = maximum;
        self
    }

    /// Replaces the maximum live binding-cell count.
    #[must_use]
    pub const fn with_max_binding_cells(mut self, maximum: u64) -> Self {
        self.max_binding_cells = maximum;
        self
    }

    /// Replaces the maximum public function/object-root count.
    #[must_use]
    pub const fn with_max_public_roots(mut self, maximum: u64) -> Self {
        self.max_public_roots = maximum;
        self
    }

    /// Replaces the maximum active interpreter-frame count.
    #[must_use]
    pub const fn with_max_active_frames(mut self, maximum: u32) -> Self {
        self.max_active_frames = maximum;
        self
    }

    /// Replaces the maximum values reserved by active frames.
    #[must_use]
    pub const fn with_max_active_frame_values(mut self, maximum: u64) -> Self {
        self.max_active_frame_values = maximum;
        self
    }
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            atom_limits: AtomLimits::default(),
            max_realms: DEFAULT_MAX_REALMS,
            max_installed_code: DEFAULT_MAX_INSTALLED_CODE,
            max_installed_templates: DEFAULT_MAX_INSTALLED_TEMPLATES,
            max_installed_atoms: DEFAULT_MAX_INSTALLED_ATOMS,
            max_installed_constants: DEFAULT_MAX_INSTALLED_CONSTANTS,
            max_heap_functions: DEFAULT_MAX_HEAP_FUNCTIONS,
            max_heap_objects: DEFAULT_MAX_HEAP_OBJECTS,
            max_object_properties: DEFAULT_MAX_OBJECT_PROPERTIES,
            max_binding_cells: DEFAULT_MAX_BINDING_CELLS,
            max_public_roots: DEFAULT_MAX_PUBLIC_ROOTS,
            max_active_frames: DEFAULT_MAX_ACTIVE_FRAMES,
            max_active_frame_values: DEFAULT_MAX_ACTIVE_FRAME_VALUES,
        }
    }
}

/// Snapshot of logical runtime usage.
///
/// Charged counts include releases queued by dropped function handles until
/// the next mutable safe point drains `pending_releases`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeUsage {
    realms: u64,
    installed_code: u64,
    installed_templates: u64,
    installed_atoms: u64,
    installed_constants: u64,
    heap_functions: u64,
    heap_objects: u64,
    object_properties: u64,
    binding_cells: u64,
    public_roots: u64,
    pending_releases: u64,
}

impl RuntimeUsage {
    /// Returns the number of live realm records.
    #[must_use]
    pub const fn realms(self) -> u64 {
        self.realms
    }

    /// Returns the number of installed verified-code instances.
    #[must_use]
    pub const fn installed_code(self) -> u64 {
        self.installed_code
    }

    /// Returns the number of installed function templates.
    #[must_use]
    pub const fn installed_templates(self) -> u64 {
        self.installed_templates
    }

    /// Returns the number of installed function-local atoms.
    #[must_use]
    pub const fn installed_atoms(self) -> u64 {
        self.installed_atoms
    }

    /// Returns the number of installed constants.
    #[must_use]
    pub const fn installed_constants(self) -> u64 {
        self.installed_constants
    }

    /// Returns the number of live runtime function objects.
    #[must_use]
    pub const fn heap_functions(self) -> u64 {
        self.heap_functions
    }

    /// Returns the number of live ordinary objects, including realm roots.
    #[must_use]
    pub const fn heap_objects(self) -> u64 {
        self.heap_objects
    }

    /// Returns the aggregate own-property slot count.
    #[must_use]
    pub const fn object_properties(self) -> u64 {
        self.object_properties
    }

    /// Returns the number of live captured-binding cells.
    #[must_use]
    pub const fn binding_cells(self) -> u64 {
        self.binding_cells
    }

    /// Returns charged public function/object roots, including queued
    /// undrained releases.
    #[must_use]
    pub const fn public_roots(self) -> u64 {
        self.public_roots
    }

    /// Returns deferred releases awaiting the next mutable runtime boundary.
    #[must_use]
    pub const fn pending_releases(self) -> u64 {
        self.pending_releases
    }
}

struct RealmState {
    object_prototype: ObjectId,
    global_object: ObjectId,
}

struct RealmHandle {
    owner: Weak<ReleaseMailbox>,
    id: RealmId,
}

/// A cloned handle to one runtime-local realm.
///
/// Realm state stays in the uniquely owned `Runtime`; only this immutable
/// identity header uses [`Arc`].
///
/// ```compile_fail
/// use quickjs_runtime::Realm;
///
/// fn require_send<T: Send>() {}
/// require_send::<Realm>();
/// ```
#[derive(Clone)]
pub struct Realm(Arc<RealmHandle>);

impl std::fmt::Debug for Realm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Realm")
            .field("index", &self.0.id.index())
            .field("generation", &self.0.id.generation())
            .field("orphaned", &self.0.owner.upgrade().is_none())
            .finish()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum FrameBindingAddress {
    Argument(u32),
    Local(u32),
}

pub(crate) enum InstalledConstant {
    Number(JsNumber),
    String(JsString),
    Function(FunctionTemplateId),
}

pub(crate) struct InstalledTemplate {
    pub(crate) atoms: Vec<Atom>,
    pub(crate) constants: Vec<InstalledConstant>,
    pub(crate) own_cell_bindings: Vec<FrameBindingAddress>,
}

pub(crate) struct InstalledCode {
    pub(crate) authority: Arc<VerifiedBytecode>,
    pub(crate) realm: RealmId,
    pub(crate) templates: Vec<InstalledTemplate>,
    pub(crate) live_functions: u64,
}

pub(crate) struct HeapFunction {
    pub(crate) code: InstalledCodeId,
    pub(crate) template: FunctionTemplateId,
    pub(crate) environment: Vec<BindingCellId>,
    pub(crate) object: ObjectRecord,
    pub(crate) public_roots: u32,
}

pub(crate) struct BindingCell {
    pub(crate) value: SlotValue,
}

/// One uniquely owned JavaScript runtime.
///
/// Mutable heap state is direct and lock-free. `Arc` backs immutable
/// bytecode/string storage plus runtime-local atom, public-handle, and mailbox
/// identity owners. Their accounting uses `Cell`, so the runtime and every
/// heap-bound handle are deliberately `!Send + !Sync`.
///
/// ```compile_fail
/// use quickjs_runtime::Runtime;
///
/// fn require_sync<T: Sync>() {}
/// require_sync::<Runtime>();
/// ```
pub struct Runtime {
    pub(crate) mailbox: Arc<ReleaseMailbox>,
    atoms: AtomTable,
    realms: Arena<crate::ids::RealmMarker, RealmState>,
    pub(crate) code: Arena<crate::ids::InstalledCodeMarker, InstalledCode>,
    pub(crate) functions: Arena<crate::ids::FunctionMarker, HeapFunction>,
    pub(crate) objects: Arena<crate::ids::ObjectMarker, HeapObject>,
    pub(crate) cells: Arena<crate::ids::BindingCellMarker, BindingCell>,
    pub(crate) limits: RuntimeLimits,
    installed_templates: u64,
    installed_atoms: u64,
    installed_constants: u64,
    object_properties: u64,
    public_roots: u64,
    pub(crate) collection_pending: bool,
}

impl Runtime {
    /// Creates one bounded runtime and its predefined atom table.
    ///
    /// # Errors
    ///
    /// Returns a structured atom-table configuration or allocation error.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "Arc ownership is user-selected while Cell deliberately keeps this runtime local"
    )]
    pub fn try_new(limits: RuntimeLimits) -> Result<Self, RuntimeError> {
        let atoms = AtomTable::try_new(limits.atom_limits)?;
        let mailbox = Arc::new(ReleaseMailbox::new());
        let runtime_identity =
            RuntimeIdentity::from_address(Arc::as_ptr(&mailbox).cast::<()>() as usize);
        Ok(Self {
            mailbox,
            atoms,
            realms: Arena::new(runtime_identity),
            code: Arena::new(runtime_identity),
            functions: Arena::new(runtime_identity),
            objects: Arena::new(runtime_identity),
            cells: Arena::new(runtime_identity),
            limits,
            installed_templates: 0,
            installed_atoms: 0,
            installed_constants: 0,
            object_properties: 0,
            public_roots: 0,
            collection_pending: false,
        })
    }

    /// Creates a realm owned by this runtime.
    ///
    /// # Errors
    ///
    /// Returns a limit or recoverable allocation failure.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "public handles use Arc headers but remain runtime-local through their mailbox"
    )]
    pub fn create_realm(&mut self) -> Result<Realm, RuntimeError> {
        self.drain_releases();
        check_limit(
            RuntimeResource::Realms,
            self.limits.max_realms,
            usize_to_u64(self.realms.len()).saturating_add(1),
        )?;
        check_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(2),
        )?;
        self.realms
            .try_reserve(1)
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Realms,
                additional: 1,
            })?;
        self.objects
            .try_reserve(2)
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 2,
            })?;
        let object_prototype = self
            .objects
            .try_insert(HeapObject {
                record: ObjectRecord::empty(None),
                public_roots: 0,
            })
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let Ok(global_object) = self.objects.try_insert(HeapObject {
            record: ObjectRecord::empty(Some(HeapReference::Object(object_prototype))),
            public_roots: 0,
        }) else {
            let removed = self.objects.remove(object_prototype);
            debug_assert!(removed.is_some());
            return Err(RuntimeError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        let Ok(id) = self.realms.try_insert(RealmState {
            object_prototype,
            global_object,
        }) else {
            let removed = self.objects.remove(global_object);
            debug_assert!(removed.is_some());
            let removed = self.objects.remove(object_prototype);
            debug_assert!(removed.is_some());
            return Err(RuntimeError::AllocationFailed {
                resource: RuntimeResource::Realms,
                additional: 1,
            });
        };
        Ok(Realm(Arc::new(RealmHandle {
            owner: Arc::downgrade(&self.mailbox),
            id,
        })))
    }

    /// Borrows an exclusive execution context for one same-runtime realm.
    ///
    /// # Errors
    ///
    /// Rejects orphaned, foreign, or stale realm handles.
    pub fn context(&mut self, realm: &Realm) -> Result<Context<'_>, HandleError> {
        self.drain_releases();
        let Some(owner) = realm.0.owner.upgrade() else {
            return Err(HandleError::Orphaned {
                kind: HandleKind::Realm,
            });
        };
        if !Arc::ptr_eq(&owner, &self.mailbox) {
            return Err(HandleError::ForeignRuntime {
                kind: HandleKind::Realm,
            });
        }
        if !self.realms.contains(realm.0.id) {
            return Err(HandleError::Stale {
                kind: HandleKind::Realm,
                index: realm.0.id.index(),
                generation: realm.0.id.generation(),
            });
        }
        Ok(Context {
            runtime: self,
            realm: realm.0.id,
        })
    }

    /// Returns current logical resource usage.
    #[must_use]
    pub fn usage(&self) -> RuntimeUsage {
        RuntimeUsage {
            realms: usize_to_u64(self.realms.len()),
            installed_code: usize_to_u64(self.code.len()),
            installed_templates: self.installed_templates,
            installed_atoms: self.installed_atoms,
            installed_constants: self.installed_constants,
            heap_functions: usize_to_u64(self.functions.len()),
            heap_objects: usize_to_u64(self.objects.len()),
            object_properties: self.object_properties,
            binding_cells: usize_to_u64(self.cells.len()),
            public_roots: self.public_roots,
            pending_releases: usize_to_u64(self.mailbox.pending_len()),
        }
    }

    /// Returns exact runtime-local atom-table usage.
    ///
    /// Dead weak interner slots are included until a mutable runtime boundary
    /// or explicit cycle collection removes them.
    #[must_use]
    pub fn atom_usage(&self) -> AtomUsage {
        self.atoms.usage()
    }

    /// Drains dropped public roots and traces the runtime-local object,
    /// function, and binding-cell graph from public and realm-owned roots.
    ///
    /// The traversal and dead-set reclamation are iterative. Runtime function
    /// heap nodes and binding cells never use `Arc`, so property, prototype,
    /// and closure cycles are reclaimable.
    ///
    /// # Errors
    ///
    /// Returns a recoverable scratch-allocation failure.
    #[allow(
        clippy::too_many_lines,
        reason = "the mark and two-phase dead-set transaction remains together for auditability"
    )]
    pub fn collect_cycles(&mut self) -> Result<CollectionReport, RuntimeError> {
        self.drain_releases();

        let mut marked_functions = HashSet::new();
        marked_functions
            .try_reserve(self.functions.len())
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: self.functions.len(),
            })?;
        let mut marked_objects = HashSet::new();
        marked_objects
            .try_reserve(self.objects.len())
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: self.objects.len(),
            })?;
        let mut marked_cells = HashSet::new();
        marked_cells
            .try_reserve(self.cells.len())
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: self.cells.len(),
            })?;
        let mut work = Vec::new();
        let graph_nodes = self
            .functions
            .len()
            .saturating_add(self.objects.len())
            .saturating_add(self.cells.len());
        work.try_reserve(graph_nodes)
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: graph_nodes,
            })?;

        for (id, function) in self.functions.iter() {
            if function.public_roots > 0 {
                mark_heap_reference(
                    HeapReference::Function(id),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
            }
        }
        for (id, object) in self.objects.iter() {
            if object.public_roots > 0 {
                mark_heap_reference(
                    HeapReference::Object(id),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
            }
        }
        for (_, realm) in self.realms.iter() {
            mark_heap_reference(
                HeapReference::Object(realm.object_prototype),
                &mut marked_functions,
                &mut marked_objects,
                &mut work,
            );
            mark_heap_reference(
                HeapReference::Object(realm.global_object),
                &mut marked_functions,
                &mut marked_objects,
                &mut work,
            );
        }

        while let Some(node) = work.pop() {
            match node {
                GraphNode::Function(id) => {
                    if let Some(function) = self.functions.get(id) {
                        for cell in function.environment.iter().copied() {
                            if marked_cells.insert(cell) {
                                work.push(GraphNode::Cell(cell));
                            }
                        }
                        mark_object_record(
                            &function.object,
                            &mut marked_functions,
                            &mut marked_objects,
                            &mut work,
                        );
                    }
                }
                GraphNode::Object(id) => {
                    if let Some(object) = self.objects.get(id) {
                        mark_object_record(
                            &object.record,
                            &mut marked_functions,
                            &mut marked_objects,
                            &mut work,
                        );
                    }
                }
                GraphNode::Cell(id) => {
                    if let Some(cell) = self.cells.get(id) {
                        match &cell.value {
                            SlotValue::Uninitialized => {}
                            SlotValue::Value(value) => mark_stored_value(
                                value,
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            ),
                        }
                    }
                }
            }
        }

        let mut dead_functions = Vec::new();
        dead_functions
            .try_reserve(self.functions.len().saturating_sub(marked_functions.len()))
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: self.functions.len().saturating_sub(marked_functions.len()),
            })?;
        dead_functions.extend(
            self.functions
                .iter()
                .map(|(id, _)| id)
                .filter(|id| !marked_functions.contains(id)),
        );

        let mut dead_objects = Vec::new();
        dead_objects
            .try_reserve(self.objects.len().saturating_sub(marked_objects.len()))
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: self.objects.len().saturating_sub(marked_objects.len()),
            })?;
        dead_objects.extend(
            self.objects
                .iter()
                .map(|(id, _)| id)
                .filter(|id| !marked_objects.contains(id)),
        );

        let mut dead_cells = Vec::new();
        dead_cells
            .try_reserve(self.cells.len().saturating_sub(marked_cells.len()))
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: self.cells.len().saturating_sub(marked_cells.len()),
            })?;
        dead_cells.extend(
            self.cells
                .iter()
                .map(|(id, _)| id)
                .filter(|id| !marked_cells.contains(id)),
        );

        let functions = dead_functions.len();
        let objects = dead_objects.len();
        let cells = dead_cells.len();
        let mut maybe_dead_code = Vec::new();
        maybe_dead_code
            .try_reserve(dead_functions.len())
            .map_err(|_| RuntimeError::AllocationFailed {
                resource: RuntimeResource::Collection,
                additional: dead_functions.len(),
            })?;
        for id in dead_functions {
            let removed = self.functions.remove(id);
            if let Some(function) = removed {
                self.object_properties = self
                    .object_properties
                    .saturating_sub(usize_to_u64(function.object.property_count()));
                if let Some(code) = self.code.get_mut(function.code) {
                    debug_assert!(code.live_functions > 0);
                    code.live_functions = code.live_functions.saturating_sub(1);
                    if code.live_functions == 0 {
                        maybe_dead_code.push(function.code);
                    }
                }
            }
        }
        for id in dead_objects {
            if let Some(object) = self.objects.remove(id) {
                self.object_properties = self
                    .object_properties
                    .saturating_sub(usize_to_u64(object.record.property_count()));
            }
        }
        for id in dead_cells {
            let removed = self.cells.remove(id);
            debug_assert!(removed.is_some());
        }
        maybe_dead_code.sort_unstable();
        maybe_dead_code.dedup();
        for id in maybe_dead_code {
            let remove = self
                .code
                .get(id)
                .is_some_and(|code| code.live_functions == 0);
            if !remove {
                continue;
            }
            if let Some(code) = self.code.remove(id) {
                self.installed_templates = self
                    .installed_templates
                    .saturating_sub(usize_to_u64(code.templates.len()));
                let atoms = code.templates.iter().fold(0_u64, |total, template| {
                    total.saturating_add(usize_to_u64(template.atoms.len()))
                });
                let constants = code.templates.iter().fold(0_u64, |total, template| {
                    total.saturating_add(usize_to_u64(template.constants.len()))
                });
                self.installed_atoms = self.installed_atoms.saturating_sub(atoms);
                self.installed_constants = self.installed_constants.saturating_sub(constants);
            }
        }
        self.atoms.collect_dead();
        self.collection_pending = false;

        Ok(CollectionReport {
            functions: usize_to_u64(functions),
            objects: usize_to_u64(objects),
            binding_cells: usize_to_u64(cells),
        })
    }

    pub(crate) fn validate_owner(
        &self,
        owner: &Arc<ReleaseMailbox>,
        kind: HandleKind,
    ) -> Result<(), HandleError> {
        if Arc::ptr_eq(owner, &self.mailbox) {
            Ok(())
        } else {
            Err(HandleError::ForeignRuntime { kind })
        }
    }

    pub(crate) fn contains_realm(&self, realm: RealmId) -> bool {
        self.realms.contains(realm)
    }

    pub(crate) fn heap_reference_is_live(&self, reference: HeapReference) -> bool {
        match reference {
            HeapReference::Function(function) => self.functions.contains(function),
            HeapReference::Object(object) => self.objects.contains(object),
        }
    }

    pub(crate) fn realm_object_prototype(
        &self,
        realm: RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        self.realms
            .get(realm)
            .map(|state| state.object_prototype)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })
    }

    pub(crate) fn realm_global_object(
        &self,
        realm: RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        self.realms
            .get(realm)
            .map(|state| state.global_object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })
    }

    pub(crate) fn object_record(
        &self,
        reference: HeapReference,
    ) -> Result<&ObjectRecord, crate::EngineFault> {
        match reference {
            HeapReference::Function(function) => self
                .functions
                .get(function)
                .map(|function| &function.object)
                .ok_or_else(|| stale_heap_reference(reference)),
            HeapReference::Object(object) => self
                .objects
                .get(object)
                .map(|object| &object.record)
                .ok_or_else(|| stale_heap_reference(reference)),
        }
    }

    pub(crate) fn object_record_mut(
        &mut self,
        reference: HeapReference,
    ) -> Result<&mut ObjectRecord, crate::EngineFault> {
        match reference {
            HeapReference::Function(function) => self
                .functions
                .get_mut(function)
                .map(|function| &mut function.object)
                .ok_or_else(|| stale_heap_reference(reference)),
            HeapReference::Object(object) => self
                .objects
                .get_mut(object)
                .map(|object| &mut object.record)
                .ok_or_else(|| stale_heap_reference(reference)),
        }
    }

    pub(crate) fn allocate_ordinary_object(
        &mut self,
        prototype: ObjectId,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.objects.contains(prototype) {
            return Err(stale_heap_reference(HeapReference::Object(prototype)).into());
        }
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .objects
            .try_insert(HeapObject {
                record: ObjectRecord::empty(Some(HeapReference::Object(prototype))),
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn append_data_property(
        &mut self,
        reference: HeapReference,
        key: PropertyKey,
        layout: PropertyLayout,
        value: StoredValue,
    ) -> Result<(), crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(1),
        )?;
        self.object_record_mut(reference)?
            .append_data(key, layout, value)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        self.object_properties += 1;
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn prepare_execution_safe_point(&mut self) -> Result<(), crate::ExecutionError> {
        self.collect_if_pending().map_err(|error| match error {
            RuntimeError::LimitExceeded {
                resource,
                limit,
                observed,
            } => crate::ExecutionError::LimitExceeded {
                resource,
                limit,
                observed,
            },
            RuntimeError::AllocationFailed {
                resource,
                additional,
            } => crate::ExecutionError::AllocationFailed {
                resource,
                additional,
            },
            RuntimeError::Atom(_) => crate::EngineFault::RuntimeInvariant {
                message: "cycle collection returned an atom-table construction error",
            }
            .into(),
        })
    }

    fn prepare_installation_safe_point(&mut self) -> Result<(), InstallError> {
        self.collect_if_pending().map_err(|error| match error {
            RuntimeError::LimitExceeded {
                resource,
                limit,
                observed,
            } => InstallError::LimitExceeded {
                resource,
                limit,
                observed,
            },
            RuntimeError::AllocationFailed {
                resource,
                additional,
            } => InstallError::AllocationFailed {
                resource,
                additional,
            },
            RuntimeError::Atom(source) => InstallError::Atom(source),
        })
    }

    fn collect_if_pending(&mut self) -> Result<(), RuntimeError> {
        self.drain_releases();
        if self.collection_pending {
            self.collect_cycles()?;
        }
        Ok(())
    }

    pub(crate) fn public_value(
        &mut self,
        value: StoredValue,
    ) -> Result<JsValue, crate::ExecutionError> {
        let reference = match value.into_root_target() {
            RootTarget::Primitive(value) => return Ok(JsValue::primitive(&self.mailbox, value)),
            RootTarget::Heap(reference) => reference,
        };
        check_execution_limit(
            RuntimeResource::PublicRoots,
            self.limits.max_public_roots,
            self.public_roots.saturating_add(1),
        )?;
        self.mailbox
            .try_reserve_root()
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ReleaseMailbox,
                additional: 1,
            })?;
        let public_roots = match reference {
            HeapReference::Function(function) => self
                .functions
                .get_mut(function)
                .map(|node| &mut node.public_roots),
            HeapReference::Object(object) => self
                .objects
                .get_mut(object)
                .map(|node| &mut node.public_roots),
        };
        let Some(public_roots) = public_roots else {
            self.mailbox.cancel_reserved_root();
            return Err(stale_heap_reference(reference).into());
        };
        let Some(next_roots) = public_roots.checked_add(1) else {
            self.mailbox.cancel_reserved_root();
            return Err(crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::PublicRoots,
                limit: u64::from(u32::MAX),
                observed: u64::from(u32::MAX) + 1,
            });
        };
        *public_roots = next_roots;
        self.public_roots += 1;
        Ok(JsValue::rooted_heap(&self.mailbox, reference))
    }

    fn retire_internal_root(
        &mut self,
        root: FunctionId,
        expected_code: InstalledCodeId,
    ) -> Result<(), crate::EngineFault> {
        let function = self
            .functions
            .get(root)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "internal Script root",
                index: root.index(),
                generation: root.generation(),
            })?;
        if function.code != expected_code {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "internal Script root changed installed-code ownership",
            });
        }
        if function.public_roots != 0 {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "internal Script root became publicly rooted",
            });
        }
        let code = self
            .code
            .get(expected_code)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "installed code",
                index: expected_code.index(),
                generation: expected_code.generation(),
            })?;
        if code.live_functions == 0 {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "internal Script root has no installed-code live-function charge",
            });
        }

        let function = self
            .functions
            .remove(root)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "internal Script root",
                index: root.index(),
                generation: root.generation(),
            })?;
        self.object_properties = self
            .object_properties
            .saturating_sub(usize_to_u64(function.object.property_count()));
        let remove_code = {
            let code =
                self.code
                    .get_mut(expected_code)
                    .ok_or(crate::EngineFault::StaleHeapEdge {
                        edge: "installed code",
                        index: expected_code.index(),
                        generation: expected_code.generation(),
                    })?;
            code.live_functions -= 1;
            code.live_functions == 0
        };
        if remove_code {
            let removed = self.remove_installed_code(expected_code);
            debug_assert!(removed);
            self.atoms.collect_dead();
        }
        self.collection_pending = true;
        Ok(())
    }

    fn remove_installed_code(&mut self, id: InstalledCodeId) -> bool {
        let Some(code) = self.code.remove(id) else {
            return false;
        };
        self.installed_templates = self
            .installed_templates
            .saturating_sub(usize_to_u64(code.templates.len()));
        let atoms = code.templates.iter().fold(0_u64, |total, template| {
            total.saturating_add(usize_to_u64(template.atoms.len()))
        });
        let constants = code.templates.iter().fold(0_u64, |total, template| {
            total.saturating_add(usize_to_u64(template.constants.len()))
        });
        self.installed_atoms = self.installed_atoms.saturating_sub(atoms);
        self.installed_constants = self.installed_constants.saturating_sub(constants);
        true
    }

    pub(crate) fn drain_releases(&mut self) {
        let pending = self.mailbox.take_pending();
        if !pending.is_empty() {
            self.collection_pending = true;
        }
        for reference in pending.iter().copied() {
            match reference {
                HeapReference::Function(function) => {
                    if let Some(node) = self.functions.get_mut(function) {
                        debug_assert!(node.public_roots > 0);
                        node.public_roots = node.public_roots.saturating_sub(1);
                        self.public_roots = self.public_roots.saturating_sub(1);
                    }
                }
                HeapReference::Object(object) => {
                    if let Some(node) = self.objects.get_mut(object) {
                        debug_assert!(node.public_roots > 0);
                        node.public_roots = node.public_roots.saturating_sub(1);
                        self.public_roots = self.public_roots.saturating_sub(1);
                    }
                }
            }
        }
        self.mailbox.restore_pending(pending);
    }

    fn stage_templates(
        &mut self,
        authority: &VerifiedBytecode,
    ) -> Result<Vec<InstalledTemplate>, InstallError> {
        let function_count = authority.functions().len();
        let mut templates = Vec::new();
        templates.try_reserve_exact(function_count).map_err(|_| {
            InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledTemplates,
                additional: function_count,
            }
        })?;

        for function in authority.functions() {
            let mut atoms = Vec::new();
            atoms
                .try_reserve_exact(function.function().atoms().len())
                .map_err(|_| InstallError::AllocationFailed {
                    resource: RuntimeResource::InstalledAtoms,
                    additional: function.function().atoms().len(),
                })?;
            for atom in function.function().atoms() {
                let string = runtime_string(atom.string())?;
                atoms.push(self.atoms.intern_string(&string)?);
            }

            let mut constants = Vec::new();
            constants
                .try_reserve_exact(function.function().constants().len())
                .map_err(|_| InstallError::AllocationFailed {
                    resource: RuntimeResource::InstalledConstants,
                    additional: function.function().constants().len(),
                })?;
            for constant in function.function().constants() {
                constants.push(match constant {
                    CompilerConstant::Value(CompilerConstantValue::Number(value)) => {
                        InstalledConstant::Number(JsNumber::from_f64(value.to_f64()))
                    }
                    CompilerConstant::Value(CompilerConstantValue::String(value)) => {
                        InstalledConstant::String(runtime_string(value)?)
                    }
                    CompilerConstant::Function(function) => InstalledConstant::Function(*function),
                });
            }

            let capture_layout = function.function().control_flow().compiler_capture_layout();
            let capture_count = function
                .function()
                .control_flow()
                .function_header()
                .variable_reference_count();
            let bindings =
                if capture_count == 0 {
                    Vec::new()
                } else {
                    let layout = capture_layout.ok_or(InstallError::AuthorityInvariant {
                        message: "captured bindings have no compiler capture layout",
                    })?;
                    let mut bindings = Vec::new();
                    bindings
                        .try_reserve_exact(layout.bindings().len())
                        .map_err(|_| InstallError::AllocationFailed {
                            resource: RuntimeResource::InstalledTemplates,
                            additional: layout.bindings().len(),
                        })?;
                    bindings.extend(layout.bindings().iter().copied().map(
                        |binding| match binding {
                            CompilerCapturedBinding::Argument(index) => {
                                FrameBindingAddress::Argument(index)
                            }
                            CompilerCapturedBinding::FunctionLocal(index)
                            | CompilerCapturedBinding::ScopedLocal(index) => {
                                FrameBindingAddress::Local(index)
                            }
                        },
                    ));
                    bindings
                };
            if bindings.len() != capture_count as usize {
                return Err(InstallError::AuthorityInvariant {
                    message: "capture layout length differs from the verified header",
                });
            }

            templates.push(InstalledTemplate {
                atoms,
                constants,
                own_cell_bindings: bindings,
            });
        }
        Ok(templates)
    }
}

enum GraphNode {
    Function(FunctionId),
    Object(ObjectId),
    Cell(BindingCellId),
}

fn mark_heap_reference(
    reference: HeapReference,
    marked_functions: &mut HashSet<FunctionId>,
    marked_objects: &mut HashSet<ObjectId>,
    work: &mut Vec<GraphNode>,
) {
    match reference {
        HeapReference::Function(function) => {
            if marked_functions.insert(function) {
                work.push(GraphNode::Function(function));
            }
        }
        HeapReference::Object(object) => {
            if marked_objects.insert(object) {
                work.push(GraphNode::Object(object));
            }
        }
    }
}

fn mark_stored_value(
    value: &StoredValue,
    marked_functions: &mut HashSet<FunctionId>,
    marked_objects: &mut HashSet<ObjectId>,
    work: &mut Vec<GraphNode>,
) {
    if let Some(reference) = value.heap_reference() {
        mark_heap_reference(reference, marked_functions, marked_objects, work);
    }
}

fn mark_object_record(
    record: &ObjectRecord,
    marked_functions: &mut HashSet<FunctionId>,
    marked_objects: &mut HashSet<ObjectId>,
    work: &mut Vec<GraphNode>,
) {
    if let Some(prototype) = record.prototype() {
        mark_heap_reference(prototype, marked_functions, marked_objects, work);
    }
    for value in record.values() {
        mark_stored_value(value, marked_functions, marked_objects, work);
    }
}

/// Counts reclaimed by one cycle-collection pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CollectionReport {
    functions: u64,
    objects: u64,
    binding_cells: u64,
}

impl CollectionReport {
    /// Returns reclaimed function objects.
    #[must_use]
    pub const fn functions(self) -> u64 {
        self.functions
    }

    /// Returns reclaimed ordinary objects.
    #[must_use]
    pub const fn objects(self) -> u64 {
        self.objects
    }

    /// Returns reclaimed binding cells.
    #[must_use]
    pub const fn binding_cells(self) -> u64 {
        self.binding_cells
    }
}

/// An exclusive runtime mutator bound to one active realm.
///
/// ```compile_fail
/// use quickjs_runtime::Context;
///
/// fn require_send<T: Send>(_: T) {}
/// fn context_is_runtime_local(context: Context<'_>) {
///     require_send(context);
/// }
/// ```
pub struct Context<'runtime> {
    pub(crate) runtime: &'runtime mut Runtime,
    pub(crate) realm: RealmId,
}

#[derive(Clone, Copy)]
enum RootPublication {
    Public,
    Internal,
}

impl RootPublication {
    const fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

struct InstalledRoot {
    function: FunctionId,
    code: InstalledCodeId,
}

impl Context<'_> {
    /// Returns current logical usage without ending the exclusive context.
    #[must_use]
    pub fn runtime_usage(&self) -> RuntimeUsage {
        self.runtime.usage()
    }

    /// Creates a runtime-local `undefined` value.
    #[must_use]
    pub fn undefined(&self) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Undefined)
    }

    /// Creates a runtime-local `null` value.
    #[must_use]
    pub fn null(&self) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Null)
    }

    /// Creates a runtime-local Boolean value.
    #[must_use]
    pub fn boolean(&self, value: bool) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Boolean(value))
    }

    /// Creates a runtime-local Number value.
    #[must_use]
    pub fn number(&self, value: JsNumber) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Number(value))
    }

    /// Roots an already-owned immutable JavaScript string in this runtime.
    #[must_use]
    pub fn string(&self, value: JsString) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::String(value))
    }

    /// Transactionally installs complete verified bytecode and materializes
    /// its root function in this context's realm.
    ///
    /// Every instruction in every template is feature-checked, including
    /// unreachable instructions and child functions. Unsupported graphs are
    /// rejected before the runtime safe point. Later failures commit no state
    /// attributable to this installation; the safe point may still reclaim
    /// previously unreachable runtime nodes.
    ///
    /// # Errors
    ///
    /// Returns an exact unsupported opcode, limit, allocation, string, atom,
    /// or authority-invariant failure.
    pub fn instantiate(
        &mut self,
        authority: Arc<VerifiedBytecode>,
    ) -> Result<Function, InstallError> {
        require_root_kind(&authority, CompilerExecutableKind::OrdinaryFunction)?;
        let installed = self.install_root(authority, RootPublication::Public)?;
        Ok(Function::from_root(JsValue::rooted_heap(
            &self.runtime.mailbox,
            HeapReference::Function(installed.function),
        )))
    }

    /// Installs and executes one complete verified dynamic-Function Script.
    ///
    /// Only an authority whose root is tagged as
    /// [`CompilerExecutableKind::DynamicFunctionScript`] is accepted. The
    /// internal root has no external lexical environment and is never exposed
    /// as a public function. Its receiver is this context's realm-owned global
    /// object; the exact Script completion is rooted before the internal root
    /// is retired.
    ///
    /// # Errors
    ///
    /// Returns a typed installation failure before execution, or an execution,
    /// exception, resource, allocation, publication, or engine failure after
    /// installation.
    pub fn execute_dynamic_function_script(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
    ) -> Result<JsValue, DynamicFunctionScriptError> {
        require_root_kind(&authority, CompilerExecutableKind::DynamicFunctionScript)?;
        let global_object = self
            .runtime
            .realm_global_object(self.realm)
            .map_err(crate::ExecutionError::from)?;
        let installed = self.install_root(authority, RootPublication::Internal)?;
        let result = self
            .execute_internal_root(
                installed.function,
                StoredValue::Object(global_object),
                limits,
            )
            .and_then(|completion| self.runtime.public_value(completion));
        let retirement = self
            .runtime
            .retire_internal_root(installed.function, installed.code);
        match retirement {
            Ok(()) => result.map_err(DynamicFunctionScriptError::Execution),
            Err(fault) => Err(DynamicFunctionScriptError::Execution(fault.into())),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "installation preflight and the failure-atomic commit are one audited transaction"
    )]
    fn install_root(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        publication: RootPublication,
    ) -> Result<InstalledRoot, InstallError> {
        preflight_opcodes(&authority)?;
        self.runtime.prepare_installation_safe_point()?;

        let graph_usage = authority.compiler_graph().usage();
        let functions = graph_usage.functions();
        let atoms = graph_usage.atoms();
        let constants = graph_usage.constants();
        check_install_limit(
            RuntimeResource::InstalledCode,
            self.runtime.limits.max_installed_code,
            usize_to_u64(self.runtime.code.len()).saturating_add(1),
        )?;
        check_install_limit(
            RuntimeResource::InstalledTemplates,
            self.runtime.limits.max_installed_templates,
            self.runtime.installed_templates.saturating_add(functions),
        )?;
        check_install_limit(
            RuntimeResource::InstalledAtoms,
            self.runtime.limits.max_installed_atoms,
            self.runtime.installed_atoms.saturating_add(atoms),
        )?;
        check_install_limit(
            RuntimeResource::InstalledConstants,
            self.runtime.limits.max_installed_constants,
            self.runtime.installed_constants.saturating_add(constants),
        )?;
        check_install_limit(
            RuntimeResource::HeapFunctions,
            self.runtime.limits.max_heap_functions,
            usize_to_u64(self.runtime.functions.len()).saturating_add(1),
        )?;
        if publication.is_public() {
            check_install_limit(
                RuntimeResource::PublicRoots,
                self.runtime.limits.max_public_roots,
                self.runtime.public_roots.saturating_add(1),
            )?;
        }

        if !authority.root().metadata().closures().is_empty() {
            return Err(InstallError::AuthorityInvariant {
                message: "root function requires an external closure environment",
            });
        }

        self.runtime
            .code
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledCode,
                additional: 1,
            })?;
        self.runtime
            .functions
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        if publication.is_public() {
            self.runtime.mailbox.try_reserve_root().map_err(|_| {
                InstallError::AllocationFailed {
                    resource: RuntimeResource::ReleaseMailbox,
                    additional: 1,
                }
            })?;
        }

        let templates = match self.runtime.stage_templates(&authority) {
            Ok(templates) => templates,
            Err(error) => {
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(error);
            }
        };

        let root_template = authority.root_id();
        let Ok(code) = self.runtime.code.try_insert(InstalledCode {
            authority,
            realm: self.realm,
            templates,
            live_functions: 1,
        }) else {
            if publication.is_public() {
                self.runtime.mailbox.cancel_reserved_root();
            }
            self.runtime.atoms.collect_dead();
            return Err(InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledCode,
                additional: 1,
            });
        };
        let Ok(root) = self.runtime.functions.try_insert(HeapFunction {
            code,
            template: root_template,
            environment: Vec::new(),
            object: ObjectRecord::empty(None),
            public_roots: u32::from(publication.is_public()),
        }) else {
            let removed = self.runtime.code.remove(code);
            debug_assert!(removed.is_some());
            if publication.is_public() {
                self.runtime.mailbox.cancel_reserved_root();
            }
            self.runtime.atoms.collect_dead();
            return Err(InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            });
        };

        self.runtime.installed_templates += functions;
        self.runtime.installed_atoms += atoms;
        self.runtime.installed_constants += constants;
        if publication.is_public() {
            self.runtime.public_roots += 1;
        }
        Ok(InstalledRoot {
            function: root,
            code,
        })
    }
}

fn runtime_string(
    value: &quickjs_bytecode::CompilerString,
) -> Result<JsString, crate::JsStringError> {
    if let Some(units) = value.latin1_units() {
        JsString::from_latin1(units)
    } else {
        JsString::from_code_units(value.code_units())
    }
}

fn require_root_kind(
    authority: &VerifiedBytecode,
    expected: CompilerExecutableKind,
) -> Result<(), InstallError> {
    let actual = authority.root().metadata().executable_kind();
    if actual == expected {
        return Ok(());
    }
    let message = match expected {
        CompilerExecutableKind::OrdinaryFunction => {
            "dynamic-function Script cannot be instantiated as an ordinary function"
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            "ordinary function cannot execute as a dynamic-function Script"
        }
    };
    Err(InstallError::AuthorityInvariant { message })
}

fn preflight_opcodes(authority: &VerifiedBytecode) -> Result<(), InstallError> {
    for (function_index, function) in authority.functions().enumerate() {
        let function_id = FunctionTemplateId::new(u32::try_from(function_index).map_err(|_| {
            InstallError::AuthorityInvariant {
                message: "function template index is not representable",
            }
        })?);
        let instructions = function.function().control_flow().instructions();
        let mappings = function.metadata().source().mappings();
        if instructions.len() != mappings.len() {
            return Err(InstallError::AuthorityInvariant {
                message: "instruction/source mapping lengths differ",
            });
        }
        for (instruction, mapping) in instructions.iter().zip(mappings) {
            let decoded = instruction.decoded();
            let opcode = decoded.instruction().opcode();
            if !is_supported_opcode(opcode) {
                return Err(InstallError::UnsupportedOpcode {
                    function: function_id,
                    pc: decoded.pc(),
                    source_span: mapping.span(),
                    opcode,
                });
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "whole-graph capability admission remains one exhaustive opcode audit"
)]
const fn is_supported_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PushI32
            | FinalOpcode::PushConst
            | FinalOpcode::FClosure
            | FinalOpcode::PushAtomValue
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
            | FinalOpcode::Drop
            | FinalOpcode::Dup
            | FinalOpcode::Insert2
            | FinalOpcode::Call
            | FinalOpcode::CallMethod
            | FinalOpcode::Throw
            | FinalOpcode::Return
            | FinalOpcode::ReturnUndef
            | FinalOpcode::GetLoc
            | FinalOpcode::PutLoc
            | FinalOpcode::SetLoc
            | FinalOpcode::GetArg
            | FinalOpcode::PutArg
            | FinalOpcode::SetArg
            | FinalOpcode::GetVarRef
            | FinalOpcode::PutVarRef
            | FinalOpcode::SetVarRef
            | FinalOpcode::SetLocUninitialized
            | FinalOpcode::GetLocCheck
            | FinalOpcode::PutLocCheck
            | FinalOpcode::SetLocCheck
            | FinalOpcode::GetVarRefCheck
            | FinalOpcode::PutVarRefCheck
            | FinalOpcode::CloseLoc
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::PutField
            | FinalOpcode::DefineField
            | FinalOpcode::IfFalse
            | FinalOpcode::IfTrue
            | FinalOpcode::Goto
            | FinalOpcode::Lnot
            | FinalOpcode::Typeof
            | FinalOpcode::StrictEq
            | FinalOpcode::StrictNeq
            | FinalOpcode::IsUndefinedOrNull
            | FinalOpcode::Nop
            | FinalOpcode::PushMinus1
            | FinalOpcode::Push0
            | FinalOpcode::Push1
            | FinalOpcode::Push2
            | FinalOpcode::Push3
            | FinalOpcode::Push4
            | FinalOpcode::Push5
            | FinalOpcode::Push6
            | FinalOpcode::Push7
            | FinalOpcode::PushI8
            | FinalOpcode::PushI16
            | FinalOpcode::PushConst8
            | FinalOpcode::FClosure8
            | FinalOpcode::PushEmptyString
            | FinalOpcode::GetLoc8
            | FinalOpcode::PutLoc8
            | FinalOpcode::SetLoc8
            | FinalOpcode::GetLoc0
            | FinalOpcode::GetLoc1
            | FinalOpcode::GetLoc2
            | FinalOpcode::GetLoc3
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
            | FinalOpcode::SetLoc0
            | FinalOpcode::SetLoc1
            | FinalOpcode::SetLoc2
            | FinalOpcode::SetLoc3
            | FinalOpcode::GetArg0
            | FinalOpcode::GetArg1
            | FinalOpcode::GetArg2
            | FinalOpcode::GetArg3
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
            | FinalOpcode::SetArg0
            | FinalOpcode::SetArg1
            | FinalOpcode::SetArg2
            | FinalOpcode::SetArg3
            | FinalOpcode::GetVarRef0
            | FinalOpcode::GetVarRef1
            | FinalOpcode::GetVarRef2
            | FinalOpcode::GetVarRef3
            | FinalOpcode::PutVarRef0
            | FinalOpcode::PutVarRef1
            | FinalOpcode::PutVarRef2
            | FinalOpcode::PutVarRef3
            | FinalOpcode::SetVarRef0
            | FinalOpcode::SetVarRef1
            | FinalOpcode::SetVarRef2
            | FinalOpcode::SetVarRef3
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::IfFalse8
            | FinalOpcode::IfTrue8
            | FinalOpcode::Goto8
            | FinalOpcode::Goto16
    )
}

fn check_limit(resource: RuntimeResource, limit: u64, observed: u64) -> Result<(), RuntimeError> {
    if observed <= limit {
        Ok(())
    } else {
        Err(RuntimeError::LimitExceeded {
            resource,
            limit,
            observed,
        })
    }
}

fn stale_heap_reference(reference: HeapReference) -> crate::EngineFault {
    match reference {
        HeapReference::Function(function) => crate::EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        },
        HeapReference::Object(object) => crate::EngineFault::StaleHeapEdge {
            edge: "object",
            index: object.index(),
            generation: object.generation(),
        },
    }
}

fn check_install_limit(
    resource: RuntimeResource,
    limit: u64,
    observed: u64,
) -> Result<(), InstallError> {
    if observed <= limit {
        Ok(())
    } else {
        Err(InstallError::LimitExceeded {
            resource,
            limit,
            observed,
        })
    }
}

pub(crate) fn check_execution_limit(
    resource: RuntimeResource,
    limit: u64,
    observed: u64,
) -> Result<(), crate::ExecutionError> {
    if observed <= limit {
        Ok(())
    } else {
        Err(crate::ExecutionError::LimitExceeded {
            resource,
            limit,
            observed,
        })
    }
}

pub(crate) fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
