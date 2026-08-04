//! Realm-owned synchronous generator intrinsic access.

use super::{HeapReference, ObjectId, RealmId, RealmIntrinsics, Runtime};

impl Runtime {
    pub(crate) fn realm_async_generator_function_prototype(
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
        let RealmIntrinsics::Ready {
            async_generators,
            function_prototype,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm async-generator intrinsics are not initialized",
            });
        };
        let prototype = self
            .objects
            .get(async_generators.function_prototype)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "AsyncGeneratorFunction.prototype intrinsic",
                index: async_generators.function_prototype.index(),
                generation: async_generators.function_prototype.generation(),
            })?;
        if prototype.record.prototype() != Some(HeapReference::Function(function_prototype)) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "AsyncGeneratorFunction.prototype has the wrong prototype",
            });
        }
        Ok(async_generators.function_prototype)
    }

    pub(crate) fn realm_async_generator_prototype(
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
        let RealmIntrinsics::Ready {
            async_generators,
            iterators,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm async-generator intrinsics are not initialized",
            });
        };
        let prototype = self
            .objects
            .get(async_generators.generator_prototype)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "AsyncGenerator.prototype intrinsic",
                index: async_generators.generator_prototype.index(),
                generation: async_generators.generator_prototype.generation(),
            })?;
        if prototype.record.prototype()
            != Some(HeapReference::Object(iterators.async_iterator_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "AsyncGenerator.prototype has the wrong prototype",
            });
        }
        Ok(async_generators.generator_prototype)
    }

    pub(crate) fn realm_generator_function_prototype(
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
        let RealmIntrinsics::Ready {
            generators,
            function_prototype,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm generator intrinsics are not initialized",
            });
        };
        let prototype = self.objects.get(generators.function_prototype).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "GeneratorFunction.prototype intrinsic",
                index: generators.function_prototype.index(),
                generation: generators.function_prototype.generation(),
            },
        )?;
        if prototype.record.prototype() != Some(HeapReference::Function(function_prototype)) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "GeneratorFunction.prototype has the wrong prototype",
            });
        }
        Ok(generators.function_prototype)
    }

    pub(crate) fn realm_generator_prototype(
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
        let RealmIntrinsics::Ready {
            generators,
            iterators,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm generator intrinsics are not initialized",
            });
        };
        let prototype = self.objects.get(generators.generator_prototype).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "Generator.prototype intrinsic",
                index: generators.generator_prototype.index(),
                generation: generators.generator_prototype.generation(),
            },
        )?;
        if prototype.record.prototype() != Some(HeapReference::Object(iterators.iterator_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Generator.prototype has the wrong prototype",
            });
        }
        Ok(generators.generator_prototype)
    }
}
