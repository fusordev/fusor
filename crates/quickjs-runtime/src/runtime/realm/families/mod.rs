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
    IntrinsicIdentity, IntrinsicIdentityPublication, IntrinsicKeySpec, IntrinsicNameSpec,
    IntrinsicObjectId, IntrinsicObjectKind, IntrinsicObjectSpec, IntrinsicPropertySpec,
    IntrinsicSchema, IntrinsicValueSpec, PrototypeSpec,
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
        IntrinsicObjectId::ErrorPrototype(_)
            | IntrinsicObjectId::BooleanPrototype
            | IntrinsicObjectId::NumberPrototype
            | IntrinsicObjectId::BigIntPrototype
            | IntrinsicObjectId::StringPrototype
            | IntrinsicObjectId::SymbolPrototype
            | IntrinsicObjectId::Reflect
            | IntrinsicObjectId::Json
            | IntrinsicObjectId::Math
    )
}

pub(super) const fn is_declarative_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::ErrorConstructor(_)
            | NativeFunctionKind::ErrorPrototypeToString
            | NativeFunctionKind::ErrorIsError
            | NativeFunctionKind::BooleanConstructor
            | NativeFunctionKind::BooleanPrototypeToString
            | NativeFunctionKind::BooleanPrototypeValueOf
            | NativeFunctionKind::NumberConstructor
            | NativeFunctionKind::NumberPrototypeToString
            | NativeFunctionKind::NumberPrototypeValueOf
            | NativeFunctionKind::NumberPredicateStatic(_)
            | NativeFunctionKind::NumberPrototypeFormat(_)
            | NativeFunctionKind::BigIntConstructor
            | NativeFunctionKind::BigIntPrototypeToString
            | NativeFunctionKind::BigIntPrototypeValueOf
            | NativeFunctionKind::BigIntAsIntN
            | NativeFunctionKind::BigIntAsUintN
            | NativeFunctionKind::StringConstructor
            | NativeFunctionKind::StringPrototypeToString
            | NativeFunctionKind::StringPrototypeValueOf
            | NativeFunctionKind::StringPrototypeMethod(_)
            | NativeFunctionKind::StringRaw
            | NativeFunctionKind::LocaleString(_)
            | NativeFunctionKind::SymbolConstructor
            | NativeFunctionKind::SymbolPrototypeToString
            | NativeFunctionKind::SymbolPrototypeValueOf
            | NativeFunctionKind::SymbolPrototypeToPrimitive
            | NativeFunctionKind::SymbolPrototypeDescription
            | NativeFunctionKind::SymbolFor
            | NativeFunctionKind::SymbolKeyFor
            | NativeFunctionKind::GlobalNumeric(_)
            | NativeFunctionKind::GlobalUri(_)
            | NativeFunctionKind::Reflect(_)
            | NativeFunctionKind::JsonIsRawJson
            | NativeFunctionKind::JsonParse
            | NativeFunctionKind::JsonRawJson
            | NativeFunctionKind::JsonStringify
            | NativeFunctionKind::Math(_)
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeclarativeBatch {
    Errors,
    ErrorGlobals,
    Globals,
    Primitives,
    PrimitiveGlobals,
    Symbols,
    SymbolGlobals,
    NamespaceObjects,
}

pub(super) const fn property_batch(property: IntrinsicPropertySpec) -> DeclarativeBatch {
    let referenced_function = match property.descriptor {
        IntrinsicDescriptorSpec::Data {
            value: IntrinsicValueSpec::Function(id),
            ..
        } => Some(id),
        _ => None,
    };
    if matches!(
        referenced_function,
        Some(IntrinsicFunctionId(
            NativeFunctionKind::GlobalNumeric(_) | NativeFunctionKind::GlobalUri(_)
        ))
    ) {
        return DeclarativeBatch::Globals;
    }
    if matches!(
        (property.holder, referenced_function),
        (
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            Some(IntrinsicFunctionId(NativeFunctionKind::ErrorConstructor(_)))
        )
    ) {
        return DeclarativeBatch::ErrorGlobals;
    }
    if matches!(
        (property.holder, referenced_function),
        (
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            Some(IntrinsicFunctionId(
                NativeFunctionKind::BooleanConstructor
                    | NativeFunctionKind::NumberConstructor
                    | NativeFunctionKind::BigIntConstructor
                    | NativeFunctionKind::StringConstructor
            ))
        )
    ) {
        return DeclarativeBatch::PrimitiveGlobals;
    }
    if matches!(
        (property.holder, referenced_function),
        (
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            Some(IntrinsicFunctionId(NativeFunctionKind::SymbolConstructor))
        )
    ) {
        return DeclarativeBatch::SymbolGlobals;
    }
    if is_error_identity(property.holder)
        || matches!(
            referenced_function,
            Some(IntrinsicFunctionId(
                NativeFunctionKind::ErrorConstructor(_)
                    | NativeFunctionKind::ErrorPrototypeToString
                    | NativeFunctionKind::ErrorIsError
            ))
        )
    {
        return DeclarativeBatch::Errors;
    }
    let references_primitive = match referenced_function {
        Some(id) => is_primitive_function(id),
        None => false,
    };
    if is_primitive_identity(property.holder) || references_primitive {
        return DeclarativeBatch::Primitives;
    }
    if is_symbol_identity(property.holder)
        || matches!(
            referenced_function,
            Some(IntrinsicFunctionId(
                NativeFunctionKind::SymbolConstructor
                    | NativeFunctionKind::SymbolPrototypeToString
                    | NativeFunctionKind::SymbolPrototypeValueOf
                    | NativeFunctionKind::SymbolPrototypeToPrimitive
                    | NativeFunctionKind::SymbolPrototypeDescription
                    | NativeFunctionKind::SymbolFor
                    | NativeFunctionKind::SymbolKeyFor
            ))
        )
    {
        return DeclarativeBatch::Symbols;
    }
    DeclarativeBatch::NamespaceObjects
}

