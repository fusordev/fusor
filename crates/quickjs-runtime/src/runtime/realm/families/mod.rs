//! Specification-ordered intrinsic family declarations.

mod array;
mod error;
mod globals;
mod iterator;
mod json;
mod kernel;
mod math;
mod primitives;
mod reflect;
mod string;
mod symbol;

use super::schema::{
    FamilyCardinality, IntrinsicDescriptorSpec, IntrinsicFunctionId, IntrinsicFunctionSpec,
    IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind,
    IntrinsicObjectSpec, IntrinsicPropertySpec, IntrinsicSchema, IntrinsicValueSpec, PrototypeSpec,
};
use super::validation::{SchemaValidationError, validate_intrinsic_schema};
use super::{NativeFunctionKind, RuntimeError, RuntimeResource, allocation_failed};

type ObjectSink<'a> = &'a mut dyn FnMut(IntrinsicObjectSpec);
type FunctionSink<'a> = &'a mut dyn FnMut(IntrinsicFunctionSpec);
type PropertySink<'a> = &'a mut dyn FnMut(IntrinsicPropertySpec);

/// Owned complete declaration table used before Runtime mutation.
pub(super) struct RealmFunctionSchema {
    objects: Vec<IntrinsicObjectSpec>,
    specs: Vec<IntrinsicFunctionSpec>,
    properties: Vec<IntrinsicPropertySpec>,
    mandatory_functions: Vec<IntrinsicFunctionId>,
}

impl RealmFunctionSchema {
    pub(super) fn try_new() -> Result<Self, RuntimeError> {
        let object_count = count_specs(visit_object_specs, RuntimeResource::HeapObjects)?;
        let function_count = count_specs(visit_function_specs, RuntimeResource::HeapFunctions)?;
        let property_count = count_specs(visit_property_specs, RuntimeResource::ObjectProperties)?;
        let mut objects = Vec::new();
        objects
            .try_reserve_exact(object_count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapObjects, object_count))?;
        visit_object_specs(&mut |spec| objects.push(spec));
        let mut specs = Vec::new();
        specs
            .try_reserve_exact(function_count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, function_count))?;
        visit_function_specs(&mut |spec| specs.push(spec));
        let mut properties = Vec::new();
        properties
            .try_reserve_exact(property_count)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, property_count))?;
        visit_property_specs(&mut |property| properties.push(property));
        let mut mandatory_functions = Vec::new();
        mandatory_functions
            .try_reserve_exact(function_count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, function_count))?;
        mandatory_functions.extend(specs.iter().map(|spec| spec.id));
        Ok(Self {
            objects,
            specs,
            properties,
            mandatory_functions,
        })
    }

    pub(super) fn specs(&self) -> &[IntrinsicFunctionSpec] {
        &self.specs
    }

    pub(super) fn objects(&self) -> &[IntrinsicObjectSpec] {
        &self.objects
    }

    pub(super) fn spec(&self, id: IntrinsicFunctionId) -> &IntrinsicFunctionSpec {
        self.specs
            .iter()
            .find(|spec| spec.id == id)
            .expect("the complete intrinsic function schema contains every allocated ID")
    }

    pub(super) fn properties(&self) -> &[IntrinsicPropertySpec] {
        &self.properties
    }

    pub(super) fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(super) fn function_count(&self) -> usize {
        self.specs.len()
    }

    pub(super) fn validate(&self) -> Result<(), SchemaValidationError> {
        let cardinalities = [
            FamilyCardinality {
                family: "Realm intrinsic objects",
                actual: self.objects.len(),
                expected: 23,
            },
            FamilyCardinality {
                family: "Realm native functions",
                actual: self.specs.len(),
                expected: 219,
            },
        ];
        validate_intrinsic_schema(IntrinsicSchema {
            objects: &self.objects,
            functions: &self.specs,
            properties: &self.properties,
            mandatory_objects: &IntrinsicObjectId::ALL,
            mandatory_functions: &self.mandatory_functions,
            constructor_prototypes: &[],
            family_cardinalities: &cardinalities,
        })
        .map(|_| ())
    }
}

pub(super) const fn is_declarative_object(id: IntrinsicObjectId) -> bool {
    matches!(
        id,
        IntrinsicObjectId::Reflect | IntrinsicObjectId::Json | IntrinsicObjectId::Math
    )
}

