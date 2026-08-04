//! Promise object and resolving-function allocation.

use super::{
    Cell, FunctionId, FunctionImplementation, HeapFunction, HeapObject, HeapReference, JsNumber,
    JsString, ObjectId, ObjectRecord, PredefinedAtom, PromiseCapabilityCapture,
    PromiseCapabilityExecutor, PromiseFinallyFunction, PromiseFinallyHandlerKind,
    PromiseFinallyThunkKind, PromiseResolvingFunction, PromiseResolvingKind, PropertyKey,
    PropertyLayout, Rc, RealmId, RefCell, Runtime, RuntimeResource, StoredValue,
    check_execution_limit, usize_to_u64,
};

fn promise_builtin_function_record(
    prototype: FunctionId,
    length: i32,
    length_key: &PropertyKey,
    name_key: &PropertyKey,
) -> Result<ObjectRecord, crate::ExecutionError> {
    let mut record = ObjectRecord::empty(Some(HeapReference::Function(prototype)));
    record
        .try_reserve_data(2)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 2,
        })?;
    record
        .append_data(
            length_key.clone(),
            PropertyLayout::data(false, false, true),
            StoredValue::Number(JsNumber::from_i32(length)),
        )
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    record
        .append_data(
            name_key.clone(),
            PropertyLayout::data(false, false, true),
            StoredValue::String(JsString::empty()),
        )
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    Ok(record)
}

