//! Temporal object allocation and internal-slot access.

use temporal_rs::{
    Duration, Instant, PlainDate, PlainDateTime, PlainMonthDay, PlainTime, PlainYearMonth,
    ZonedDateTime,
};

use super::{
    HeapObject, HeapReference, ObjectId, ObjectRecord, RealmId, RealmIntrinsics, Runtime,
    RuntimeResource, check_execution_limit, stale_heap_reference, usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_temporal_duration(
        &mut self,
        prototype: HeapReference,
        duration: Duration,
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
            .insert_heap_object(HeapObject::temporal_duration(
                ObjectRecord::empty(Some(prototype)),
                duration,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_duration(
        &self,
        object: ObjectId,
    ) -> Result<Option<Duration>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_duration_value)
    }

    pub(crate) fn realm_temporal_duration_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self.objects.get(temporal.duration_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.Duration.prototype intrinsic",
                index: temporal.duration_prototype.index(),
                generation: temporal.duration_prototype.generation(),
            });
        }
        Ok(temporal.duration_prototype)
    }

    pub(crate) fn allocate_temporal_instant(
        &mut self,
        prototype: HeapReference,
        instant: Instant,
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
            .insert_heap_object(HeapObject::temporal_instant(
                ObjectRecord::empty(Some(prototype)),
                instant,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_instant(
        &self,
        object: ObjectId,
    ) -> Result<Option<Instant>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_instant_value)
    }

    pub(crate) fn realm_temporal_instant_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self.objects.get(temporal.instant_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.Instant.prototype intrinsic",
                index: temporal.instant_prototype.index(),
                generation: temporal.instant_prototype.generation(),
            });
        }
        Ok(temporal.instant_prototype)
    }

    pub(crate) fn allocate_temporal_plain_date(
        &mut self,
        prototype: HeapReference,
        date: PlainDate,
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
            .insert_heap_object(HeapObject::temporal_plain_date(
                ObjectRecord::empty(Some(prototype)),
                date,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_plain_date(
        &self,
        object: ObjectId,
    ) -> Result<Option<PlainDate>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_plain_date_value)
    }

    pub(crate) fn realm_temporal_plain_date_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self.objects.get(temporal.plain_date_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.PlainDate.prototype intrinsic",
                index: temporal.plain_date_prototype.index(),
                generation: temporal.plain_date_prototype.generation(),
            });
        }
        Ok(temporal.plain_date_prototype)
    }

    pub(crate) fn allocate_temporal_plain_date_time(
        &mut self,
        prototype: HeapReference,
        date_time: PlainDateTime,
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
            .insert_heap_object(HeapObject::temporal_plain_date_time(
                ObjectRecord::empty(Some(prototype)),
                date_time,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_plain_date_time(
        &self,
        object: ObjectId,
    ) -> Result<Option<PlainDateTime>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_plain_date_time_value)
    }

    pub(crate) fn realm_temporal_plain_date_time_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self
            .objects
            .get(temporal.plain_date_time_prototype)
            .is_none()
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.PlainDateTime.prototype intrinsic",
                index: temporal.plain_date_time_prototype.index(),
                generation: temporal.plain_date_time_prototype.generation(),
            });
        }
        Ok(temporal.plain_date_time_prototype)
    }

    pub(crate) fn allocate_temporal_plain_time(
        &mut self,
        prototype: HeapReference,
        time: PlainTime,
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
            .insert_heap_object(HeapObject::temporal_plain_time(
                ObjectRecord::empty(Some(prototype)),
                time,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_plain_time(
        &self,
        object: ObjectId,
    ) -> Result<Option<PlainTime>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_plain_time_value)
    }

    pub(crate) fn realm_temporal_plain_time_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self.objects.get(temporal.plain_time_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.PlainTime.prototype intrinsic",
                index: temporal.plain_time_prototype.index(),
                generation: temporal.plain_time_prototype.generation(),
            });
        }
        Ok(temporal.plain_time_prototype)
    }

    pub(crate) fn allocate_temporal_plain_month_day(
        &mut self,
        prototype: HeapReference,
        month_day: PlainMonthDay,
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
            .insert_heap_object(HeapObject::temporal_plain_month_day(
                ObjectRecord::empty(Some(prototype)),
                month_day,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_plain_month_day(
        &self,
        object: ObjectId,
    ) -> Result<Option<PlainMonthDay>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_plain_month_day_value)
    }

    pub(crate) fn realm_temporal_plain_month_day_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self
            .objects
            .get(temporal.plain_month_day_prototype)
            .is_none()
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.PlainMonthDay.prototype intrinsic",
                index: temporal.plain_month_day_prototype.index(),
                generation: temporal.plain_month_day_prototype.generation(),
            });
        }
        Ok(temporal.plain_month_day_prototype)
    }

    pub(crate) fn allocate_temporal_plain_year_month(
        &mut self,
        prototype: HeapReference,
        year_month: PlainYearMonth,
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
            .insert_heap_object(HeapObject::temporal_plain_year_month(
                ObjectRecord::empty(Some(prototype)),
                year_month,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_plain_year_month(
        &self,
        object: ObjectId,
    ) -> Result<Option<PlainYearMonth>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_plain_year_month_value)
    }

    pub(crate) fn realm_temporal_plain_year_month_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self
            .objects
            .get(temporal.plain_year_month_prototype)
            .is_none()
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.PlainYearMonth.prototype intrinsic",
                index: temporal.plain_year_month_prototype.index(),
                generation: temporal.plain_year_month_prototype.generation(),
            });
        }
        Ok(temporal.plain_year_month_prototype)
    }

    pub(crate) fn allocate_temporal_zoned_date_time(
        &mut self,
        prototype: HeapReference,
        date_time: ZonedDateTime,
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
            .insert_heap_object(HeapObject::temporal_zoned_date_time(
                ObjectRecord::empty(Some(prototype)),
                date_time,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_zoned_date_time(
        &self,
        object: ObjectId,
    ) -> Result<Option<ZonedDateTime>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_zoned_date_time_value)
    }

    pub(crate) fn realm_temporal_zoned_date_time_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self
            .objects
            .get(temporal.zoned_date_time_prototype)
            .is_none()
        {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.ZonedDateTime.prototype intrinsic",
                index: temporal.zoned_date_time_prototype.index(),
                generation: temporal.zoned_date_time_prototype.generation(),
            });
        }
        Ok(temporal.zoned_date_time_prototype)
    }
}
