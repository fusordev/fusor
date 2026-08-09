//! `ArrayBuffer` allocation, byte-data ownership, and logical byte accounting.

use super::{
    Arc, ArrayBufferState, HeapObject, HeapReference, ObjectId, ObjectRecord, Runtime,
    RuntimeResource, check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::shared_array_buffer::SharedDataBlock;

impl Runtime {
    /// Allocates one zero-initialized ECMAScript `ArrayBuffer` data block.
    ///
    /// `max_byte_length` is present exactly for resizable buffers and must be
    /// no smaller than `byte_length`. Callers have already performed the
    /// observable `ToIndex` conversions and report user-facing range errors.
    pub(crate) fn allocate_array_buffer(
        &mut self,
        prototype: HeapReference,
        byte_length: usize,
        max_byte_length: Option<usize>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        self.allocate_buffer(prototype, byte_length, max_byte_length, false)
    }

    /// Allocates one zero-initialized ECMAScript `SharedArrayBuffer` data
    /// block. Shared buffers retain a mutex-protected data block that can be
    /// imported by another runtime; they keep a distinct brand and never
    /// detach.
    pub(crate) fn allocate_shared_array_buffer(
        &mut self,
        prototype: HeapReference,
        byte_length: usize,
        max_byte_length: Option<usize>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        self.allocate_buffer(prototype, byte_length, max_byte_length, true)
    }

    /// Creates a realm-local `SharedArrayBuffer` object for an existing Shared
    /// Data Block imported from another runtime agent.
    pub(crate) fn allocate_shared_array_buffer_block(
        &mut self,
        prototype: HeapReference,
        block: Arc<SharedDataBlock>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        let byte_length = block.byte_length();
        let reservation = block.max_byte_length();
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        let observed_bytes = self
            .array_buffer_bytes
            .saturating_add(usize_to_u64(reservation));
        check_execution_limit(
            RuntimeResource::ArrayBufferBytes,
            self.limits.max_array_buffer_bytes,
            observed_bytes,
        )?;
        debug_assert!(byte_length <= reservation);
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::array_buffer(
                ObjectRecord::empty(Some(prototype)),
                ArrayBufferState::shared_block(block),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.array_buffer_bytes = observed_bytes;
        self.collection_pending = true;
        Ok(object)
    }

    /// Atomically grows one Shared Data Block. The maximum backing-store
    /// charge was reserved when this runtime's `SharedArrayBuffer` object was
    /// allocated, so growth changes no local accounting.
    pub(crate) fn grow_shared_array_buffer(
        &mut self,
        object: ObjectId,
        new_byte_length: usize,
    ) -> Result<bool, crate::ExecutionError> {
        let block = self
            .array_buffer_state(object)?
            .and_then(|state| state.shared_data_block())
            .cloned()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "SharedArrayBuffer grow lost its Shared Data Block",
            })?;
        let current = block.byte_length();
        block
            .grow(new_byte_length)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ArrayBufferBytes,
                additional: new_byte_length.saturating_sub(current),
            })
    }

    /// Allocates one fixed-length immutable `ArrayBuffer` from an already
    /// copied byte block. Immutable data is installed only after all runtime
    /// limits and arena reservations have succeeded, so no writable view can
    /// observe the target during construction.
    pub(crate) fn allocate_immutable_array_buffer(
        &mut self,
        prototype: HeapReference,
        data: Vec<u8>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        let byte_length = data.len();
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        let observed_bytes = self
            .array_buffer_bytes
            .saturating_add(usize_to_u64(byte_length));
        check_execution_limit(
            RuntimeResource::ArrayBufferBytes,
            self.limits.max_array_buffer_bytes,
            observed_bytes,
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::array_buffer(
                ObjectRecord::empty(Some(prototype)),
                ArrayBufferState::immutable(data),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.array_buffer_bytes = observed_bytes;
        self.collection_pending = true;
        Ok(object)
    }

    fn allocate_buffer(
        &mut self,
        prototype: HeapReference,
        byte_length: usize,
        max_byte_length: Option<usize>,
        shared: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        debug_assert!(max_byte_length.is_none_or(|maximum| byte_length <= maximum));
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        // A resizable or growable buffer makes its maximum allocation
        // observable at construction. Reserve that capacity against the
        // runtime limit before creating the object, so impossible maxima are
        // reported synchronously instead of becoming a later host failure.
        let reservation = max_byte_length.unwrap_or(byte_length);
        let observed_reservation = self
            .array_buffer_bytes
            .saturating_add(usize_to_u64(reservation));
        check_execution_limit(
            RuntimeResource::ArrayBufferBytes,
            self.limits.max_array_buffer_bytes,
            observed_reservation,
        )?;
        let charged_bytes = if shared { reservation } else { byte_length };
        let observed_bytes = self
            .array_buffer_bytes
            .saturating_add(usize_to_u64(charged_bytes));
        check_execution_limit(
            RuntimeResource::ArrayBufferBytes,
            self.limits.max_array_buffer_bytes,
            observed_bytes,
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let data = zeroed_byte_data(byte_length)?;
        let object = self
            .insert_heap_object(HeapObject::array_buffer(
                ObjectRecord::empty(Some(prototype)),
                if shared {
                    ArrayBufferState::shared(data, max_byte_length)
                } else {
                    ArrayBufferState::new(data, max_byte_length)
                },
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.array_buffer_bytes = observed_bytes;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn array_buffer_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&ArrayBufferState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::array_buffer_state)
    }

    pub(crate) fn realm_array_buffer_prototype(
        &self,
        realm: super::RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let super::RealmIntrinsics::Ready { array_buffer, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm ArrayBuffer intrinsics are not initialized",
            });
        };
        if self.objects.get(array_buffer.prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "ArrayBuffer.prototype intrinsic",
                index: array_buffer.prototype.index(),
                generation: array_buffer.prototype.generation(),
            });
        }
        Ok(array_buffer.prototype)
    }

    pub(crate) fn realm_array_buffer_constructor(
        &self,
        realm: super::RealmId,
    ) -> Result<super::FunctionId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let super::RealmIntrinsics::Ready { array_buffer, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm ArrayBuffer intrinsics are not initialized",
            });
        };
        if self.functions.get(array_buffer.constructor).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "ArrayBuffer constructor intrinsic",
                index: array_buffer.constructor.index(),
                generation: array_buffer.constructor.generation(),
            });
        }
        Ok(array_buffer.constructor)
    }

    pub(crate) fn realm_shared_array_buffer_prototype(
        &self,
        realm: super::RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let super::RealmIntrinsics::Ready {
            shared_array_buffer,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm SharedArrayBuffer intrinsics are not initialized",
            });
        };
        if self.objects.get(shared_array_buffer.prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "SharedArrayBuffer.prototype intrinsic",
                index: shared_array_buffer.prototype.index(),
                generation: shared_array_buffer.prototype.generation(),
            });
        }
        Ok(shared_array_buffer.prototype)
    }

    pub(crate) fn realm_shared_array_buffer_constructor(
        &self,
        realm: super::RealmId,
    ) -> Result<super::FunctionId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let super::RealmIntrinsics::Ready {
            shared_array_buffer,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm SharedArrayBuffer intrinsics are not initialized",
            });
        };
        if self
            .functions
            .get(shared_array_buffer.constructor)
            .is_none()
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "SharedArrayBuffer constructor intrinsic",
                index: shared_array_buffer.constructor.index(),
                generation: shared_array_buffer.constructor.generation(),
            });
        }
        Ok(shared_array_buffer.constructor)
    }

    /// Copies a non-observable backing-store range after both `ArrayBuffer`
    /// operands have passed their post-call detached checks.
    pub(crate) fn copy_array_buffer_bytes(
        &mut self,
        source: ObjectId,
        source_offset: usize,
        target: ObjectId,
        count: usize,
    ) -> Result<(), crate::ExecutionError> {
        self.copy_array_buffer_bytes_to(source, source_offset, target, 0, count)
    }

    /// Copies one non-observable backing-store range into a byte offset of the
    /// target buffer. The temporary source copy gives overlapping ranges the
    /// `memmove` behavior required by typed-array copies over a shared buffer.
    pub(crate) fn copy_array_buffer_bytes_to(
        &mut self,
        source: ObjectId,
        source_offset: usize,
        target: ObjectId,
        target_offset: usize,
        count: usize,
    ) -> Result<(), crate::ExecutionError> {
        let bytes = {
            let state =
                self.array_buffer_state(source)?
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer copy source lost its internal slots",
                    })?;
            state
                .with_data(|data| {
                    let end = source_offset.checked_add(count).ok_or(
                        crate::EngineFault::RuntimeInvariant {
                            message: "ArrayBuffer copy source range overflowed",
                        },
                    )?;
                    let range = data.get(source_offset..end).ok_or(
                        crate::EngineFault::RuntimeInvariant {
                            message: "ArrayBuffer copy source range escaped its backing store",
                        },
                    )?;
                    let mut copied = Vec::new();
                    copied.try_reserve_exact(range.len()).map_err(|_| {
                        crate::ExecutionError::AllocationFailed {
                            resource: RuntimeResource::ArrayBufferBytes,
                            additional: range.len(),
                        }
                    })?;
                    copied.extend_from_slice(range);
                    Ok::<Vec<u8>, crate::ExecutionError>(copied)
                })
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer copy source is detached",
                })??
        };
        let state = self
            .objects
            .get_mut(target)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "ArrayBuffer copy target",
                index: target.index(),
                generation: target.generation(),
            })?
            .array_buffer_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer copy target lost its internal slots",
            })?;
        state
            .with_data_mut(|data| {
                let target_end = target_offset.checked_add(count).ok_or(
                    crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer copy target range overflowed",
                    },
                )?;
                let target = data.get_mut(target_offset..target_end).ok_or(
                    crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer copy target range escaped its backing store",
                    },
                )?;
                target.copy_from_slice(&bytes);
                Ok::<(), crate::EngineFault>(())
            })
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer copy target is detached",
            })??;
        Ok(())
    }

    /// Copies a non-observable range in ascending byte-index order.
    ///
    /// `%TypedArray%.prototype.slice` uses `SetValueInBuffer` for matching
    /// element types. If a species constructor returns another view over the
    /// source buffer, that algorithm observes each preceding write; it is not
    /// the overlapping-range `memmove` operation used by `set` and
    /// `copyWithin`.
    pub(crate) fn copy_array_buffer_bytes_forward(
        &mut self,
        source: ObjectId,
        source_offset: usize,
        target: ObjectId,
        target_offset: usize,
        count: usize,
    ) -> Result<(), crate::ExecutionError> {
        if source != target {
            return self.copy_array_buffer_bytes_to(
                source,
                source_offset,
                target,
                target_offset,
                count,
            );
        }

        let state = self
            .objects
            .get_mut(source)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "ArrayBuffer forward copy",
                index: source.index(),
                generation: source.generation(),
            })?
            .array_buffer_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer forward copy lost its internal slots",
            })?;
        state
            .with_data_mut(|data| {
                let source_end = source_offset.checked_add(count).ok_or(
                    crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer forward copy source range overflowed",
                    },
                )?;
                let target_end = target_offset.checked_add(count).ok_or(
                    crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer forward copy target range overflowed",
                    },
                )?;
                if source_end > data.len() || target_end > data.len() {
                    return Err(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer forward copy range escaped its backing store",
                    });
                }
                for index in 0..count {
                    let byte = data[source_offset + index];
                    data[target_offset + index] = byte;
                }
                Ok::<(), crate::EngineFault>(())
            })
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer forward copy source is detached",
            })??;
        Ok(())
    }

    /// Replaces a live resizable buffer's data block after all checks and
    /// allocation work have succeeded. The operation is failure-atomic.
    pub(crate) fn resize_array_buffer(
        &mut self,
        object: ObjectId,
        new_byte_length: usize,
    ) -> Result<(), crate::ExecutionError> {
        let (old_byte_length, old_charge, maximum, source) = {
            let state =
                self.array_buffer_state(object)?
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer resize lost its internal slots",
                    })?;
            debug_assert!(!state.is_shared());
            let maximum =
                state
                    .resizable_max_byte_length()
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer resize received a fixed-length buffer",
                    })?;
            let (old_byte_length, copied) = state
                .with_data(|source| {
                    let mut copied = Vec::new();
                    copied.try_reserve_exact(source.len()).map_err(|_| {
                        crate::ExecutionError::AllocationFailed {
                            resource: RuntimeResource::ArrayBufferBytes,
                            additional: source.len(),
                        }
                    })?;
                    copied.extend_from_slice(source);
                    Ok::<(usize, Vec<u8>), crate::ExecutionError>((source.len(), copied))
                })
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer resize received a detached buffer",
                })??;
            (
                old_byte_length,
                state.accounted_byte_length(),
                maximum,
                copied,
            )
        };
        debug_assert!(new_byte_length <= maximum);
        let observed_bytes = self
            .array_buffer_bytes
            .saturating_sub(usize_to_u64(old_charge))
            .saturating_add(usize_to_u64(if old_charge == old_byte_length {
                new_byte_length
            } else {
                old_charge
            }));
        check_execution_limit(
            RuntimeResource::ArrayBufferBytes,
            self.limits.max_array_buffer_bytes,
            observed_bytes,
        )?;
        let mut replacement = zeroed_byte_data(new_byte_length)?;
        let preserved = old_byte_length.min(new_byte_length);
        replacement[..preserved].copy_from_slice(&source[..preserved]);
        let state = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?
            .array_buffer_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer resize lost its internal slots",
            })?;
        let replaced =
            state
                .replace_data(replacement)
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer resize received a detached buffer",
                })?;
        debug_assert_eq!(replaced.len(), old_byte_length);
        self.array_buffer_bytes = observed_bytes;
        Ok(())
    }

    /// Copies an `ArrayBuffer` into a fresh buffer and detaches the source as
    /// one runtime transaction. No JavaScript can run between allocation and
    /// detachment, so resource accounting observes the post-transfer total
    /// instead of charging both temporary backing stores at once.
    pub(crate) fn transfer_array_buffer(
        &mut self,
        source: ObjectId,
        prototype: HeapReference,
        new_byte_length: usize,
        preserve_resizability: bool,
        immutable: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        let (old_byte_length, max_byte_length, source_data) = {
            let state =
                self.array_buffer_state(source)?
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer transfer lost its internal slots",
                    })?;
            let max_byte_length = preserve_resizability
                .then(|| state.resizable_max_byte_length())
                .flatten();
            let (old_byte_length, copied) = state
                .with_data(|data| {
                    let mut copied = Vec::new();
                    copied.try_reserve_exact(data.len()).map_err(|_| {
                        crate::ExecutionError::AllocationFailed {
                            resource: RuntimeResource::ArrayBufferBytes,
                            additional: data.len(),
                        }
                    })?;
                    copied.extend_from_slice(data);
                    Ok::<(usize, Vec<u8>), crate::ExecutionError>((data.len(), copied))
                })
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer transfer received a detached buffer",
                })??;
            (old_byte_length, max_byte_length, copied)
        };
        debug_assert!(max_byte_length.is_none_or(|maximum| new_byte_length <= maximum));
        let observed_bytes = self
            .array_buffer_bytes
            .saturating_sub(usize_to_u64(old_byte_length))
            .saturating_add(usize_to_u64(new_byte_length));
        check_execution_limit(
            RuntimeResource::ArrayBufferBytes,
            self.limits.max_array_buffer_bytes,
            observed_bytes,
        )?;
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
        let mut data = zeroed_byte_data(new_byte_length)?;
        let copy_length = old_byte_length.min(new_byte_length);
        data[..copy_length].copy_from_slice(&source_data[..copy_length]);
        debug_assert!(!immutable || max_byte_length.is_none());
        let target_state = if immutable {
            ArrayBufferState::immutable(data)
        } else {
            ArrayBufferState::new(data, max_byte_length)
        };
        let target = self
            .insert_heap_object(HeapObject::array_buffer(
                ObjectRecord::empty(Some(prototype)),
                target_state,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let detached = self
            .objects
            .get_mut(source)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "ArrayBuffer transfer source",
                index: source.index(),
                generation: source.generation(),
            })?
            .array_buffer_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer transfer lost its source internal slots",
            })?
            .detach()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer transfer source detached during transfer",
            })?;
        debug_assert_eq!(detached.len(), old_byte_length);
        self.array_buffer_bytes = observed_bytes;
        self.collection_pending = true;
        Ok(target)
    }
}

fn zeroed_byte_data(byte_length: usize) -> Result<Vec<u8>, crate::ExecutionError> {
    let mut data = Vec::new();
    data.try_reserve_exact(byte_length)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ArrayBufferBytes,
            additional: byte_length,
        })?;
    data.resize(byte_length, 0);
    Ok(data)
}