impl Runtime {
    pub(crate) fn realm_promise_prototype(
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
        let super::RealmIntrinsics::Ready { promise, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Promise prototype requested while Realm intrinsics are initializing",
            });
        };
        Ok(promise.prototype)
    }

    pub(crate) fn realm_promise_constructor(
        &self,
        realm: RealmId,
    ) -> Result<FunctionId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let super::RealmIntrinsics::Ready { promise, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Promise constructor requested while Realm intrinsics are initializing",
            });
        };
        Ok(promise.constructor)
    }

    #[cfg(test)]
    pub(crate) fn allocate_intrinsic_promise(
        &mut self,
        realm: RealmId,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_promise_prototype(realm)?;
        self.allocate_promise_with_prototype(HeapReference::Object(prototype))
    }

    pub(crate) fn allocate_promise_with_prototype(
        &mut self,
        prototype: HeapReference,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(super::stale_heap_reference(prototype).into());
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
        let promise = self
            .objects
            .try_insert(HeapObject::promise(ObjectRecord::empty(Some(prototype))))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(promise)
    }

    pub(crate) fn allocate_promise_resolving_functions(
        &mut self,
        promise: ObjectId,
        realm: RealmId,
    ) -> Result<(FunctionId, FunctionId), crate::ExecutionError> {
        if !self
            .objects
            .get(promise)
            .is_some_and(HeapObject::is_promise)
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Promise",
                index: promise.index(),
                generation: promise.generation(),
            }
            .into());
        }
        check_execution_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(2),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(4),
        )?;
        self.functions
            .try_reserve(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 2,
            })?;

        let prototype = self.realm_function_prototype(realm)?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let name_key = self.predefined_property_key(PredefinedAtom::Name);
        let resolve_record = promise_builtin_function_record(prototype, 1, &length_key, &name_key)?;
        let reject_record = promise_builtin_function_record(prototype, 1, &length_key, &name_key)?;
        let already_resolved = Rc::new(Cell::new(false));
        let resolve = self
            .functions
            .try_insert(HeapFunction {
                implementation: FunctionImplementation::PromiseResolving(
                    PromiseResolvingFunction {
                        promise,
                        realm,
                        kind: PromiseResolvingKind::Resolve,
                        already_resolved: Rc::clone(&already_resolved),
                    },
                ),
                object: resolve_record,
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let Ok(reject) = self.functions.try_insert(HeapFunction {
            implementation: FunctionImplementation::PromiseResolving(PromiseResolvingFunction {
                promise,
                realm,
                kind: PromiseResolvingKind::Reject,
                already_resolved,
            }),
            object: reject_record,
            public_roots: 0,
        }) else {
            let removed = self.functions.remove(resolve);
            debug_assert!(removed.is_some());
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            });
        };
        self.object_properties = self.object_properties.saturating_add(4);
        self.collection_pending = true;
        Ok((resolve, reject))
    }

    pub(crate) fn allocate_promise_capability_executor(
        &mut self,
        realm: RealmId,
    ) -> Result<(FunctionId, Rc<RefCell<PromiseCapabilityCapture>>), crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(2),
        )?;
        self.functions
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let prototype = self.realm_function_prototype(realm)?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let name_key = self.predefined_property_key(PredefinedAtom::Name);
        let object = promise_builtin_function_record(prototype, 2, &length_key, &name_key)?;
        let capture = Rc::new(RefCell::new(PromiseCapabilityCapture::default()));
        let function = self
            .functions
            .try_insert(HeapFunction {
                implementation: FunctionImplementation::PromiseCapabilityExecutor(
                    PromiseCapabilityExecutor {
                        realm,
                        capture: Rc::clone(&capture),
                    },
                ),
                object,
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(2);
        self.collection_pending = true;
        Ok((function, capture))
    }

    pub(crate) fn allocate_promise_finally_handlers(
        &mut self,
        realm: RealmId,
        on_finally: FunctionId,
        constructor: FunctionId,
    ) -> Result<(FunctionId, FunctionId), crate::ExecutionError> {
        for function in [on_finally, constructor] {
            if self.functions.get(function).is_none() {
                return Err(crate::EngineFault::StaleHeapEdge {
                    edge: "function",
                    index: function.index(),
                    generation: function.generation(),
                }
                .into());
            }
        }
        check_execution_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(2),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(4),
        )?;
        self.functions
            .try_reserve(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 2,
            })?;

        let prototype = self.realm_function_prototype(realm)?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let name_key = self.predefined_property_key(PredefinedAtom::Name);
        let then_record = promise_builtin_function_record(prototype, 1, &length_key, &name_key)?;
        let catch_record = promise_builtin_function_record(prototype, 1, &length_key, &name_key)?;
        let then_finally = self
            .functions
            .try_insert(HeapFunction {
                implementation: FunctionImplementation::PromiseFinally(
                    PromiseFinallyFunction::Handler {
                        realm,
                        on_finally,
                        constructor,
                        kind: PromiseFinallyHandlerKind::Then,
                    },
                ),
                object: then_record,
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let Ok(catch_finally) = self.functions.try_insert(HeapFunction {
            implementation: FunctionImplementation::PromiseFinally(
                PromiseFinallyFunction::Handler {
                    realm,
                    on_finally,
                    constructor,
                    kind: PromiseFinallyHandlerKind::Catch,
                },
            ),
            object: catch_record,
            public_roots: 0,
        }) else {
            let removed = self.functions.remove(then_finally);
            debug_assert!(removed.is_some());
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            });
        };
        self.object_properties = self.object_properties.saturating_add(4);
        self.collection_pending = true;
        Ok((then_finally, catch_finally))
    }

    pub(crate) fn allocate_promise_finally_thunk(
        &mut self,
        realm: RealmId,
        completion: StoredValue,
        kind: PromiseFinallyThunkKind,
    ) -> Result<FunctionId, crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(2),
        )?;
        self.functions
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let prototype = self.realm_function_prototype(realm)?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let name_key = self.predefined_property_key(PredefinedAtom::Name);
        let object = promise_builtin_function_record(prototype, 0, &length_key, &name_key)?;
        let function = self
            .functions
            .try_insert(HeapFunction {
                implementation: FunctionImplementation::PromiseFinally(
                    PromiseFinallyFunction::Thunk {
                        realm,
                        completion,
                        kind,
                    },
                ),
                object,
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(2);
        self.collection_pending = true;
        Ok(function)
    }
}
