//! `%Intl%` object allocation and Locale internal-slot access.

use quickjs_intl::{
    CollatorState, DateTimeFormatState, DisplayNamesState, ListFormatState, NumberFormatState,
    PluralRulesState, RelativeTimeFormatState,
};

use super::{
    Atom, BoundFunction, FunctionId, FunctionImplementation, HeapFunction, HeapObject,
    HeapReference, JsString, NativeFunctionKind, ObjectId, ObjectRecord, PredefinedAtom,
    PropertyLayout, RealmId, RealmIntrinsics, Runtime, RuntimeResource, StoredValue,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_intl_display_names(
        &mut self,
        prototype: HeapReference,
        resolved: DisplayNamesState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_display_names(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_display_names_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&DisplayNamesState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DisplayNames object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::intl_display_names_state)
    }

    pub(crate) fn realm_intl_display_names_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.display_names_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DisplayNames.prototype intrinsic",
                index: intl.display_names_prototype.index(),
                generation: intl.display_names_prototype.generation(),
            });
        }
        Ok(intl.display_names_prototype)
    }

    pub(crate) fn allocate_intl_list_format(
        &mut self,
        prototype: HeapReference,
        resolved: ListFormatState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_list_format(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_list_format_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&ListFormatState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.ListFormat object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::intl_list_format_state)
    }

    pub(crate) fn realm_intl_list_format_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.list_format_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.ListFormat.prototype intrinsic",
                index: intl.list_format_prototype.index(),
                generation: intl.list_format_prototype.generation(),
            });
        }
        Ok(intl.list_format_prototype)
    }

    pub(crate) fn allocate_intl_relative_time_format(
        &mut self,
        prototype: HeapReference,
        resolved: RelativeTimeFormatState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_relative_time_format(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_relative_time_format_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&RelativeTimeFormatState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.RelativeTimeFormat object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::intl_relative_time_format_state)
    }

    pub(crate) fn realm_intl_relative_time_format_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self
            .objects
            .get(intl.relative_time_format_prototype)
            .is_none()
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.RelativeTimeFormat.prototype intrinsic",
                index: intl.relative_time_format_prototype.index(),
                generation: intl.relative_time_format_prototype.generation(),
            });
        }
        Ok(intl.relative_time_format_prototype)
    }

    pub(crate) fn allocate_intl_plural_rules(
        &mut self,
        prototype: HeapReference,
        resolved: PluralRulesState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_plural_rules(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_plural_rules_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&PluralRulesState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.PluralRules object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::intl_plural_rules_state)
    }

    pub(crate) fn realm_intl_plural_rules_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.plural_rules_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.PluralRules.prototype intrinsic",
                index: intl.plural_rules_prototype.index(),
                generation: intl.plural_rules_prototype.generation(),
            });
        }
        Ok(intl.plural_rules_prototype)
    }

    pub(crate) fn intl_number_format_fallback_symbol(&self) -> Atom {
        self.predefined_atom(PredefinedAtom::IntlLegacyConstructedSymbol)
    }

    pub(crate) fn realm_intl_number_format_constructor(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        let function = self.functions.get(intl.number_format_constructor).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat constructor intrinsic",
                index: intl.number_format_constructor.index(),
                generation: intl.number_format_constructor.generation(),
            },
        )?;
        if !matches!(
            function.native(),
            Some(super::NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::IntlNumberFormatConstructor,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Intl.NumberFormat constructor intrinsic has the wrong implementation",
            });
        }
        Ok(intl.number_format_constructor)
    }

    pub(crate) fn realm_intl_date_time_format_constructor(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        let function = self
            .functions
            .get(intl.date_time_format_constructor)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat constructor intrinsic",
                index: intl.date_time_format_constructor.index(),
                generation: intl.date_time_format_constructor.generation(),
            })?;
        if !matches!(
            function.native(),
            Some(super::NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::IntlDateTimeFormatConstructor,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Intl.DateTimeFormat constructor intrinsic has the wrong implementation",
            });
        }
        Ok(intl.date_time_format_constructor)
    }

    pub(crate) fn allocate_intl_number_format(
        &mut self,
        prototype: HeapReference,
        resolved: NumberFormatState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_number_format(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_intl_date_time_format(
        &mut self,
        prototype: HeapReference,
        resolved: DateTimeFormatState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_date_time_format(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_date_time_format_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&DateTimeFormatState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .intl_date_time_format_state()
                    .map(|state| &state.resolved)
            })
    }

    pub(crate) fn intl_date_time_format_bound_format(
        &self,
        object: ObjectId,
    ) -> Result<Option<FunctionId>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .intl_date_time_format_state()
                    .and_then(|state| state.bound_format)
            })
    }

    pub(crate) fn set_intl_date_time_format_bound_format(
        &mut self,
        object: ObjectId,
        function: FunctionId,
    ) -> Result<(), crate::EngineFault> {
        if self.functions.get(function).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat bound format",
                index: function.index(),
                generation: function.generation(),
            });
        }
        let object = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat object",
                index: object.index(),
                generation: object.generation(),
            })?;
        let state = object.intl_date_time_format_state_mut().ok_or(
            crate::EngineFault::RuntimeInvariant {
                message: "bound format target is not an Intl.DateTimeFormat",
            },
        )?;
        state.bound_format = Some(function);
        Ok(())
    }

    pub(crate) fn allocate_intl_date_time_format_bound_format(
        &mut self,
        realm: RealmId,
        date_time_format: ObjectId,
    ) -> Result<FunctionId, crate::ExecutionError> {
        if self
            .intl_date_time_format_state(date_time_format)?
            .is_none()
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "bound format target is not an Intl.DateTimeFormat",
            }
            .into());
        }
        let target = self.realm_intl_date_time_format_format(realm)?;
        let prototype = self.realm_function_prototype(realm)?;
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
        let mut record = ObjectRecord::empty(Some(HeapReference::Function(prototype)));
        record
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::data(false, false, true),
                StoredValue::Number(1.0.into()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Name),
                PropertyLayout::data(false, false, true),
                StoredValue::String(JsString::empty()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let function = self
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::Bound(BoundFunction {
                    target,
                    bound_this: StoredValue::Object(date_time_format),
                    bound_arguments: Vec::new(),
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
        self.set_intl_date_time_format_bound_format(date_time_format, function)?;
        Ok(function)
    }

    pub(crate) fn intl_number_format_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&NumberFormatState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .intl_number_format_state()
                    .map(|state| &state.resolved)
            })
    }

    pub(crate) fn intl_number_format_bound_format(
        &self,
        object: ObjectId,
    ) -> Result<Option<FunctionId>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .intl_number_format_state()
                    .and_then(|state| state.bound_format)
            })
    }

    pub(crate) fn set_intl_number_format_bound_format(
        &mut self,
        object: ObjectId,
        function: FunctionId,
    ) -> Result<(), crate::EngineFault> {
        if self.functions.get(function).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat bound format",
                index: function.index(),
                generation: function.generation(),
            });
        }
        let object = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat object",
                index: object.index(),
                generation: object.generation(),
            })?;
        let state =
            object
                .intl_number_format_state_mut()
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "bound format target is not an Intl.NumberFormat",
                })?;
        state.bound_format = Some(function);
        Ok(())
    }

    pub(crate) fn allocate_intl_number_format_bound_format(
        &mut self,
        realm: RealmId,
        number_format: ObjectId,
    ) -> Result<FunctionId, crate::ExecutionError> {
        if self.intl_number_format_state(number_format)?.is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "bound format target is not an Intl.NumberFormat",
            }
            .into());
        }
        let target = self.realm_intl_number_format_format(realm)?;
        let prototype = self.realm_function_prototype(realm)?;
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
        let mut record = ObjectRecord::empty(Some(HeapReference::Function(prototype)));
        record
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::data(false, false, true),
                StoredValue::Number(1.0.into()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Name),
                PropertyLayout::data(false, false, true),
                StoredValue::String(JsString::empty()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let function = self
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::Bound(BoundFunction {
                    target,
                    bound_this: StoredValue::Object(number_format),
                    bound_arguments: Vec::new(),
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
        self.set_intl_number_format_bound_format(number_format, function)?;
        Ok(function)
    }

    pub(crate) fn allocate_intl_collator(
        &mut self,
        prototype: HeapReference,
        resolved: CollatorState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_collator(
                ObjectRecord::empty(Some(prototype)),
                resolved,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_collator_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&CollatorState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Collator object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| object.intl_collator_state().map(|state| &state.resolved))
    }

    pub(crate) fn intl_collator_bound_compare(
        &self,
        object: ObjectId,
    ) -> Result<Option<FunctionId>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Collator object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .intl_collator_state()
                    .and_then(|state| state.bound_compare)
            })
    }

    pub(crate) fn set_intl_collator_bound_compare(
        &mut self,
        object: ObjectId,
        function: FunctionId,
    ) -> Result<(), crate::EngineFault> {
        if self.functions.get(function).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Collator bound compare",
                index: function.index(),
                generation: function.generation(),
            });
        }
        let object = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Collator object",
                index: object.index(),
                generation: object.generation(),
            })?;
        let state =
            object
                .intl_collator_state_mut()
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "bound compare target is not an Intl.Collator",
                })?;
        state.bound_compare = Some(function);
        Ok(())
    }

    pub(crate) fn allocate_intl_collator_bound_compare(
        &mut self,
        realm: RealmId,
        collator: ObjectId,
    ) -> Result<FunctionId, crate::ExecutionError> {
        if self.intl_collator_state(collator)?.is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "bound compare target is not an Intl.Collator",
            }
            .into());
        }
        let target = self.realm_intl_collator_compare(realm)?;
        let prototype = self.realm_function_prototype(realm)?;
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
        let mut record = ObjectRecord::empty(Some(HeapReference::Function(prototype)));
        record
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::data(false, false, true),
                StoredValue::Number(2.0.into()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Name),
                PropertyLayout::data(false, false, true),
                StoredValue::String(JsString::empty()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let function = self
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::Bound(BoundFunction {
                    target,
                    bound_this: StoredValue::Object(collator),
                    bound_arguments: Vec::new(),
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
        self.set_intl_collator_bound_compare(collator, function)?;
        Ok(function)
    }

    pub(crate) fn allocate_intl_locale(
        &mut self,
        prototype: HeapReference,
        locale: JsString,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
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
            .insert_heap_object(HeapObject::intl_locale(
                ObjectRecord::empty(Some(prototype)),
                locale,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_locale_value(
        &self,
        object: ObjectId,
    ) -> Result<Option<&JsString>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::intl_locale_value)
    }

    pub(crate) fn realm_intl_locale_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.locale_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Locale.prototype intrinsic",
                index: intl.locale_prototype.index(),
                generation: intl.locale_prototype.generation(),
            });
        }
        Ok(intl.locale_prototype)
    }

    pub(crate) fn realm_intl_collator_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.collator_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Collator.prototype intrinsic",
                index: intl.collator_prototype.index(),
                generation: intl.collator_prototype.generation(),
            });
        }
        Ok(intl.collator_prototype)
    }

    pub(crate) fn realm_intl_number_format_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.number_format_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat.prototype intrinsic",
                index: intl.number_format_prototype.index(),
                generation: intl.number_format_prototype.generation(),
            });
        }
        Ok(intl.number_format_prototype)
    }

    pub(crate) fn realm_intl_date_time_format_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.date_time_format_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat.prototype intrinsic",
                index: intl.date_time_format_prototype.index(),
                generation: intl.date_time_format_prototype.generation(),
            });
        }
        Ok(intl.date_time_format_prototype)
    }

    fn realm_intl_number_format_format(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        let function = self.functions.get(intl.number_format_format).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "Intl.NumberFormat format intrinsic",
                index: intl.number_format_format.index(),
                generation: intl.number_format_format.generation(),
            },
        )?;
        if !matches!(
            function.native(),
            Some(super::NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::IntlNumberFormatFormat,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl.NumberFormat format intrinsic has the wrong implementation",
            });
        }
        Ok(intl.number_format_format)
    }

    fn realm_intl_date_time_format_format(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        let function = self.functions.get(intl.date_time_format_format).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "Intl.DateTimeFormat format intrinsic",
                index: intl.date_time_format_format.index(),
                generation: intl.date_time_format_format.generation(),
            },
        )?;
        if !matches!(
            function.native(),
            Some(super::NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::IntlDateTimeFormatFormat,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl.DateTimeFormat format intrinsic has the wrong implementation",
            });
        }
        Ok(intl.date_time_format_format)
    }

    fn realm_intl_collator_compare(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        let function =
            self.functions
                .get(intl.collator_compare)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Intl.Collator compare intrinsic",
                    index: intl.collator_compare.index(),
                    generation: intl.collator_compare.generation(),
                })?;
        if !matches!(
            function.native(),
            Some(super::NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::IntlCollatorCompare,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl.Collator compare intrinsic has the wrong implementation",
            });
        }
        Ok(intl.collator_compare)
    }
}
