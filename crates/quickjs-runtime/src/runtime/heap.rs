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
    Arc, Atom, AtomError, BoxedPrimitive, ErrorIntrinsicKind, ExceptionKind, FunctionId,
    FunctionImplementation, HandleError, HandleKind, HeapObject, HeapReference, IntegrityLevel,
    JsBigInt, JsNumber, JsString, NativeFunctionKind, ObjectId, ObjectRecord, OwnProperty,
    PredefinedAtom, PropertyDeletion, PropertyKey, PropertyLayout, PropertyLayoutKind, RealmId,
    RealmIntrinsics, ReleaseMailbox, Runtime, RuntimeResource, SetPrototypeOutcome, StoredValue,
    array_length_from_number, check_execution_limit, stale_heap_reference, usize_to_u64,
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

    /// Returns the realm's `BigInt.prototype`.
    ///
    /// Unlike the Number and String prototypes this one is an ordinary object
    /// rather than a wrapper: `BigInt.prototype` carries no `[[BigIntData]]`,
    /// which is why `BigInt.prototype.valueOf()` throws instead of returning
    /// `0n` (`quickjs.c:56014-56027`).
    pub(crate) fn realm_bigint_prototype(
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
                message: "realm BigInt intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready { bigint, .. } => {
                if self.objects.get(bigint.prototype).is_none() {
                    return Err(crate::EngineFault::StaleHeapEdge {
                        edge: "BigInt.prototype intrinsic",
                        index: bigint.prototype.index(),
                        generation: bigint.prototype.generation(),
                    });
                }
                Ok(bigint.prototype)
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

    pub(crate) fn realm_error_intrinsic_prototype(
        &self,
        realm: RealmId,
        kind: ErrorIntrinsicKind,
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
        let error = errors.intrinsic(ErrorIntrinsicKind::Error);
        let error_prototype =
            self.objects
                .get(error.prototype)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Error.prototype intrinsic",
                    index: error.prototype.index(),
                    generation: error.prototype.generation(),
                })?;
        if error_prototype.record.prototype() != Some(HeapReference::Object(state.object_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Error.prototype intrinsic has the wrong prototype",
            });
        }
        let intrinsic = errors.intrinsic(kind);
        let prototype =
            self.objects
                .get(intrinsic.prototype)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "native Error prototype intrinsic",
                    index: intrinsic.prototype.index(),
                    generation: intrinsic.prototype.generation(),
                })?;
        let expected_parent = if kind == ErrorIntrinsicKind::Error {
            HeapReference::Object(state.object_prototype)
        } else {
            HeapReference::Object(error.prototype)
        };
        if prototype.record.prototype() != Some(expected_parent) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "native Error prototype intrinsic has the wrong prototype",
            });
        }
        Ok(intrinsic.prototype)
    }

    pub(crate) fn realm_error_prototype(
        &self,
        realm: RealmId,
        kind: ExceptionKind,
    ) -> Result<ObjectId, crate::EngineFault> {
        self.realm_error_intrinsic_prototype(realm, ErrorIntrinsicKind::from_exception_kind(kind))
    }

    pub(crate) fn allocate_error_with_prototype(
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
            .try_insert(HeapObject::error(ObjectRecord::empty(Some(prototype))))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn define_error_data_property(
        &mut self,
        object: ObjectId,
        atom: PredefinedAtom,
        value: StoredValue,
    ) -> Result<(), crate::ExecutionError> {
        if !matches!(
            atom,
            PredefinedAtom::Message
                | PredefinedAtom::Cause
                | PredefinedAtom::Errors
                | PredefinedAtom::Stack
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Error construction received an unsupported own data property",
            }
            .into());
        }
        if !self.is_error_object(object)? {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Error data property target is not an Error object",
            }
            .into());
        }
        let key = self.predefined_property_key(atom);
        if self
            .object_record(HeapReference::Object(object))?
            .own_property(&key)
            .is_some()
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Error data property already exists",
            }
            .into());
        }
        self.append_data_property(
            HeapReference::Object(object),
            key,
            PropertyLayout::data(true, false, true),
            value,
        )
    }

    pub(crate) fn is_error_object(&self, object: ObjectId) -> Result<bool, crate::EngineFault> {
        self.objects.get(object).map(HeapObject::is_error).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            },
        )
    }

    pub(crate) fn materialize_error_object(
        &mut self,
        realm: RealmId,
        kind: ExceptionKind,
        message: JsString,
        stack: Option<JsString>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_error_prototype(realm, kind)?;
        let property_count = 1_usize.saturating_add(usize::from(stack.is_some()));
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties
                .saturating_add(usize_to_u64(property_count)),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let mut record = ObjectRecord::empty(Some(HeapReference::Object(prototype)));
        record.try_reserve_data(property_count).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: property_count,
            }
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
        if let Some(stack) = stack {
            record
                .append_data(
                    self.predefined_property_key(PredefinedAtom::Stack),
                    PropertyLayout::data(true, false, true),
                    StoredValue::String(stack),
                )
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ObjectProperties,
                    additional: 1,
                })?;
        }
        let object = self
            .objects
            .try_insert(HeapObject::error(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self
            .object_properties
            .saturating_add(usize_to_u64(property_count));
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn function_realm(
        &self,
        mut function: FunctionId,
    ) -> Result<RealmId, crate::EngineFault> {
        let mut remaining = self.functions.len().saturating_add(1);
        loop {
            let node =
                self.functions
                    .get(function)
                    .ok_or_else(|| crate::EngineFault::StaleHeapEdge {
                        edge: "function",
                        index: function.index(),
                        generation: function.generation(),
                    })?;
            match &node.implementation {
                FunctionImplementation::Bytecode(bytecode) => {
                    return self.code.get(bytecode.code).map(|code| code.realm).ok_or(
                        crate::EngineFault::StaleHeapEdge {
                            edge: "installed code",
                            index: bytecode.code.index(),
                            generation: bytecode.code.generation(),
                        },
                    );
                }
                FunctionImplementation::Native(native) => return Ok(native.realm),
                FunctionImplementation::Bound(bound) => {
                    if remaining == 0 {
                        return Err(crate::EngineFault::RuntimeInvariant {
                            message: "bound-function target chain exceeds the heap size",
                        });
                    }
                    remaining -= 1;
                    function = bound.target;
                }
            }
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

    /// Applies ECMAScript `OrdinarySetPrototypeOf`.
    ///
    /// The same-value case succeeds before the extensibility test, so
    /// re-assigning the current prototype of a non-extensible object is
    /// permitted, exactly as the pinned `JS_SetPrototypeInternal` does
    /// (`quickjs.c:7940`). A non-extensible object otherwise rejects, and a
    /// prototype chain that would reach the target rejects as a cycle.
    pub(crate) fn set_prototype_of(
        &mut self,
        target: HeapReference,
        prototype: Option<HeapReference>,
    ) -> Result<SetPrototypeOutcome, crate::EngineFault> {
        let record = self.object_record(target)?;
        if record.prototype() == prototype {
            return Ok(SetPrototypeOutcome::Complete);
        }
        if !record.is_extensible() {
            return Ok(SetPrototypeOutcome::NonExtensible);
        }
        if self.replace_prototype_checked(target, prototype)? {
            Ok(SetPrototypeOutcome::Complete)
        } else {
            Ok(SetPrototypeOutcome::CyclicPrototype)
        }
    }

    /// Applies ECMAScript `OrdinaryPreventExtensions`, which always succeeds
    /// for an ordinary object.
    pub(crate) fn prevent_extensions(
        &mut self,
        target: HeapReference,
    ) -> Result<(), crate::EngineFault> {
        self.object_record_mut(target)?.prevent_extensions();
        Ok(())
    }

    /// Returns `[[IsExtensible]]` for an ordinary object.
    pub(crate) fn is_extensible(&self, target: HeapReference) -> Result<bool, crate::EngineFault> {
        Ok(self.object_record(target)?.is_extensible())
    }

    /// Applies ECMAScript `SetIntegrityLevel`.
    ///
    /// Both levels first prevent extensions, then clamp every own property's
    /// attributes. Freezing additionally clears `writable` on data properties;
    /// an accessor has no `writable` attribute, so only its `configurable`
    /// attribute changes (`quickjs.c:40549`).
    pub(crate) fn set_integrity_level(
        &mut self,
        target: HeapReference,
        level: IntegrityLevel,
    ) -> Result<(), crate::EngineFault> {
        let record = self.object_record_mut(target)?;
        record.prevent_extensions();
        match level {
            IntegrityLevel::Sealed => record.seal_own_properties(),
            IntegrityLevel::Frozen => record.freeze_own_properties(),
        }
        Ok(())
    }

    /// Applies ECMAScript `TestIntegrityLevel`.
    ///
    /// An extensible object is neither sealed nor frozen regardless of its
    /// properties, so an empty but extensible object reports `false`.
    pub(crate) fn tests_integrity_level(
        &self,
        target: HeapReference,
        level: IntegrityLevel,
    ) -> Result<bool, crate::EngineFault> {
        let record = self.object_record(target)?;
        if record.is_extensible() {
            return Ok(false);
        }
        Ok(match level {
            IntegrityLevel::Sealed => record.own_properties_are_sealed(),
            IntegrityLevel::Frozen => record.own_properties_are_frozen(),
        })
    }

    /// Applies ECMAScript `[[Delete]]` for an ordinary object, keeping the
    /// runtime's own-property accounting in step with the removal.
    ///
    /// An array's `length` property is not configurable, so a delete of it
    /// reports `NotConfigurable`. Deleting an element never shortens the
    /// cached array length; the element simply becomes an absent property,
    /// which is what ECMAScript and the pinned `delete_property`
    /// (`quickjs.c:9311`) both do.
    pub(crate) fn delete_own_property(
        &mut self,
        target: HeapReference,
        key: &PropertyKey,
    ) -> Result<PropertyDeletion, crate::EngineFault> {
        let deletion = self.object_record_mut(target)?.delete_own_property(key);
        if deletion == PropertyDeletion::Deleted {
            self.object_properties = self.object_properties.saturating_sub(1);
            self.collection_pending = true;
        }
        Ok(deletion)
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

    /// Allocates an `Object(bigint)` wrapper.
    pub(crate) fn allocate_boxed_bigint_with_prototype(
        &mut self,
        prototype: HeapReference,
        value: Arc<JsBigInt>,
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
                BoxedPrimitive::BigInt(value),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    /// Allocates an `Object(bigint)` wrapper inheriting `BigInt.prototype`.
    pub(crate) fn allocate_boxed_bigint(
        &mut self,
        realm: RealmId,
        value: Arc<JsBigInt>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_bigint_prototype(realm)?;
        self.allocate_boxed_bigint_with_prototype(HeapReference::Object(prototype), value)
    }

    /// Returns the wrapped `BigInt`, or `None` when `object` is not one.
    pub(crate) fn boxed_bigint(
        &self,
        object: ObjectId,
    ) -> Result<Option<Arc<JsBigInt>>, crate::EngineFault> {
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
                    .and_then(BoxedPrimitive::as_bigint)
                    .map(Arc::clone)
            })
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

    /// Appends one own property described by a completed descriptor decision.
    ///
    /// This is the insertion half of `OrdinaryDefineOwnProperty`: the caller
    /// has already validated compatibility and extensibility.
    pub(crate) fn append_own_property(
        &mut self,
        reference: HeapReference,
        key: PropertyKey,
        property: OwnProperty,
    ) -> Result<(), crate::ExecutionError> {
        match property {
            OwnProperty::Data { layout, value } => {
                self.append_data_property(reference, key, layout, value)
            }
            OwnProperty::Accessor {
                layout,
                getter,
                setter,
            } => self.append_accessor_property(reference, key, layout, getter, setter),
        }
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
