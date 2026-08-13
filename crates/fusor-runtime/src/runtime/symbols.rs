/*
 * JavaScript Symbol intrinsic storage derived from QuickJS.
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

//! Realm Symbol prototype lookup, symbol creation, and wrapper allocation.

use super::{
    Atom, AtomError, BoxedPrimitive, HeapObject, HeapReference, JsString, ObjectId, ObjectRecord,
    RealmId, RealmIntrinsics, Runtime, RuntimeResource, check_execution_limit,
    stale_heap_reference, usize_to_u64,
};

impl Runtime {
    pub(crate) fn realm_symbol_prototype(
        &self,
        realm: RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let RealmIntrinsics::Ready { symbol, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Symbol intrinsics are not initialized",
            });
        };
        if self.objects.get(symbol.prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Symbol.prototype intrinsic",
                index: symbol.prototype.index(),
                generation: symbol.prototype.generation(),
            });
        }
        Ok(symbol.prototype)
    }

    pub(crate) fn new_unique_symbol(
        &mut self,
        description: Option<&JsString>,
    ) -> Result<Atom, AtomError> {
        self.atoms.new_unique_symbol(description)
    }

    pub(crate) fn new_private_name(&mut self, description: &JsString) -> Result<Atom, AtomError> {
        self.atoms.new_private_name(description)
    }

    pub(crate) fn intern_global_symbol(
        &mut self,
        description: &JsString,
    ) -> Result<Atom, AtomError> {
        self.atoms.intern_global_symbol(description)
    }

    pub(crate) fn allocate_boxed_symbol(
        &mut self,
        realm: RealmId,
        value: Atom,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_symbol_prototype(realm)?;
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
            .insert_heap_object(HeapObject::with_boxed_primitive(
                ObjectRecord::empty(Some(HeapReference::Object(prototype))),
                BoxedPrimitive::Symbol(value),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn boxed_symbol(
        &self,
        object: ObjectId,
    ) -> Result<Option<&Atom>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))
            .map(|object| object.boxed_primitive().and_then(BoxedPrimitive::as_symbol))
    }
}
