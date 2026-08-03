//! Transaction-private materialization of typed intrinsic identities.

use super::{
    FunctionId, ObjectId, RuntimeError, RuntimeResource, allocation_failed,
    schema::{IntrinsicFunctionId, IntrinsicObjectId},
};

/// Fully typed identities allocated before the Realm becomes observable.
pub(super) struct AllocatedIntrinsics {
    objects: Vec<(IntrinsicObjectId, ObjectId)>,
    functions: Vec<(IntrinsicFunctionId, FunctionId)>,
}

impl AllocatedIntrinsics {
    pub(super) fn try_new(objects: usize, functions: usize) -> Result<Self, RuntimeError> {
        let mut object_slots = Vec::new();
        object_slots
            .try_reserve_exact(objects)
            .map_err(|_| allocation_failed(RuntimeResource::HeapObjects, objects))?;
        let mut function_slots = Vec::new();
        function_slots
            .try_reserve_exact(functions)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, functions))?;
        Ok(Self {
            objects: object_slots,
            functions: function_slots,
        })
    }

    pub(super) fn insert_object(&mut self, id: IntrinsicObjectId, object: ObjectId) {
        assert!(
            self.objects.iter().all(|(candidate, _)| *candidate != id),
            "an intrinsic object identity can be initialized only once"
        );
        debug_assert!(self.objects.len() < self.objects.capacity());
        self.objects.push((id, object));
    }

    pub(super) fn insert_function(&mut self, id: IntrinsicFunctionId, function: FunctionId) {
        assert!(
            self.functions.iter().all(|(candidate, _)| *candidate != id),
            "an intrinsic function identity can be initialized only once"
        );
        debug_assert!(self.functions.len() < self.functions.capacity());
        self.functions.push((id, function));
    }

    pub(super) fn object(&self, id: IntrinsicObjectId) -> ObjectId {
        self.objects
            .iter()
            .find_map(|(candidate, object)| (*candidate == id).then_some(*object))
            .expect("validated intrinsic object slot is initialized")
    }

    pub(super) fn function(&self, id: IntrinsicFunctionId) -> FunctionId {
        self.functions
            .iter()
            .find_map(|(candidate, function)| (*candidate == id).then_some(*function))
            .expect("validated intrinsic function slot is initialized")
    }

    pub(super) fn assert_complete(&self) {
        assert_eq!(
            self.objects.len(),
            IntrinsicObjectId::ALL.len(),
            "every mandatory intrinsic object slot must be initialized"
        );
        for id in IntrinsicObjectId::ALL {
            let _ = self.object(id);
        }
        assert_eq!(
            self.functions.len(),
            self.functions.capacity(),
            "every reserved intrinsic function slot must be initialized"
        );
        for &(id, function) in &self.functions {
            assert_eq!(self.function(id), function);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        arena::{Arena, RuntimeIdentity},
        ids::{FunctionMarker, ObjectMarker},
        runtime::NativeFunctionKind,
    };

    const RUNTIME: RuntimeIdentity = RuntimeIdentity::from_address(1);

    #[test]
    fn typed_slots_resolve_independently_from_arena_indices() {
        let mut allocated = AllocatedIntrinsics::try_new(1, 1).expect("table");
        let object = Arena::<ObjectMarker, _>::new(RUNTIME)
            .try_insert(())
            .expect("object ID");
        let function = Arena::<FunctionMarker, _>::new(RUNTIME)
            .try_insert(())
            .expect("function ID");
        let function_id = IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype);
        allocated.insert_object(IntrinsicObjectId::ObjectPrototype, object);
        allocated.insert_function(function_id, function);

        assert_eq!(allocated.object(IntrinsicObjectId::ObjectPrototype), object);
        assert_eq!(allocated.function(function_id), function);
    }

    #[test]
    #[should_panic(expected = "initialized only once")]
    fn duplicate_slots_are_rejected() {
        let mut allocated = AllocatedIntrinsics::try_new(2, 0).expect("table");
        let mut objects = Arena::<ObjectMarker, _>::new(RUNTIME);
        let first = objects.try_insert(()).expect("first object ID");
        let second = objects.try_insert(()).expect("second object ID");
        allocated.insert_object(IntrinsicObjectId::ObjectPrototype, first);
        allocated.insert_object(IntrinsicObjectId::ObjectPrototype, second);
    }
}