pub(super) const fn is_declarative_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::Reflect(_)
            | NativeFunctionKind::JsonIsRawJson
            | NativeFunctionKind::JsonParse
            | NativeFunctionKind::JsonRawJson
            | NativeFunctionKind::JsonStringify
            | NativeFunctionKind::Math(_)
    )
}

fn count_specs<T>(
    visit: fn(&mut dyn FnMut(T)),
    resource: RuntimeResource,
) -> Result<usize, RuntimeError> {
    let mut count = Some(0_usize);
    visit(&mut |_| {
        count = count.and_then(|value| value.checked_add(1));
    });
    count.ok_or_else(|| allocation_failed(resource, usize::MAX))
}

fn visit_object_specs(visit: ObjectSink<'_>) {
    kernel::visit_objects(visit);
    error::visit_objects(visit);
    primitives::visit_objects(visit);
    array::visit_objects(visit);
    iterator::visit_objects(visit);
    symbol::visit_objects(visit);
    reflect::visit_objects(visit);
    json::visit_objects(visit);
    math::visit_objects(visit);
}

fn visit_function_specs(visit: FunctionSink<'_>) {
    kernel::visit_functions(visit);
    error::visit_functions(visit);
    primitives::visit_functions(visit);
    string::visit_functions(visit);
    array::visit_kernel_functions(visit);
    iterator::visit_functions(visit);
    symbol::visit_functions(visit);
    reflect::visit_functions(visit);
    json::visit_functions(visit);
    math::visit_functions(visit);
    globals::visit_functions(visit);
    array::visit_method_functions(visit);
}

fn visit_property_specs(visit: PropertySink<'_>) {
    reflect::visit_properties(visit);
    json::visit_properties(visit);
    math::visit_properties(visit);
}

const fn object(
    id: IntrinsicObjectId,
    prototype: PrototypeSpec,
    kind: IntrinsicObjectKind,
) -> IntrinsicObjectSpec {
    IntrinsicObjectSpec {
        id,
        prototype,
        kind,
    }
}

const fn object_prototype() -> PrototypeSpec {
    PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
        IntrinsicObjectId::ObjectPrototype,
    ))
}

const fn function_prototype() -> IntrinsicFunctionId {
    IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype)
}

const fn ordinary(
    implementation: NativeFunctionKind,
    name: IntrinsicNameSpec,
    length: i32,
) -> IntrinsicFunctionSpec {
    function(
        implementation,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype())),
        name,
        length,
    )
}

const fn function(
    implementation: NativeFunctionKind,
    prototype: PrototypeSpec,
    name: IntrinsicNameSpec,
    length: i32,
) -> IntrinsicFunctionSpec {
    IntrinsicFunctionSpec {
        id: IntrinsicFunctionId(implementation),
        implementation,
        prototype,
        name,
        length,
        constructable: implementation.is_constructor(),
    }
}

const fn data(
    holder: IntrinsicIdentity,
    key: IntrinsicKeySpec,
    layout: super::PropertyLayout,
    value: IntrinsicValueSpec,
) -> IntrinsicPropertySpec {
    IntrinsicPropertySpec {
        holder,
        key,
        descriptor: IntrinsicDescriptorSpec::Data { layout, value },
    }
}

const fn method(
    holder: IntrinsicIdentity,
    key: IntrinsicKeySpec,
    function: NativeFunctionKind,
) -> IntrinsicPropertySpec {
    data(
        holder,
        key,
        super::METHOD_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(function)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_function_schema_has_characterized_cardinality_and_unique_ids() {
        let schema = RealmFunctionSchema::try_new().expect("function schema");
        assert_eq!(schema.specs().len(), 219);
        for (index, spec) in schema.specs().iter().enumerate() {
            assert!(
                schema.specs()[..index]
                    .iter()
                    .all(|candidate| candidate.id != spec.id)
            );
            assert_eq!(spec.constructable, spec.implementation.is_constructor());
        }
    }

    #[test]
    fn complete_object_schema_has_every_stable_identity_once() {
        let schema = RealmFunctionSchema::try_new().expect("function schema");
        assert_eq!(schema.objects.len(), IntrinsicObjectId::ALL.len());
        for id in IntrinsicObjectId::ALL {
            assert_eq!(
                schema.objects.iter().filter(|spec| spec.id == id).count(),
                1
            );
        }
    }
}
