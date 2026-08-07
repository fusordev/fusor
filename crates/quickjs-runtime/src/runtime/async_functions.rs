//! Realm-owned async function intrinsic access.

use super::{HeapReference, ObjectId, RealmId, RealmIntrinsics, Runtime};

impl Runtime {
    pub(crate) fn realm_async_function_prototype(
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
            async_functions,
            function_prototype,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm async function intrinsics are not initialized",
            });
        };
        let prototype = self.objects.get(async_functions.function_prototype).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "AsyncFunction.prototype intrinsic",
                index: async_functions.function_prototype.index(),
                generation: async_functions.function_prototype.generation(),
            },
        )?;
        if prototype.record.prototype() != Some(HeapReference::Function(function_prototype)) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "AsyncFunction.prototype has the wrong prototype",
            });
        }
        Ok(async_functions.function_prototype)
    }
}
