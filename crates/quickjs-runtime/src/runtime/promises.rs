//! Promise object and resolving-function allocation.

use super::{
    ArrayState, Cell, ErrorIntrinsicKind, FunctionId, FunctionImplementation, HeapFunction,
    HeapObject, HeapReference, JsNumber, JsString, ObjectId, ObjectRecord, PredefinedAtom,
    PromiseCapabilityCapture, PromiseCapabilityExecutor, PromiseCombinatorElementFunction,
    PromiseCombinatorElementKind, PromiseCombinatorKind, PromiseCombinatorShared,
    PromiseFinallyFunction, PromiseFinallyHandlerKind, PromiseFinallyThunkKind,
    PromiseResolvingFunction, PromiseResolvingKind, PropertyKey, PropertyLayout, Rc, RealmId,
    RefCell, Runtime, RuntimeResource, StoredValue, check_execution_limit, usize_to_u64,
};
use crate::object::PromiseCapability;

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

fn promise_any_error_records(
    array_prototype: ObjectId,
    error_prototype: ObjectId,
    length_key: PropertyKey,
    length: u32,
    errors: Vec<StoredValue>,
) -> Result<(ObjectRecord, ObjectRecord), crate::ExecutionError> {
    let array_property_count = errors.len().saturating_add(1);
    let mut array_record = ObjectRecord::empty(Some(HeapReference::Object(array_prototype)));
    array_record
        .try_reserve_data(array_property_count)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: array_property_count,
        })?;
    array_record
        .append_data(
            length_key,
            PropertyLayout::data(true, false, false),
            StoredValue::Number(JsNumber::from_f64(f64::from(length))),
        )
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: array_property_count,
        })?;
    array_record.append_dense_array_data(errors).map_err(|_| {
        crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: array_property_count,
        }
    })?;
    let mut error_record = ObjectRecord::empty(Some(HeapReference::Object(error_prototype)));
    error_record
        .try_reserve_data(1)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    Ok((array_record, error_record))
}

