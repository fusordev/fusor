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
    Arc, Atom, AtomError, BindingCell, BoxedPrimitive, ErrorIntrinsicKind, ErrorObjectKind,
    ExceptionKind, FunctionId, FunctionImplementation, HandleError, HandleKind, HeapFunction,
    HeapObject, HeapReference, JsBigInt, JsNumber, JsString, NativeFunction, NativeFunctionKind,
    ObjectId, ObjectRecord, OwnProperty, PredefinedAtom, PropertyDeletion, PropertyKey,
    PropertyLayout, PropertyLayoutKind, Rc, RealmId, RealmIntrinsics, ReleaseMailbox, Runtime,
    RuntimeResource, SetPrototypeOutcome, SlotValue, StoredValue, array_length_from_number,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};

#[derive(Clone, Copy)]
struct ArgumentsIntrinsics {
    object_prototype: ObjectId,
    array_values: FunctionId,
    throw_type_error: FunctionId,
}

impl Runtime {
    pub(crate) fn value_has_is_html_dda(
        &self,
        value: &StoredValue,
    ) -> Result<bool, crate::EngineFault> {
        let reference = match value {
            StoredValue::Function(function) => HeapReference::Function(*function),
            StoredValue::Object(object) => HeapReference::Object(*object),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_) => return Ok(false),
        };
        self.object_record(reference).map(ObjectRecord::is_html_dda)
    }

    pub(crate) fn to_boolean(&self, value: &StoredValue) -> Result<bool, crate::EngineFault> {
        if let Some(result) = value.primitive_to_boolean() {
            return Ok(result);
        }
        Ok(!self.value_has_is_html_dda(value)?)
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

    pub(crate) fn realm_async_from_sync_iterator_prototype(
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
                message: "realm iterator intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready { iterators, .. } => {
                let prototype = iterators.async_from_sync_iterator_prototype;
                if self.objects.get(prototype).is_none() {
                    return Err(crate::EngineFault::StaleHeapEdge {
                        edge: "AsyncFromSyncIteratorPrototype intrinsic",
                        index: prototype.index(),
                        generation: prototype.generation(),
                    });
                }
                Ok(prototype)
            }
        }
    }

    pub(crate) fn realm_async_from_sync_iterator_next(
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
                message: "realm iterator intrinsics are not initialized",
            }),
            RealmIntrinsics::Ready { iterators, .. } => {
                let function = iterators.async_from_sync_iterator_next;
                if self.functions.get(function).is_none() {
                    return Err(crate::EngineFault::StaleHeapEdge {
                        edge: "AsyncFromSyncIterator next intrinsic",
                        index: function.index(),
                        generation: function.generation(),
                    });
                }
                Ok(function)
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

    /// Returns the realm's intrinsic `%Array%` constructor.
    pub(crate) fn realm_array_constructor(
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
        let RealmIntrinsics::Ready { array, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Array intrinsics are not initialized",
            });
        };
        let function =
            self.functions
                .get(array.constructor)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Array constructor intrinsic",
                    index: array.constructor.index(),
                    generation: array.constructor.generation(),
                })?;
        if !matches!(
            function.native(),
            Some(NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::ArrayConstructor,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Array constructor intrinsic has the wrong implementation",
            });
        }
        Ok(array.constructor)
    }

    /// Returns the realm's intrinsic `%RegExp%` constructor.
    pub(crate) fn realm_regexp_constructor(
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
        let RealmIntrinsics::Ready { regexp, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm RegExp intrinsics are not initialized",
            });
        };
        let function =
            self.functions
                .get(regexp.constructor)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "RegExp constructor intrinsic",
                    index: regexp.constructor.index(),
                    generation: regexp.constructor.generation(),
                })?;
        if !matches!(
            function.native(),
            Some(NativeFunction {
                realm: function_realm,
                kind: NativeFunctionKind::RegExpConstructor,
            }) if *function_realm == realm
        ) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm RegExp constructor intrinsic has the wrong implementation",
            });
        }
        Ok(regexp.constructor)
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
            .insert_heap_object(HeapObject::error(ObjectRecord::empty(Some(prototype))))
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

    pub(crate) fn error_object_kind(
        &self,
        object: ObjectId,
    ) -> Result<Option<ErrorObjectKind>, crate::EngineFault> {
        let object_record = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Error object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if !object_record.is_error() {
            return Ok(None);
        }
        let mut prototype = object_record.record.prototype();
        let mut remaining = self.objects.len().saturating_add(1);
        while remaining != 0 {
            remaining -= 1;
            let Some(HeapReference::Object(current)) = prototype else {
                return Ok(None);
            };
            for (_, state) in self.realms.iter() {
                let RealmIntrinsics::Ready { errors, .. } = state.intrinsics else {
                    continue;
                };
                for kind in ErrorIntrinsicKind::ALL {
                    if errors.intrinsic(kind).prototype == current {
                        return Ok(Some(kind.public_kind()));
                    }
                }
            }
            prototype = self
                .objects
                .get(current)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "Error prototype chain",
                    index: current.index(),
                    generation: current.generation(),
                })?
                .record
                .prototype();
        }
        Err(crate::EngineFault::RuntimeInvariant {
            message: "Error prototype chain is cyclic",
        })
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
            .insert_heap_object(HeapObject::error(record))
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
                FunctionImplementation::PromiseResolving(resolving) => {
                    return Ok(resolving.realm);
                }
                FunctionImplementation::PromiseCapabilityExecutor(executor) => {
                    return Ok(executor.realm);
                }
                FunctionImplementation::PromiseFinally(function) => {
                    return Ok(function.realm());
                }
                FunctionImplementation::PromiseCombinatorElement(function) => {
                    return Ok(function.realm);
                }
                FunctionImplementation::Proxy(proxy) => return Ok(proxy.realm),
                FunctionImplementation::ProxyRevoker(revoker) => return Ok(revoker.realm),
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
        if matches!(target, HeapReference::Object(object) if self.realms.iter().any(|(_, realm)| realm.object_prototype == object))
        {
            return Ok(SetPrototypeOutcome::NonExtensible);
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
        let dense_deleted = match (target, key.as_index()) {
            (HeapReference::Object(object), Some(index)) => self
                .objects
                .get_mut(object)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "object",
                    index: object.index(),
                    generation: object.generation(),
                })?
                .array_state_mut()
                .is_some_and(|state| state.delete_dense(index)),
            (HeapReference::Function(_), _) | (HeapReference::Object(_), None) => false,
        };
        let deletion = if dense_deleted {
            PropertyDeletion::Deleted
        } else {
            self.object_record_mut(target)?.delete_own_property(key)
        };
        if deletion == PropertyDeletion::Deleted {
            if let HeapReference::Object(object) = target {
                self.detach_mapped_arguments_property(object, key)?;
            }
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
        let interner = Rc::clone(&self.shape_interner);
        let record = match reference {
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
        }?;
        record.adopt_shape_interner(interner);
        Ok(record)
    }

    pub(crate) fn insert_heap_object(
        &mut self,
        mut object: HeapObject,
    ) -> Result<ObjectId, std::collections::TryReserveError> {
        object
            .record
            .adopt_shape_interner(Rc::clone(&self.shape_interner));
        self.objects.try_insert(object)
    }

    pub(crate) fn insert_heap_function(
        &mut self,
        mut function: HeapFunction,
    ) -> Result<FunctionId, std::collections::TryReserveError> {
        function
            .object
            .adopt_shape_interner(Rc::clone(&self.shape_interner));
        self.functions.try_insert(function)
    }

    pub(crate) fn canonicalize_all_shapes(&mut self) {
        let interner = Rc::clone(&self.shape_interner);
        for (_, function) in self.functions.iter_mut() {
            function.object.adopt_shape_interner(Rc::clone(&interner));
        }
        for (_, object) in self.objects.iter_mut() {
            object.record.adopt_shape_interner(Rc::clone(&interner));
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
            .insert_heap_object(HeapObject::ordinary(ObjectRecord::empty(Some(prototype))))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    /// Allocates an ordinary object whose prototype may be absent.
    ///
    /// `Object.create(null)` and `Object.groupBy` need a null prototype. A
    /// prototype-less object is genuinely useful as a bare dictionary, so the
    /// absence is represented rather than substituted.
    pub(crate) fn allocate_ordinary_object_with_optional_prototype(
        &mut self,
        prototype: Option<HeapReference>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if let Some(prototype) = prototype {
            return self.allocate_ordinary_object_with_prototype(prototype);
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
            .insert_heap_object(HeapObject::ordinary(ObjectRecord::empty(None)))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    /// Allocates the frozen null-prototype object created by `JSON.rawJSON`.
    ///
    /// The object and its single data property are prepared before publication,
    /// so either resource limit can reject the complete allocation without
    /// leaving a partially initialized heap object behind.
    pub(crate) fn allocate_raw_json_object(
        &mut self,
        text: JsString,
    ) -> Result<ObjectId, crate::ExecutionError> {
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

        let mut record = ObjectRecord::empty(None);
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::RawJson),
                PropertyLayout::data(false, true, false),
                StoredValue::String(text),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record.prevent_extensions();
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::raw_json(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(1);
        self.collection_pending = true;
        Ok(object)
    }

    /// Tests the unforgeable `[[IsRawJSON]]` object brand.
    pub(crate) fn is_raw_json_object(&self, object: ObjectId) -> Result<bool, crate::EngineFault> {
        self.objects.get(object).map(HeapObject::is_raw_json).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            },
        )
    }

    /// Returns the immutable source text carried by a branded raw-JSON object.
    pub(crate) fn raw_json_text(
        &self,
        object: ObjectId,
    ) -> Result<Option<JsString>, crate::EngineFault> {
        let object = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if !object.is_raw_json() {
            return Ok(None);
        }
        let key = self.predefined_property_key(PredefinedAtom::RawJson);
        match object.record.own_property(&key) {
            Some(OwnProperty::Data {
                value: StoredValue::String(text),
                ..
            }) => Ok(Some(text)),
            Some(OwnProperty::Data { .. } | OwnProperty::Accessor { .. }) | None => {
                Err(crate::EngineFault::RuntimeInvariant {
                    message: "branded raw JSON object lost its immutable source text",
                })
            }
        }
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
            .insert_heap_object(HeapObject::with_boxed_primitive(
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
            .insert_heap_object(HeapObject::with_boxed_primitive(
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
            .insert_heap_object(HeapObject::with_boxed_primitive(
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
            .insert_heap_object(HeapObject::with_boxed_primitive(
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

    /// Reads an internal private-name slot without walking prototypes or
    /// invoking Proxy traps. Private elements are deliberately not ordinary
    /// property accesses, even though their storage shares the ordinary shape
    /// backing for GC and transition interning.
    pub(crate) fn private_own_property(
        &self,
        reference: HeapReference,
        key: &PropertyKey,
    ) -> Result<Option<OwnProperty>, crate::ExecutionError> {
        Ok(self.object_record(reference)?.own_property(key))
    }

    /// Replaces an existing writable internal private-name data slot. The
    /// operation never falls back to a prototype, setter, or Proxy trap.
    ///
    /// `None` means the slot is absent, `Some(false)` means it is an
    /// immutable private method, and `Some(true)` reports a completed write.
    pub(crate) fn replace_private_own_data_property(
        &mut self,
        reference: HeapReference,
        key: &PropertyKey,
        value: StoredValue,
    ) -> Result<Option<bool>, crate::ExecutionError> {
        let writable = match self.object_record(reference)?.own_property(key) {
            Some(OwnProperty::Data { layout, .. }) => {
                layout
                    .writable()
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "private data storage has no writable attribute",
                    })?
            }
            Some(OwnProperty::Accessor { .. }) => {
                return Err(crate::EngineFault::RuntimeInvariant {
                    message: "private data storage became an accessor",
                }
                .into());
            }
            None => return Ok(None),
        };
        if !writable {
            return Ok(Some(false));
        }
        let replaced = self
            .object_record_mut(reference)?
            .replace_existing_data(key, value);
        debug_assert!(replaced, "checked private data slot remains present");
        self.collection_pending = true;
        Ok(Some(true))
    }

    /// Defines one private accessor half without invoking ordinary property
    /// machinery. Getter/setter halves merge only with the same own private
    /// accessor slot; an existing data element is a duplicate private name.
    ///
    /// `getter` selects the supplied function's accessor half. `Ok(false)`
    /// reports an existing private data slot or an already-installed matching
    /// accessor half, while `Ok(true)` publishes or completes a getter/setter
    /// pair without walking prototypes or invoking Proxy traps.
    pub(crate) fn define_private_accessor_property(
        &mut self,
        reference: HeapReference,
        key: PropertyKey,
        function: FunctionId,
        is_getter: bool,
    ) -> Result<bool, crate::ExecutionError> {
        if !self.functions.contains(function) {
            return Err(stale_heap_reference(HeapReference::Function(function)).into());
        }
        let existing = self.object_record(reference)?.own_property(&key);
        let is_new = existing.is_none();
        let layout = PropertyLayout::accessor(false, false);
        let (getter_function, setter_function) = match existing {
            None => {
                if is_getter {
                    (Some(function), None)
                } else {
                    (None, Some(function))
                }
            }
            Some(OwnProperty::Data { .. }) => return Ok(false),
            Some(OwnProperty::Accessor { getter, setter, .. }) => {
                if (is_getter && getter.is_some()) || (!is_getter && setter.is_some()) {
                    return Ok(false);
                }
                if is_getter {
                    (Some(function), setter)
                } else {
                    (getter, Some(function))
                }
            }
        };
        if is_new {
            self.append_accessor_property(
                reference,
                key,
                layout,
                getter_function,
                setter_function,
            )?;
        } else if self
            .object_record_mut(reference)?
            .replace_existing_with_accessor(&key, layout, getter_function, setter_function)
            .is_none()
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "checked private accessor slot disappeared during definition",
            }
            .into());
        } else {
            self.collection_pending = true;
        }
        Ok(true)
    }

    /// Creates the ordinary arguments object used by strict functions.
    ///
    /// The object carries the `[[ParameterMap]]` brand for
    /// `Object.prototype.toString`, but uses ordinary property internal
    /// methods because its parameter map is `undefined`.
    pub(crate) fn allocate_unmapped_arguments_object(
        &mut self,
        realm: RealmId,
        values: Vec<StoredValue>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let property_count =
            values
                .len()
                .checked_add(3)
                .ok_or(crate::ExecutionError::LimitExceeded {
                    resource: RuntimeResource::ObjectProperties,
                    limit: self.limits.max_object_properties,
                    observed: u64::MAX,
                })?;
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
        let length =
            u32::try_from(values.len()).map_err(|_| crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: u64::from(u32::MAX - 1),
                observed: usize_to_u64(values.len()),
            })?;
        let intrinsics = self.arguments_intrinsics(realm)?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let record =
            self.build_unmapped_arguments_record(intrinsics, values, length, property_count)?;

        let object = self
            .insert_heap_object(HeapObject::arguments(record))
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

    /// Creates the arguments exotic object used by a sloppy function with a
    /// simple parameter list.
    pub(crate) fn allocate_mapped_arguments_object(
        &mut self,
        realm: RealmId,
        callee: FunctionId,
        values: Vec<StoredValue>,
        mapped_indices: &[u32],
    ) -> Result<ObjectId, crate::ExecutionError> {
        let active_mapping_count = mapped_indices
            .iter()
            .take_while(|index| usize::try_from(**index).is_ok_and(|index| index < values.len()))
            .count();
        let mapping_len = mapped_indices[..active_mapping_count]
            .last()
            .copied()
            .map_or(Ok(0_usize), |index| {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(1))
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "mapped arguments domain fits usize",
                    })
            })?;
        let property_count =
            values
                .len()
                .checked_add(3)
                .ok_or(crate::ExecutionError::LimitExceeded {
                    resource: RuntimeResource::ObjectProperties,
                    limit: self.limits.max_object_properties,
                    observed: u64::MAX,
                })?;
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
        check_execution_limit(
            RuntimeResource::BindingCells,
            self.limits.max_binding_cells,
            usize_to_u64(self.cells.len()).saturating_add(usize_to_u64(active_mapping_count)),
        )?;
        let length =
            u32::try_from(values.len()).map_err(|_| crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: u64::from(u32::MAX - 1),
                observed: usize_to_u64(values.len()),
            })?;
        let intrinsics = self.arguments_intrinsics(realm)?;
        if !self.functions.contains(callee) {
            return Err(stale_heap_reference(HeapReference::Function(callee)).into());
        }

        let mut mapped_values = Vec::new();
        mapped_values
            .try_reserve_exact(active_mapping_count)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: active_mapping_count,
            })?;
        for &index in mapped_indices.iter().take(active_mapping_count) {
            let value = values
                .get(index as usize)
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "active mapped argument was supplied",
                })?;
            mapped_values.push((index, value.duplicate()));
        }
        let record =
            self.build_mapped_arguments_record(intrinsics, callee, values, length, property_count)?;

        self.commit_mapped_arguments_object(record, mapped_values, mapping_len, property_count)
    }

    fn commit_mapped_arguments_object(
        &mut self,
        record: ObjectRecord,
        mapped_values: Vec<(u32, StoredValue)>,
        mapping_len: usize,
        property_count: usize,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let mapped_count = mapped_values.len();
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.cells.try_reserve(mapped_count).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: mapped_count,
            }
        })?;
        let mut parameter_map = Vec::new();
        parameter_map.try_reserve_exact(mapping_len).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: mapping_len,
            }
        })?;
        parameter_map.resize(mapping_len, None);
        let mut rollback_cells = Vec::new();
        rollback_cells
            .try_reserve_exact(mapped_count)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: mapped_count,
            })?;

        for (index, value) in mapped_values {
            let Ok(cell) = self.cells.try_insert(BindingCell {
                value: SlotValue::Value(value),
            }) else {
                for cell in rollback_cells {
                    let removed = self.cells.remove(cell);
                    debug_assert!(removed.is_some());
                }
                return Err(crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::BindingCells,
                    additional: 1,
                });
            };
            rollback_cells.push(cell);
            let Some(target) = parameter_map.get_mut(index as usize) else {
                for cell in rollback_cells {
                    let removed = self.cells.remove(cell);
                    debug_assert!(removed.is_some());
                }
                return Err(crate::EngineFault::RuntimeInvariant {
                    message: "mapped argument index belongs to its parameter map",
                }
                .into());
            };
            if target.replace(cell).is_some() {
                for cell in rollback_cells {
                    let removed = self.cells.remove(cell);
                    debug_assert!(removed.is_some());
                }
                return Err(crate::EngineFault::RuntimeInvariant {
                    message: "mapped argument indices are unique",
                }
                .into());
            }
        }

        let Ok(object) =
            self.insert_heap_object(HeapObject::mapped_arguments(record, parameter_map))
        else {
            for cell in rollback_cells {
                let removed = self.cells.remove(cell);
                debug_assert!(removed.is_some());
            }
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        self.object_properties = self
            .object_properties
            .saturating_add(usize_to_u64(property_count));
        self.collection_pending = true;
        Ok(object)
    }

    fn arguments_intrinsics(
        &self,
        realm: RealmId,
    ) -> Result<ArgumentsIntrinsics, crate::ExecutionError> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let RealmIntrinsics::Ready {
            throw_type_error,
            iterators,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm arguments intrinsics are not initialized",
            }
            .into());
        };
        for function in [throw_type_error, iterators.array_values] {
            if !self.functions.contains(function) {
                return Err(stale_heap_reference(HeapReference::Function(function)).into());
            }
        }
        if !self.objects.contains(state.object_prototype) {
            return Err(stale_heap_reference(HeapReference::Object(state.object_prototype)).into());
        }
        Ok(ArgumentsIntrinsics {
            object_prototype: state.object_prototype,
            array_values: iterators.array_values,
            throw_type_error,
        })
    }

    fn build_unmapped_arguments_record(
        &self,
        intrinsics: ArgumentsIntrinsics,
        values: Vec<StoredValue>,
        length: u32,
        property_count: usize,
    ) -> Result<ObjectRecord, crate::ExecutionError> {
        let mut record =
            ObjectRecord::empty(Some(HeapReference::Object(intrinsics.object_prototype)));
        record
            .try_reserve_data(property_count)
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let iterator_key = self.predefined_symbol_property_key(PredefinedAtom::SymbolIterator);
        let callee_key = self.predefined_property_key(PredefinedAtom::Callee);
        let ordinary = PropertyLayout::data(true, false, true);
        record
            .append_data(
                length_key,
                ordinary,
                StoredValue::Number(JsNumber::from_u32(length)),
            )
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        for (index, value) in values.into_iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| crate::EngineFault::RuntimeInvariant {
                message: "preflighted arguments index fits u32",
            })?;
            let index =
                crate::ArrayIndex::new(index).ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "arguments index does not use the array-length sentinel",
                })?;
            record
                .append_data(
                    PropertyKey::from_index(index),
                    PropertyLayout::data(true, true, true),
                    value,
                )
                .map_err(|_| arguments_property_allocation_failed(property_count))?;
        }
        record
            .append_data(
                iterator_key,
                ordinary,
                StoredValue::Function(intrinsics.array_values),
            )
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        record
            .append_accessor(
                callee_key,
                PropertyLayout::accessor(false, false),
                Some(intrinsics.throw_type_error),
                Some(intrinsics.throw_type_error),
            )
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        Ok(record)
    }

    fn build_mapped_arguments_record(
        &self,
        intrinsics: ArgumentsIntrinsics,
        callee: FunctionId,
        values: Vec<StoredValue>,
        length: u32,
        property_count: usize,
    ) -> Result<ObjectRecord, crate::ExecutionError> {
        let mut record =
            ObjectRecord::empty(Some(HeapReference::Object(intrinsics.object_prototype)));
        record
            .try_reserve_data(property_count)
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        for (index, value) in values.into_iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| crate::EngineFault::RuntimeInvariant {
                message: "preflighted arguments index fits u32",
            })?;
            let index =
                crate::ArrayIndex::new(index).ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "arguments index does not use the array-length sentinel",
                })?;
            record
                .append_data(
                    PropertyKey::from_index(index),
                    PropertyLayout::data(true, true, true),
                    value,
                )
                .map_err(|_| arguments_property_allocation_failed(property_count))?;
        }
        let ordinary = PropertyLayout::data(true, false, true);
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                ordinary,
                StoredValue::Number(JsNumber::from_u32(length)),
            )
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        record
            .append_data(
                self.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                ordinary,
                StoredValue::Function(intrinsics.array_values),
            )
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Callee),
                ordinary,
                StoredValue::Function(callee),
            )
            .map_err(|_| arguments_property_allocation_failed(property_count))?;
        Ok(record)
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

fn arguments_property_allocation_failed(additional: usize) -> crate::ExecutionError {
    crate::ExecutionError::AllocationFailed {
        resource: RuntimeResource::ObjectProperties,
        additional,
    }
}
