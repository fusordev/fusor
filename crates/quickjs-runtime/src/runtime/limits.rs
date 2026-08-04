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

//! Runtime resource ceilings and observable logical usage snapshots.

use super::AtomLimits;

const DEFAULT_MAX_REALMS: u64 = 65_535;
const DEFAULT_MAX_INSTALLED_CODE: u64 = 65_535;
const DEFAULT_MAX_INSTALLED_TEMPLATES: u64 = 1_048_576;
const DEFAULT_MAX_INSTALLED_ATOMS: u64 = 1_048_576;
const DEFAULT_MAX_INSTALLED_CONSTANTS: u64 = 1_048_576;
const DEFAULT_MAX_HEAP_FUNCTIONS: u64 = 1_048_576;
const DEFAULT_MAX_HEAP_OBJECTS: u64 = 1_048_576;
const DEFAULT_MAX_OBJECT_PROPERTIES: u64 = 16_777_216;
const DEFAULT_MAX_FOR_IN_ENTRIES: u64 = 16_777_216;
const DEFAULT_MAX_BINDING_CELLS: u64 = 1_048_576;
const DEFAULT_MAX_REALM_GLOBAL_BINDINGS: u64 = 1_048_576;
const DEFAULT_MAX_PUBLIC_ROOTS: u64 = 1_048_576;
const DEFAULT_MAX_ACTIVE_FRAMES: u32 = 1_024;
const DEFAULT_MAX_ACTIVE_FRAME_VALUES: u64 = 16_777_216;
const DEFAULT_MAX_PENDING_PROMISE_JOBS: u64 = 1_048_576;

/// Inclusive logical ceilings for one JavaScript runtime.
///
/// Immutable string backing still follows Rust's global allocator policy. The
/// installed string and atom counts are enforced, but this initial VM does not
/// claim a complete byte-accurate heap limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeLimits {
    pub(super) atom_limits: AtomLimits,
    pub(super) max_realms: u64,
    pub(super) max_installed_code: u64,
    pub(super) max_installed_templates: u64,
    pub(super) max_installed_atoms: u64,
    pub(super) max_installed_constants: u64,
    pub(crate) max_heap_functions: u64,
    pub(crate) max_heap_objects: u64,
    pub(crate) max_object_properties: u64,
    pub(crate) max_for_in_entries: u64,
    pub(crate) max_binding_cells: u64,
    pub(crate) max_realm_global_bindings: u64,
    pub(super) max_public_roots: u64,
    pub(crate) max_active_frames: u32,
    pub(crate) max_active_frame_values: u64,
    pub(crate) max_pending_promise_jobs: u64,
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

    /// Replaces the maximum aggregate `for-in` snapshot and visited-key entry count.
    #[must_use]
    pub const fn with_max_for_in_entries(mut self, maximum: u64) -> Self {
        self.max_for_in_entries = maximum;
        self
    }

    /// Replaces the maximum live binding-cell count.
    #[must_use]
    pub const fn with_max_binding_cells(mut self, maximum: u64) -> Self {
        self.max_binding_cells = maximum;
        self
    }

    /// Replaces the maximum realm-owned global binding count.
    #[must_use]
    pub const fn with_max_realm_global_bindings(mut self, maximum: u64) -> Self {
        self.max_realm_global_bindings = maximum;
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

    /// Replaces the maximum number of Promise jobs waiting in the runtime FIFO.
    #[must_use]
    pub const fn with_max_pending_promise_jobs(mut self, maximum: u64) -> Self {
        self.max_pending_promise_jobs = maximum;
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
            max_for_in_entries: DEFAULT_MAX_FOR_IN_ENTRIES,
            max_binding_cells: DEFAULT_MAX_BINDING_CELLS,
            max_realm_global_bindings: DEFAULT_MAX_REALM_GLOBAL_BINDINGS,
            max_public_roots: DEFAULT_MAX_PUBLIC_ROOTS,
            max_active_frames: DEFAULT_MAX_ACTIVE_FRAMES,
            max_active_frame_values: DEFAULT_MAX_ACTIVE_FRAME_VALUES,
            max_pending_promise_jobs: DEFAULT_MAX_PENDING_PROMISE_JOBS,
        }
    }
}

/// Snapshot of logical runtime usage.
///
/// Charged counts include releases queued by dropped function handles until
/// the next mutable safe point drains `pending_releases`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeUsage {
    pub(super) realms: u64,
    pub(super) installed_code: u64,
    pub(super) installed_templates: u64,
    pub(super) installed_atoms: u64,
    pub(super) installed_constants: u64,
    pub(super) heap_functions: u64,
    pub(super) heap_objects: u64,
    pub(super) object_properties: u64,
    pub(super) for_in_entries: u64,
    pub(super) binding_cells: u64,
    pub(super) realm_global_bindings: u64,
    pub(super) public_roots: u64,
    pub(super) pending_releases: u64,
    pub(super) pending_promise_jobs: u64,
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

    /// Returns the aggregate property-key entries retained by live `for-in` iterators.
    #[must_use]
    pub const fn for_in_entries(self) -> u64 {
        self.for_in_entries
    }

    /// Returns the number of live captured-binding cells.
    #[must_use]
    pub const fn binding_cells(self) -> u64 {
        self.binding_cells
    }

    /// Returns the number of constructor-realm global binding records.
    #[must_use]
    pub const fn realm_global_bindings(self) -> u64 {
        self.realm_global_bindings
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

    /// Returns the number of Promise jobs retained by the runtime FIFO.
    #[must_use]
    pub const fn pending_promise_jobs(self) -> u64 {
        self.pending_promise_jobs
    }
}
