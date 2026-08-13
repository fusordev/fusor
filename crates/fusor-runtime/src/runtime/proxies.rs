//! Proxy exotic allocation and revocation state.

#[allow(
    clippy::wildcard_imports,
    reason = "this private runtime sibling participates in the shared runtime implementation namespace"
)]
use super::*;

fn proxy_revoker_record(
    prototype: FunctionId,
    length_key: PropertyKey,
    name_key: PropertyKey,
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
            length_key,
            PropertyLayout::data(false, false, true),
            StoredValue::Number(JsNumber::from_i32(0)),
        )
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    record
        .append_data(
            name_key,
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
    pub(crate) fn proxy_state(
        &self,
        reference: HeapReference,
    ) -> Result<Option<&ProxyState>, crate::EngineFault> {
        match reference {
            HeapReference::Function(function) => {
                self.functions.get(function).map(HeapFunction::proxy).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "Proxy function",
                        index: function.index(),
                        generation: function.generation(),
                    },
                )
            }
            HeapReference::Object(object) => {
                self.objects.get(object).map(HeapObject::proxy_state).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "Proxy object",
                        index: object.index(),
                        generation: object.generation(),
                    },
                )
            }
        }
    }

    pub(crate) fn proxy_state_mut(
        &mut self,
        reference: HeapReference,
    ) -> Result<Option<&mut ProxyState>, crate::EngineFault> {
        match reference {
            HeapReference::Function(function) => self
                .functions
                .get_mut(function)
                .map(HeapFunction::proxy_mut)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Proxy function",
                    index: function.index(),
                    generation: function.generation(),
                }),
            HeapReference::Object(object) => self
                .objects
                .get_mut(object)
                .map(HeapObject::proxy_state_mut)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Proxy object",
                    index: object.index(),
                    generation: object.generation(),
                }),
        }
    }

    pub(crate) fn revoke_proxy(
        &mut self,
        reference: HeapReference,
    ) -> Result<(), crate::EngineFault> {
        let state =
            self.proxy_state_mut(reference)?
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "Proxy revoker target is not a Proxy exotic object",
                })?;
        state.revoke();
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn allocate_proxy(
        &mut self,
        realm: RealmId,
        target: HeapReference,
        handler: HeapReference,
        constructable: bool,
    ) -> Result<StoredValue, crate::ExecutionError> {
        if !self.heap_reference_is_live(target) {
            return Err(stale_heap_reference(target).into());
        }
        if !self.heap_reference_is_live(handler) {
            return Err(stale_heap_reference(handler).into());
        }
        let callable = matches!(target, HeapReference::Function(_));
        let state = ProxyState::new(target, handler, callable, constructable, realm);
        let value = if callable {
            check_execution_limit(
                RuntimeResource::HeapFunctions,
                self.limits.max_heap_functions,
                usize_to_u64(self.functions.len()).saturating_add(1),
            )?;
            self.functions
                .try_reserve(1)
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::HeapFunctions,
                    additional: 1,
                })?;
            let function = self
                .insert_heap_function(HeapFunction {
                    implementation: FunctionImplementation::Proxy(state),
                    object: ObjectRecord::empty(None),
                    public_roots: 0,
                })
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::HeapFunctions,
                    additional: 1,
                })?;
            StoredValue::Function(function)
        } else {
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
                .insert_heap_object(HeapObject::proxy(ObjectRecord::empty(None), state))
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::HeapObjects,
                    additional: 1,
                })?;
            StoredValue::Object(object)
        };
        self.collection_pending = true;
        Ok(value)
    }

    pub(crate) fn allocate_proxy_revoker(
        &mut self,
        realm: RealmId,
        proxy: HeapReference,
    ) -> Result<FunctionId, crate::ExecutionError> {
        if self.proxy_state(proxy)?.is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Proxy revoker allocation target is not a Proxy exotic object",
            }
            .into());
        }
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
        let record = proxy_revoker_record(
            self.realm_function_prototype(realm)?,
            self.predefined_property_key(PredefinedAtom::Length),
            self.predefined_property_key(PredefinedAtom::Name),
        )?;
        let revoker = self
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::ProxyRevoker(ProxyRevokerFunction {
                    proxy,
                    realm,
                }),
                object: record,
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(2);
        self.collection_pending = true;
        Ok(revoker)
    }

    pub(crate) fn allocate_proxy_revocable_result(
        &mut self,
        realm: RealmId,
        proxy: StoredValue,
        revoke: FunctionId,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if proxy.heap_reference().is_none() || self.functions.get(revoke).is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Proxy.revocable result received a stale component",
            }
            .into());
        }
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
        let mut record = ObjectRecord::empty(Some(HeapReference::Object(
            self.realm_object_prototype(realm)?,
        )));
        record
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::ProxyIdentifier),
                PropertyLayout::data(true, true, true),
                proxy,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Revoke),
                PropertyLayout::data(true, true, true),
                StoredValue::Function(revoke),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let result = self
            .insert_heap_object(HeapObject::ordinary(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(2);
        self.collection_pending = true;
        Ok(result)
    }
}
