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

//! Realm-owned objects, prototypes, boxed primitives, and ordinary properties.

use super::{
    Arc, Atom, AtomError, BoxedPrimitive, ExceptionKind, FunctionId, FunctionImplementation,
    HandleError, HandleKind, HeapObject, HeapReference, JsNumber, JsString, NativeFunctionKind,
    ObjectId, ObjectRecord, OwnProperty, PredefinedAtom, PropertyKey, PropertyLayout,
    PropertyLayoutKind, RealmId, RealmIntrinsics, ReleaseMailbox, Runtime, RuntimeResource,
    StoredValue, array_length_from_number, check_execution_limit, stale_heap_reference,
    usize_to_u64,
};

impl Runtime {
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

    pub(crate) fn predefined_property_key(&self, atom: PredefinedAtom) -> PropertyKey {
        PropertyKey::from_validated_atom(self.atoms.predefined(atom))
    }

    pub(crate) fn predefined_symbol_property_key(&self, atom: PredefinedAtom) -> PropertyKey {
        PropertyKey::from_validated_symbol(self.atoms.predefined(atom))
    }

    pub(crate) fn property_key_from_string(
        &mut self,
        value: &JsString,
    ) -> Result<PropertyKey, AtomError> {
        self.atoms.property_key_from_string(value)
    }

    pub(crate) fn property_key_from_symbol(&self, value: &Atom) -> Result<PropertyKey, AtomError> {
        self.atoms.property_key_from_symbol(value)
    }

