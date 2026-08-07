//! `ArrayBuffer` allocation, byte-data ownership, and logical byte accounting.

use super::{
    ArrayBufferState, HeapObject, HeapReference, ObjectId, ObjectRecord, Runtime, RuntimeResource,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};

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
    /// block. Shared buffers use the same view backing representation as
    /// `ArrayBuffer`, but retain their distinct brand and are never detached.
    pub(crate) fn allocate_shared_array_buffer(
        &mut self,
        prototype: HeapReference,
        byte_length: usize,
        max_byte_length: Option<usize>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        self.allocate_buffer(prototype, byte_length, max_byte_length, true)
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
            let data = state.data().ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer copy source is detached",
            })?;
            let end =
                source_offset
                    .checked_add(count)
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer copy source range overflowed",
                    })?;
            let range =
                data.get(source_offset..end)
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer copy source range escaped its backing store",
                    })?;
            let mut copied = Vec::new();
            copied.try_reserve_exact(range.len()).map_err(|_| {
                crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ArrayBufferBytes,
                    additional: range.len(),
                }
            })?;
            copied.extend_from_slice(range);
            copied
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
        let data = state
            .data_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer copy target is detached",
            })?;
        let target_end =
            target_offset
                .checked_add(count)
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer copy target range overflowed",
                })?;
        let target = data.get_mut(target_offset..target_end).ok_or(
            crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer copy target range escaped its backing store",
            },
        )?;
        target.copy_from_slice(&bytes);
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
        let data = state
            .data_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer forward copy source is detached",
            })?;
        let source_end =
            source_offset
                .checked_add(count)
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer forward copy source range overflowed",
                })?;
        let target_end =
            target_offset
                .checked_add(count)
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer forward copy target range overflowed",
                })?;
        if source_end > data.len() || target_end > data.len() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer forward copy range escaped its backing store",
            }
            .into());
        }
        for index in 0..count {
            let byte = data[source_offset + index];
            data[target_offset + index] = byte;
        }
        Ok(())
    }

    /// Replaces a live resizable buffer's data block after all checks and
    /// allocation work have succeeded. The operation is failure-atomic.
    pub(crate) fn resize_array_buffer(
        &mut self,
        object: ObjectId,
        new_byte_length: usize,
    ) -> Result<(), crate::ExecutionError> {
        let (old_byte_length, maximum, source) = {
            let state =
                self.array_buffer_state(object)?
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer resize lost its internal slots",
                    })?;
            let source = state.data().ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer resize received a detached buffer",
            })?;
            let maximum =
                state
                    .resizable_max_byte_length()
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "ArrayBuffer resize received a fixed-length buffer",
                    })?;
            let mut copied = Vec::new();
            copied.try_reserve_exact(source.len()).map_err(|_| {
                crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ArrayBufferBytes,
                    additional: source.len(),
                }
            })?;
            copied.extend_from_slice(source);
            (source.len(), maximum, copied)
        };
        debug_assert!(new_byte_length <= maximum);
        let observed_bytes = self
            .array_buffer_bytes
            .saturating_sub(usize_to_u64(old_byte_length))
            .saturating_add(usize_to_u64(new_byte_length));
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
            let data = state.data().ok_or(crate::EngineFault::RuntimeInvariant {
                message: "ArrayBuffer transfer received a detached buffer",
            })?;
            let mut copied = Vec::new();
            copied.try_reserve_exact(data.len()).map_err(|_| {
                crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ArrayBufferBytes,
                    additional: data.len(),
                }
            })?;
            copied.extend_from_slice(data);
            (
                data.len(),
                preserve_resizability
                    .then(|| state.resizable_max_byte_length())
                    .flatten(),
                copied,
            )
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
        let target = self
            .insert_heap_object(HeapObject::array_buffer(
                ObjectRecord::empty(Some(prototype)),
                ArrayBufferState::new(data, max_byte_length),
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
