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

//! Runtime usage accounting, deferred root releases, and iterative cycle collection.

use super::{
    AtomUsage, BindingCellId, EnvironmentBinding, FunctionId, FunctionImplementation, HashSet,
    HeapReference, ObjectId, ObjectRecord, PromiseJob, RealmIntrinsics, Runtime, RuntimeError,
    RuntimeResource, RuntimeUsage, SlotValue, StoredValue, usize_to_u64,
};
use crate::object::{PromiseCapability, PromiseState};

#[derive(Clone, Copy)]
pub(crate) enum CollectionRoot {
    Heap(HeapReference),
    BindingCell(BindingCellId),
}

enum GraphNode {
    Function(FunctionId),
    Object(ObjectId),
    Cell(BindingCellId),
}

fn mark_collection_root(
    root: CollectionRoot,
    marked_functions: &mut HashSet<FunctionId>,
    marked_objects: &mut HashSet<ObjectId>,
    marked_cells: &mut HashSet<BindingCellId>,
    work: &mut Vec<GraphNode>,
) {
    match root {
        CollectionRoot::Heap(reference) => {
            mark_heap_reference(reference, marked_functions, marked_objects, work);
        }
        CollectionRoot::BindingCell(cell) => {
            if marked_cells.insert(cell) {
                work.push(GraphNode::Cell(cell));
            }
        }
    }
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

fn mark_promise_capability(
    capability: &PromiseCapability,
    marked_functions: &mut HashSet<FunctionId>,
    marked_objects: &mut HashSet<ObjectId>,
    work: &mut Vec<GraphNode>,
) {
    mark_stored_value(&capability.promise, marked_functions, marked_objects, work);
    for function in [capability.resolve, capability.reject] {
        mark_heap_reference(
            HeapReference::Function(function),
            marked_functions,
            marked_objects,
            work,
        );
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
    for function in record.accessor_functions() {
        mark_heap_reference(
            HeapReference::Function(function),
            marked_functions,
            marked_objects,
            work,
        );
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

impl Runtime {
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
            for_in_entries: self.for_in_entries,
            binding_cells: usize_to_u64(self.cells.len()),
            realm_global_bindings: usize_to_u64(self.global_bindings.len()),
            public_roots: self.public_roots,
            pending_releases: usize_to_u64(self.mailbox.pending_len()),
            pending_promise_jobs: usize_to_u64(self.promise_jobs.len()),
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
        self.collect_cycles_with_roots(|_| {})
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the mark and two-phase dead-set transaction remains together for auditability"
    )]
    pub(crate) fn collect_cycles_with_roots(
        &mut self,
        trace_additional_roots: impl FnOnce(&mut dyn FnMut(CollectionRoot)),
    ) -> Result<CollectionReport, RuntimeError> {
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
            if let RealmIntrinsics::Ready {
                function_prototype,
                throw_type_error,
                function_constructor,
                errors,
                boolean,
                number,
                bigint,
                string,
                array,
                promise,
                symbol,
                iterators,
            } = realm.intrinsics
            {
                mark_heap_reference(
                    HeapReference::Function(function_prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(throw_type_error),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(function_constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                for intrinsic in errors.entries {
                    mark_heap_reference(
                        HeapReference::Object(intrinsic.prototype),
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                    mark_heap_reference(
                        HeapReference::Function(intrinsic.constructor),
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                }
                for function in [errors.to_string, errors.is_error] {
                    mark_heap_reference(
                        HeapReference::Function(function),
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                }
                mark_heap_reference(
                    HeapReference::Object(boolean.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(boolean.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Object(number.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(number.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Object(bigint.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(bigint.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Object(string.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(string.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Object(array.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(array.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Object(promise.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(promise.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Object(symbol.prototype),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                mark_heap_reference(
                    HeapReference::Function(symbol.constructor),
                    &mut marked_functions,
                    &mut marked_objects,
                    &mut work,
                );
                for prototype in [
                    iterators.iterator_prototype,
                    iterators.array_iterator_prototype,
                    iterators.string_iterator_prototype,
                ] {
                    mark_heap_reference(
                        HeapReference::Object(prototype),
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                }
            }
        }
        for job in &self.promise_jobs {
            match job {
                PromiseJob::Reaction { reaction, argument } => {
                    if let Some(handler) = reaction.handler {
                        mark_heap_reference(
                            HeapReference::Function(handler),
                            &mut marked_functions,
                            &mut marked_objects,
                            &mut work,
                        );
                    }
                    mark_promise_capability(
                        &reaction.capability,
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                    mark_stored_value(
                        argument,
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                }
                PromiseJob::Thenable {
                    promise,
                    realm: _,
                    thenable,
                    then,
                } => {
                    mark_heap_reference(
                        HeapReference::Object(*promise),
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                    mark_stored_value(
                        thenable,
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                    mark_heap_reference(
                        HeapReference::Function(*then),
                        &mut marked_functions,
                        &mut marked_objects,
                        &mut work,
                    );
                }
            }
        }
        trace_additional_roots(&mut |root| {
            let live = match root {
                CollectionRoot::Heap(HeapReference::Function(function)) => {
                    self.functions.contains(function)
                }
                CollectionRoot::Heap(HeapReference::Object(object)) => {
                    self.objects.contains(object)
                }
                CollectionRoot::BindingCell(cell) => self.cells.contains(cell),
            };
            debug_assert!(live, "execution root must name a live heap node");
            if !live {
                return;
            }
            mark_collection_root(
                root,
                &mut marked_functions,
                &mut marked_objects,
                &mut marked_cells,
                &mut work,
            );
        });

        while let Some(node) = work.pop() {
            match node {
                GraphNode::Function(id) => {
                    if let Some(function) = self.functions.get(id) {
                        if let FunctionImplementation::Bytecode(bytecode) = &function.implementation
                        {
                            for binding in bytecode.environment.iter().copied() {
                                if let EnvironmentBinding::Captured(cell) = binding
                                    && marked_cells.insert(cell)
                                {
                                    work.push(GraphNode::Cell(cell));
                                }
                            }
                        }
                        if let FunctionImplementation::Bound(bound) = &function.implementation {
                            mark_heap_reference(
                                HeapReference::Function(bound.target),
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            );
                            mark_stored_value(
                                &bound.bound_this,
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            );
                            for argument in &bound.bound_arguments {
                                mark_stored_value(
                                    argument,
                                    &mut marked_functions,
                                    &mut marked_objects,
                                    &mut work,
                                );
                            }
                        }
                        if let FunctionImplementation::PromiseResolving(resolving) =
                            &function.implementation
                        {
                            mark_heap_reference(
                                HeapReference::Object(resolving.promise),
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            );
                        }
                        if let FunctionImplementation::PromiseCapabilityExecutor(executor) =
                            &function.implementation
                        {
                            let capture = executor.capture.borrow();
                            for value in [capture.resolve.as_ref(), capture.reject.as_ref()]
                                .into_iter()
                                .flatten()
                            {
                                mark_stored_value(
                                    value,
                                    &mut marked_functions,
                                    &mut marked_objects,
                                    &mut work,
                                );
                            }
                        }
                        if let FunctionImplementation::PromiseFinally(finally) =
                            &function.implementation
                        {
                            match finally {
                                super::PromiseFinallyFunction::Handler {
                                    on_finally,
                                    constructor,
                                    ..
                                } => {
                                    for function in [on_finally, constructor] {
                                        mark_heap_reference(
                                            HeapReference::Function(*function),
                                            &mut marked_functions,
                                            &mut marked_objects,
                                            &mut work,
                                        );
                                    }
                                }
                                super::PromiseFinallyFunction::Thunk { completion, .. } => {
                                    mark_stored_value(
                                        completion,
                                        &mut marked_functions,
                                        &mut marked_objects,
                                        &mut work,
                                    );
                                }
                            }
                        }
                        if let FunctionImplementation::PromiseCombinatorElement(element) =
                            &function.implementation
                        {
                            let shared = element.shared.borrow();
                            mark_promise_capability(
                                &shared.capability,
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            );
                            for value in shared.values.iter().flatten() {
                                mark_stored_value(
                                    value,
                                    &mut marked_functions,
                                    &mut marked_objects,
                                    &mut work,
                                );
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
                        for cell in object.arguments_cells() {
                            if marked_cells.insert(cell) {
                                work.push(GraphNode::Cell(cell));
                            }
                        }
                        if let Some(current) = object.for_in_current() {
                            mark_heap_reference(
                                current,
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            );
                        }
                        if let Some(current) = object.array_iterator_current() {
                            mark_heap_reference(
                                current,
                                &mut marked_functions,
                                &mut marked_objects,
                                &mut work,
                            );
                        }
                        if let Some(state) = object.promise_state() {
                            match state {
                                PromiseState::Pending {
                                    fulfill_reactions,
                                    reject_reactions,
                                    ..
                                } => {
                                    for reaction in
                                        fulfill_reactions.iter().chain(reject_reactions.iter())
                                    {
                                        if let Some(handler) = reaction.handler {
                                            mark_heap_reference(
                                                HeapReference::Function(handler),
                                                &mut marked_functions,
                                                &mut marked_objects,
                                                &mut work,
                                            );
                                        }
                                        mark_promise_capability(
                                            &reaction.capability,
                                            &mut marked_functions,
                                            &mut marked_objects,
                                            &mut work,
                                        );
                                    }
                                }
                                PromiseState::Fulfilled(value)
                                | PromiseState::Rejected { reason: value, .. } => {
                                    mark_stored_value(
                                        value,
                                        &mut marked_functions,
                                        &mut marked_objects,
                                        &mut work,
                                    );
                                }
                            }
                        }
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
                if let FunctionImplementation::Bytecode(bytecode) = function.implementation
                    && let Some(code) = self.code.get_mut(bytecode.code)
                {
                    debug_assert!(code.live_functions > 0);
                    code.live_functions = code.live_functions.saturating_sub(1);
                    if code.live_functions == 0 {
                        maybe_dead_code.push(bytecode.code);
                    }
                }
            }
        }
        for id in dead_objects {
            if let Some(object) = self.objects.remove(id) {
                self.object_properties = self
                    .object_properties
                    .saturating_sub(usize_to_u64(object.record.property_count()));
                self.for_in_entries = self
                    .for_in_entries
                    .saturating_sub(usize_to_u64(object.for_in_entry_count()));
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
}