impl Runtime {
    pub(crate) fn allocate_promise_combinator_elements(
        &mut self,
        realm: RealmId,
        kind: PromiseCombinatorKind,
        index: usize,
        shared: &Rc<RefCell<PromiseCombinatorShared>>,
    ) -> Result<(Option<FunctionId>, Option<FunctionId>), crate::ExecutionError> {
        let (resolve_kind, reject_kind) = match kind {
            PromiseCombinatorKind::All => (Some(PromiseCombinatorElementKind::AllResolve), None),
            PromiseCombinatorKind::AllSettled => (
                Some(PromiseCombinatorElementKind::AllSettledResolve),
                Some(PromiseCombinatorElementKind::AllSettledReject),
            ),
            PromiseCombinatorKind::Any => (None, Some(PromiseCombinatorElementKind::AnyReject)),
            PromiseCombinatorKind::Race => {
                return Err(crate::EngineFault::RuntimeInvariant {
                    message: "Promise.race requested element closures",
                }
                .into());
            }
        };
        let function_count =
            usize::from(resolve_kind.is_some()) + usize::from(reject_kind.is_some());
        check_execution_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(usize_to_u64(function_count)),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties
                .saturating_add(usize_to_u64(function_count.saturating_mul(2))),
        )?;
        self.functions.try_reserve(function_count).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: function_count,
            }
        })?;

        let prototype = self.realm_function_prototype(realm)?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let name_key = self.predefined_property_key(PredefinedAtom::Name);
        let already_called = Rc::new(Cell::new(false));
        let mut insert = |element_kind| -> Result<FunctionId, crate::ExecutionError> {
            let object = promise_builtin_function_record(prototype, 1, &length_key, &name_key)?;
            self.functions
                .try_insert(HeapFunction {
                    implementation: FunctionImplementation::PromiseCombinatorElement(
                        PromiseCombinatorElementFunction {
                            realm,
                            kind: element_kind,
                            index,
                            shared: Rc::clone(shared),
                            already_called: Rc::clone(&already_called),
                        },
                    ),
                    object,
                    public_roots: 0,
                })
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::HeapFunctions,
                    additional: 1,
                })
        };
        let resolve = resolve_kind.map(&mut insert).transpose()?;
        let reject = match reject_kind.map(&mut insert).transpose() {
            Ok(reject) => reject,
            Err(error) => {
                if let Some(resolve) = resolve {
                    let removed = self.functions.remove(resolve);
                    debug_assert!(removed.is_some());
                }
                return Err(error);
            }
        };
        self.object_properties = self
            .object_properties
            .saturating_add(usize_to_u64(function_count.saturating_mul(2)));
        self.collection_pending = true;
        Ok((resolve, reject))
    }

    pub(crate) fn allocate_promise_settlement_record(
        &mut self,
        realm: RealmId,
        fulfilled: bool,
        completion: StoredValue,
    ) -> Result<ObjectId, crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(2),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let prototype = self.realm_object_prototype(realm)?;
        let mut record = ObjectRecord::empty(Some(HeapReference::Object(prototype)));
        record
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        let layout = PropertyLayout::data(true, true, true);
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Status),
                layout,
                StoredValue::String(JsString::from_utf8(if fulfilled {
                    "fulfilled"
                } else {
                    "rejected"
                })?),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(if fulfilled {
                    PredefinedAtom::Value
                } else {
                    PredefinedAtom::Reason
                }),
                layout,
                completion,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let object = self
            .objects
            .try_insert(HeapObject::ordinary(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(2);
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_promise_any_error(
        &mut self,
        realm: RealmId,
        errors: Vec<StoredValue>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let array_property_count =
            errors
                .len()
                .checked_add(1)
                .ok_or(crate::ExecutionError::LimitExceeded {
                    resource: RuntimeResource::ObjectProperties,
                    limit: u64::from(u32::MAX).saturating_add(1),
                    observed: u64::MAX,
                })?;
        let length =
            u32::try_from(errors.len()).map_err(|_| crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: u64::from(u32::MAX).saturating_add(1),
                observed: usize_to_u64(array_property_count),
            })?;
        let property_count =
            array_property_count
                .checked_add(1)
                .ok_or(crate::ExecutionError::LimitExceeded {
                    resource: RuntimeResource::ObjectProperties,
                    limit: self.limits.max_object_properties,
                    observed: u64::MAX,
                })?;
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(2),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties
                .saturating_add(usize_to_u64(property_count)),
        )?;
        self.objects
            .try_reserve(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 2,
            })?;

        let array_prototype = self.realm_array_prototype(realm)?;
        let error_prototype =
            self.realm_error_intrinsic_prototype(realm, ErrorIntrinsicKind::AggregateError)?;
        let (array_record, mut error_record) = promise_any_error_records(
            array_prototype,
            error_prototype,
            self.predefined_property_key(PredefinedAtom::Length),
            length,
            errors,
        )?;

        let errors = self
            .objects
            .try_insert(HeapObject::array(array_record, ArrayState::new(length)))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        if error_record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Errors),
                PropertyLayout::data(true, false, true),
                StoredValue::Object(errors),
            )
            .is_err()
        {
            let removed = self.objects.remove(errors);
            debug_assert!(removed.is_some());
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            });
        }
        let Ok(error) = self.objects.try_insert(HeapObject::error(error_record)) else {
            let removed = self.objects.remove(errors);
            debug_assert!(removed.is_some());
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        self.object_properties = self
            .object_properties
            .saturating_add(usize_to_u64(property_count));
        self.collection_pending = true;
        Ok(error)
    }

    pub(crate) fn allocate_promise_capability_record(
        &mut self,
        realm: RealmId,
        capability: PromiseCapability,
    ) -> Result<ObjectId, crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(3),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;

        let prototype = self.realm_object_prototype(realm)?;
        let mut record = ObjectRecord::empty(Some(HeapReference::Object(prototype)));
        record
            .try_reserve_data(3)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 3,
            })?;
        let layout = PropertyLayout::data(true, true, true);
        for (atom, value) in [
            (PredefinedAtom::PromiseIdentifier, capability.promise),
            (
                PredefinedAtom::Resolve,
                StoredValue::Function(capability.resolve),
            ),
            (
                PredefinedAtom::Reject,
                StoredValue::Function(capability.reject),
            ),
        ] {
            record
                .append_data(self.predefined_property_key(atom), layout, value)
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ObjectProperties,
                    additional: 1,
                })?;
        }
        let object = self
            .objects
            .try_insert(HeapObject::ordinary(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(3);
        self.collection_pending = true;
        Ok(object)
    }

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