    pub(crate) fn realm_function_prototype(
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
        match state.intrinsics {
            RealmIntrinsics::Initializing => Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Function intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready {
                function_prototype, ..
            } => {
                let function = self.functions.get(function_prototype).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "Function.prototype intrinsic",
                        index: function_prototype.index(),
                        generation: function_prototype.generation(),
                    },
                )?;
                let Some(native) = function.native() else {
                    return Err(crate::EngineFault::RuntimeInvariant {
                        message: "Function.prototype intrinsic is not native",
                    });
                };
                if native.realm != realm || native.kind != NativeFunctionKind::FunctionPrototype {
                    return Err(crate::EngineFault::RuntimeInvariant {
                        message: "Function.prototype intrinsic has the wrong native identity",
                    });
                }
                Ok(function_prototype)
            }
        }
    }

    pub(crate) fn realm_boolean_prototype(
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
        match state.intrinsics {
            RealmIntrinsics::Initializing => Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Boolean intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready { boolean, .. } => {
                let prototype = self.objects.get(boolean.prototype).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "Boolean.prototype intrinsic",
                        index: boolean.prototype.index(),
                        generation: boolean.prototype.generation(),
                    },
                )?;
                if prototype
                    .boxed_primitive()
                    .and_then(BoxedPrimitive::as_boolean)
                    != Some(false)
                {
                    return Err(crate::EngineFault::RuntimeInvariant {
                        message: "Boolean.prototype intrinsic has the wrong boxed value",
                    });
                }
                Ok(boolean.prototype)
            }
        }
    }

    pub(crate) fn realm_number_prototype(
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
        match state.intrinsics {
            RealmIntrinsics::Initializing => Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Number intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready { number, .. } => {
                let prototype = self.objects.get(number.prototype).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "Number.prototype intrinsic",
                        index: number.prototype.index(),
                        generation: number.prototype.generation(),
                    },
                )?;
                let valid_zero = prototype
                    .boxed_primitive()
                    .and_then(BoxedPrimitive::as_number)
                    .is_some_and(|value| value.same_value(JsNumber::from_i32(0)));
                if !valid_zero {
                    return Err(crate::EngineFault::RuntimeInvariant {
                        message: "Number.prototype intrinsic has the wrong boxed value",
                    });
                }
                Ok(number.prototype)
            }
        }
    }

    pub(crate) fn realm_string_prototype(
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
        match state.intrinsics {
            RealmIntrinsics::Initializing => Err(crate::EngineFault::RuntimeInvariant {
                message: "realm String intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready { string, .. } => {
                let prototype = self.objects.get(string.prototype).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "String.prototype intrinsic",
                        index: string.prototype.index(),
                        generation: string.prototype.generation(),
                    },
                )?;
                if prototype
                    .boxed_primitive()
                    .and_then(BoxedPrimitive::as_string)
                    .is_none_or(|value| !value.is_empty())
                {
                    return Err(crate::EngineFault::RuntimeInvariant {
                        message: "String.prototype intrinsic has the wrong boxed value",
                    });
                }
                Ok(string.prototype)
            }
        }
    }

    pub(crate) fn realm_array_prototype(
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
        let RealmIntrinsics::Ready { array, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Array intrinsics are not initialized",
            });
        };
        let prototype =
            self.objects
                .get(array.prototype)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Array.prototype intrinsic",
                    index: array.prototype.index(),
                    generation: array.prototype.generation(),
                })?;
        let array_length = prototype
            .array_state()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Array.prototype intrinsic has no array state",
            })?
            .length();
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        if !matches!(
            prototype.record.own_property(&length_key),
            Some(OwnProperty::Data {
                layout,
                value: StoredValue::Number(value),
            }) if layout == PropertyLayout::data(true, false, false)
                && array_length_from_number(value) == Some(array_length)
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Array.prototype intrinsic has an invalid length property",
            });
        }
        Ok(array.prototype)
    }

    pub(crate) fn realm_error_prototype(
        &self,
        realm: RealmId,
        kind: ExceptionKind,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let RealmIntrinsics::Ready { errors, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Error intrinsics are not initialized",
            });
        };
        let error_prototype =
            self.objects
                .get(errors.error)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Error.prototype intrinsic",
                    index: errors.error.index(),
                    generation: errors.error.generation(),
                })?;
        if error_prototype.record.prototype() != Some(HeapReference::Object(state.object_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Error.prototype intrinsic has the wrong prototype",
            });
        }
        let prototype = errors.prototype(kind);
        let native_error =
            self.objects
                .get(prototype)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "native Error prototype intrinsic",
                    index: prototype.index(),
                    generation: prototype.generation(),
                })?;
        if native_error.record.prototype() != Some(HeapReference::Object(errors.error)) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "native Error prototype intrinsic has the wrong prototype",
            });
        }
        Ok(prototype)
    }

    pub(crate) fn materialize_error_object(
        &mut self,
        realm: RealmId,
        kind: ExceptionKind,
        message: JsString,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_error_prototype(realm, kind)?;
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let mut record = ObjectRecord::empty(Some(HeapReference::Object(prototype)));
        record
            .try_reserve_data(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Message),
                PropertyLayout::data(true, false, true),
                StoredValue::String(message),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let object = self
            .objects
            .try_insert(HeapObject::error(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties += 1;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn function_realm(
        &self,
        function: FunctionId,
    ) -> Result<RealmId, crate::EngineFault> {
        let function = self
            .functions
            .get(function)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "function",
                index: function.index(),
                generation: function.generation(),
            })?;
        match &function.implementation {
            FunctionImplementation::Bytecode(bytecode) => {
                self.code.get(bytecode.code).map(|code| code.realm).ok_or(
                    crate::EngineFault::StaleHeapEdge {
                        edge: "installed code",
                        index: bytecode.code.index(),
                        generation: bytecode.code.generation(),
                    },
                )
            }
            FunctionImplementation::Native(native) => Ok(native.realm),
        }
    }

    pub(crate) fn replace_prototype_checked(
        &mut self,
        target: HeapReference,
        prototype: Option<HeapReference>,
    ) -> Result<bool, crate::EngineFault> {
        self.object_record(target)?;
        let mut current = prototype;
        let mut remaining = self
            .functions
            .len()
            .saturating_add(self.objects.len())
            .saturating_add(1);
        while let Some(reference) = current {
            if reference == target {
                return Ok(false);
            }
            if remaining == 0 {
                return Err(crate::EngineFault::RuntimeInvariant {
                    message: "ordinary prototype chain contains a cycle",
                });
            }
            remaining -= 1;
            current = self.object_record(reference)?.prototype();
        }
        self.object_record_mut(target)?.replace_prototype(prototype);
        self.collection_pending = true;
        Ok(true)
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
        self.allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
    }

    pub(crate) fn allocate_ordinary_object_with_prototype(
        &mut self,
        prototype: HeapReference,
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
            .objects
            .try_insert(HeapObject::ordinary(ObjectRecord::empty(Some(prototype))))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_boxed_boolean_with_prototype(
        &mut self,
        prototype: HeapReference,
        value: bool,
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
            .objects
            .try_insert(HeapObject::with_boxed_primitive(
                ObjectRecord::empty(Some(prototype)),
                BoxedPrimitive::Boolean(value),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_boxed_boolean(
        &mut self,
        realm: RealmId,
        value: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_boolean_prototype(realm)?;
        self.allocate_boxed_boolean_with_prototype(HeapReference::Object(prototype), value)
    }

    pub(crate) fn boxed_boolean(
        &self,
        object: ObjectId,
    ) -> Result<Option<bool>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .boxed_primitive()
                    .and_then(BoxedPrimitive::as_boolean)
            })
    }

    pub(crate) fn allocate_boxed_number_with_prototype(
        &mut self,
        prototype: HeapReference,
        value: JsNumber,
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
            .objects
            .try_insert(HeapObject::with_boxed_primitive(
                ObjectRecord::empty(Some(prototype)),
                BoxedPrimitive::Number(value),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_boxed_number(
        &mut self,
        realm: RealmId,
        value: JsNumber,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_number_prototype(realm)?;
        self.allocate_boxed_number_with_prototype(HeapReference::Object(prototype), value)
    }

    pub(crate) fn boxed_number(
        &self,
        object: ObjectId,
    ) -> Result<Option<JsNumber>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| object.boxed_primitive().and_then(BoxedPrimitive::as_number))
    }

    pub(crate) fn allocate_boxed_string_with_prototype(
        &mut self,
        prototype: HeapReference,
        value: JsString,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let mut record = ObjectRecord::empty(Some(prototype));
        record
            .try_reserve_data(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::data(false, false, false),
                StoredValue::Number(JsNumber::from_i32(
                    i32::try_from(value.len())
                        .expect("QuickJS String length always fits in a signed 32-bit integer"),
                )),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let object = self
            .objects
            .try_insert(HeapObject::with_boxed_primitive(
                record,
                BoxedPrimitive::String(value),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties += 1;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_boxed_string(
        &mut self,
        realm: RealmId,
        value: JsString,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_string_prototype(realm)?;
        self.allocate_boxed_string_with_prototype(HeapReference::Object(prototype), value)
    }

    pub(crate) fn boxed_string(
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
            .map(|object| object.boxed_primitive().and_then(BoxedPrimitive::as_string))
    }

    pub(crate) fn boxed_string_code_unit_at(
        &self,
        object: ObjectId,
        index: u32,
    ) -> Result<Option<u16>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| {
                object
                    .boxed_primitive()
                    .and_then(|value| value.string_code_unit_at(index))
            })
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

    pub(crate) fn append_accessor_property(
        &mut self,
        reference: HeapReference,
        key: PropertyKey,
        layout: PropertyLayout,
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    ) -> Result<(), crate::ExecutionError> {
        if layout.kind() != PropertyLayoutKind::Accessor {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "accessor insertion received a data-property layout",
            }
            .into());
        }
        if self.object_record(reference)?.own_property(&key).is_some() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "accessor insertion targeted an existing own property",
            }
            .into());
        }
        for function in [getter, setter].into_iter().flatten() {
            if !self.functions.contains(function) {
                return Err(stale_heap_reference(HeapReference::Function(function)).into());
            }
        }
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(1),
        )?;
        self.object_record_mut(reference)?
            .append_accessor(key, layout, getter, setter)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        self.object_properties += 1;
        self.collection_pending = true;
        Ok(())
    }
}