const fn is_error_identity(id: IntrinsicIdentity) -> bool {
    matches!(
        id,
        IntrinsicIdentity::Object(IntrinsicObjectId::ErrorPrototype(_))
            | IntrinsicIdentity::Function(IntrinsicFunctionId(
                NativeFunctionKind::ErrorConstructor(_)
                    | NativeFunctionKind::ErrorPrototypeToString
                    | NativeFunctionKind::ErrorIsError,
            ))
    )
}

const fn is_symbol_identity(id: IntrinsicIdentity) -> bool {
    matches!(
        id,
        IntrinsicIdentity::Object(IntrinsicObjectId::SymbolPrototype)
            | IntrinsicIdentity::Function(IntrinsicFunctionId(
                NativeFunctionKind::SymbolConstructor
                    | NativeFunctionKind::SymbolPrototypeToString
                    | NativeFunctionKind::SymbolPrototypeValueOf
                    | NativeFunctionKind::SymbolPrototypeToPrimitive
                    | NativeFunctionKind::SymbolPrototypeDescription
                    | NativeFunctionKind::SymbolFor
                    | NativeFunctionKind::SymbolKeyFor,
            ))
    )
}

const fn is_primitive_identity(id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(id) => matches!(
            id,
            IntrinsicObjectId::BooleanPrototype
                | IntrinsicObjectId::NumberPrototype
                | IntrinsicObjectId::BigIntPrototype
                | IntrinsicObjectId::StringPrototype
        ),
        IntrinsicIdentity::Function(id) => is_primitive_function(id),
    }
}

const fn is_primitive_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::BooleanConstructor
            | NativeFunctionKind::BooleanPrototypeToString
            | NativeFunctionKind::BooleanPrototypeValueOf
            | NativeFunctionKind::NumberConstructor
            | NativeFunctionKind::NumberPrototypeToString
            | NativeFunctionKind::NumberPrototypeValueOf
            | NativeFunctionKind::NumberPredicateStatic(_)
            | NativeFunctionKind::NumberPrototypeFormat(_)
            | NativeFunctionKind::BigIntConstructor
            | NativeFunctionKind::BigIntPrototypeToString
            | NativeFunctionKind::BigIntPrototypeValueOf
            | NativeFunctionKind::BigIntAsIntN
            | NativeFunctionKind::BigIntAsUintN
            | NativeFunctionKind::StringConstructor
            | NativeFunctionKind::StringPrototypeToString
            | NativeFunctionKind::StringPrototypeValueOf
            | NativeFunctionKind::StringPrototypeMethod(_)
            | NativeFunctionKind::StringRaw
            | NativeFunctionKind::LocaleString(_)
    )
}

pub(super) const fn function_batch(id: IntrinsicFunctionId) -> DeclarativeBatch {
    if matches!(
        id.0,
        NativeFunctionKind::ErrorConstructor(_)
            | NativeFunctionKind::ErrorPrototypeToString
            | NativeFunctionKind::ErrorIsError
    ) {
        DeclarativeBatch::Errors
    } else if matches!(
        id.0,
        NativeFunctionKind::GlobalNumeric(_) | NativeFunctionKind::GlobalUri(_)
    ) {
        DeclarativeBatch::Globals
    } else if is_primitive_function(id) {
        DeclarativeBatch::Primitives
    } else if matches!(
        id.0,
        NativeFunctionKind::SymbolConstructor
            | NativeFunctionKind::SymbolPrototypeToString
            | NativeFunctionKind::SymbolPrototypeValueOf
            | NativeFunctionKind::SymbolPrototypeToPrimitive
            | NativeFunctionKind::SymbolPrototypeDescription
            | NativeFunctionKind::SymbolFor
            | NativeFunctionKind::SymbolKeyFor
    ) {
        DeclarativeBatch::Symbols
    } else {
        DeclarativeBatch::NamespaceObjects
    }
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
    error::visit_properties(visit);
    primitives::visit_properties(visit);
    string::visit_properties(visit);
    symbol::visit_properties(visit);
    globals::visit_properties(visit);
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
        identity_publication: IntrinsicIdentityPublication::Automatic,
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

const fn accessor(
    holder: IntrinsicIdentity,
    key: IntrinsicKeySpec,
    layout: super::PropertyLayout,
    getter: Option<IntrinsicFunctionId>,
    setter: Option<IntrinsicFunctionId>,
) -> IntrinsicPropertySpec {
    IntrinsicPropertySpec {
        holder,
        key,
        descriptor: IntrinsicDescriptorSpec::Accessor {
            layout,
            getter,
            setter,
        },
    }
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
