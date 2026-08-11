//! Transaction-private materialization of typed intrinsic identities.

use super::{
    ARRAY_LENGTH_PROPERTY, ArrayState, FunctionId, HashMap, HeapObject, HeapReference, JsNumber,
    JsString, ObjectId, ObjectRecord, PredefinedAtom, PropertyKey, RealmBuildTransaction, RealmId,
    RealmIntrinsics, RealmState, RuntimeError, RuntimeResource, StoredValue, allocation_failed,
    families::{RealmIntrinsicSchema, is_declarative_function, is_declarative_object},
    reserved_record,
    schema::{
        IntrinsicFunctionId, IntrinsicFunctionSpec, IntrinsicIdentity, IntrinsicObjectId,
        IntrinsicObjectKind, PrototypeSpec,
    },
};
use crate::runtime::BoxedPrimitive;

/// Pre-reserved records whose capacities are derived from declarative holders.
pub(super) struct IntrinsicRecords {
    records: Vec<(IntrinsicIdentity, Option<ObjectRecord>)>,
}

impl IntrinsicRecords {
    pub(super) fn try_new(schema: &RealmIntrinsicSchema) -> Result<Self, RuntimeError> {
        let count = schema
            .objects()
            .len()
            .checked_add(schema.specs().len())
            .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(count)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, count))?;
        for object in schema.objects() {
            let identity = IntrinsicIdentity::Object(object.id);
            records.push((
                identity,
                Some(reserved_record(property_count(schema, identity))?),
            ));
        }
        for function in schema.specs() {
            let identity = IntrinsicIdentity::Function(function.id);
            let identity_properties =
                usize::from(function.identity_publication.is_automatic()).saturating_mul(2);
            let capacity = property_count(schema, identity)
                .checked_add(identity_properties)
                .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
            records.push((identity, Some(reserved_record(capacity)?)));
        }
        Ok(Self { records })
    }

    pub(super) fn take(&mut self, id: IntrinsicIdentity) -> ObjectRecord {
        self.records
            .iter_mut()
            .find_map(|(candidate, record)| (*candidate == id).then_some(record))
            .expect("every declarative intrinsic has one reserved record")
            .take()
            .expect("each declarative intrinsic record is consumed exactly once")
    }
}

fn property_count(schema: &RealmIntrinsicSchema, holder: IntrinsicIdentity) -> usize {
    schema
        .properties()
        .iter()
        .filter(|property| property.holder == holder)
        .count()
}

impl RealmBuildTransaction<'_> {
    /// Materializes the two root objects, then the callable bootstrap kernel
    /// whose native functions require the new Realm identity as their owner.
    pub(super) fn insert_realm_kernel(
        &mut self,
        schema: &RealmIntrinsicSchema,
        records: &mut IntrinsicRecords,
    ) -> RealmId {
        let object_prototype = self.insert_reserved_object(
            IntrinsicObjectId::ObjectPrototype,
            HeapObject::ordinary(records.take(IntrinsicIdentity::Object(
                IntrinsicObjectId::ObjectPrototype,
            ))),
        );
        let mut global_record =
            records.take(IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject));
        global_record.replace_prototype(Some(HeapReference::Object(object_prototype)));
        let global_object = self.insert_reserved_object(
            IntrinsicObjectId::GlobalObject,
            HeapObject::ordinary(global_record),
        );
        let realm = self
            .realms
            .try_insert(RealmState {
                object_prototype,
                global_object,
                intrinsics: RealmIntrinsics::Initializing,
                global_bindings: HashMap::new(),
                math_random_state: 1,
                module_registry: HashMap::new(),
            })
            .expect("the realm transaction reserved its realm slot");
        self.record_realm(realm);

        let function_prototype_id =
            IntrinsicFunctionId(super::NativeFunctionKind::FunctionPrototype);
        self.insert_reserved_native(
            realm,
            HeapReference::Object(object_prototype),
            super::NativeFunctionKind::FunctionPrototype,
            records.take(IntrinsicIdentity::Function(function_prototype_id)),
        );
        for function in schema
            .specs()
            .iter()
            .filter(|spec| super::families::is_kernel_function(spec.id))
            .filter(|spec| spec.id != function_prototype_id)
        {
            let prototype = self
                .resolve_intrinsic_prototype(function.prototype)
                .expect("native kernel functions always have a prototype");
            self.insert_reserved_native(
                realm,
                prototype,
                function.implementation,
                records.take(IntrinsicIdentity::Function(function.id)),
            );
        }
        realm
    }

    /// Materializes shell identities for the fully declarative ordinary-object
    /// families without retaining a parallel family graph.
    pub(super) fn insert_intrinsics(
        &mut self,
        realm: RealmId,
        schema: &RealmIntrinsicSchema,
        mut records: IntrinsicRecords,
    ) {
        for object in schema
            .objects()
            .iter()
            .filter(|spec| is_declarative_object(spec.id))
        {
            let mut record = records.take(IntrinsicIdentity::Object(object.id));
            record.replace_prototype(self.resolve_intrinsic_prototype(object.prototype));
            let object_value = match object.kind {
                IntrinsicObjectKind::Ordinary | IntrinsicObjectKind::DatePrototype => {
                    HeapObject::ordinary(record)
                }
                IntrinsicObjectKind::BooleanPrototype => {
                    HeapObject::with_boxed_primitive(record, BoxedPrimitive::Boolean(false))
                }
                IntrinsicObjectKind::NumberPrototype => HeapObject::with_boxed_primitive(
                    record,
                    BoxedPrimitive::Number(JsNumber::from_i32(0)),
                ),
                IntrinsicObjectKind::StringPrototype => HeapObject::with_boxed_primitive(
                    record,
                    BoxedPrimitive::String(JsString::empty()),
                ),
                IntrinsicObjectKind::ArrayPrototype => {
                    // Array's exotic `length` must exist before the object is
                    // materialized; every other descriptor is published from
                    // the ordinary schema path after all identities exist.
                    let length_key = PropertyKey::from_validated_atom(
                        self.atoms.predefined(PredefinedAtom::Length),
                    );
                    record
                        .append_data(
                            length_key,
                            ARRAY_LENGTH_PROPERTY,
                            StoredValue::Number(JsNumber::from_i32(0)),
                        )
                        .expect("the schema-derived Array prototype record is pre-reserved");
                    HeapObject::array(record, ArrayState::new(0))
                }
            };
            self.insert_reserved_object(object.id, object_value);
        }
        for function in schema
            .specs()
            .iter()
            .filter(|spec| is_declarative_function(spec.id))
        {
            let prototype = self
                .resolve_intrinsic_prototype(function.prototype)
                .expect("native intrinsic functions always have a prototype");
            self.insert_reserved_native(
                realm,
                prototype,
                function.implementation,
                records.take(IntrinsicIdentity::Function(function.id)),
            );
        }
    }

    fn resolve_intrinsic_prototype(&self, prototype: PrototypeSpec) -> Option<HeapReference> {
        match prototype {
            PrototypeSpec::Null => None,
            PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(id)) => {
                Some(HeapReference::Object(self.allocated.object(id)))
            }
            PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(id)) => {
                Some(HeapReference::Function(self.allocated.function(id)))
            }
        }
    }
}

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

    pub(super) fn assert_matches(&self, specs: &[IntrinsicFunctionSpec]) {
        assert_eq!(self.functions.len(), specs.len());
        for spec in specs {
            let _ = self.function(spec.id);
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
