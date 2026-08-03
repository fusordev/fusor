//! Allocation-free validation for typed Realm intrinsic declarations.

#![allow(
    dead_code,
    reason = "validation becomes the production entry point as intrinsic families migrate"
)]

use crate::{PredefinedAtom, PropertyLayoutKind, predefined_atoms::PredefinedAtomKind};

use super::schema::{
    ConstructorPrototypeSpec, IntrinsicDescriptorSpec, IntrinsicFunctionId, IntrinsicIdentity,
    IntrinsicIdentityPublication, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId,
    IntrinsicPropertySpec, IntrinsicSchema, IntrinsicStringSpec, IntrinsicValueSpec, PrototypeSpec,
};

/// A structural defect in the immutable intrinsic declaration graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum SchemaValidationError {
    DuplicateObject(IntrinsicObjectId),
    DuplicateFunction(IntrinsicFunctionId),
    DanglingPrototype {
        owner: IntrinsicIdentity,
        target: IntrinsicIdentity,
    },
    DanglingPropertyHolder(IntrinsicIdentity),
    DanglingPropertyReference {
        holder: IntrinsicIdentity,
        target: IntrinsicIdentity,
    },
    NonSymbolWellKnownKey(IntrinsicKeySpec),
    DuplicatePropertyKey {
        holder: IntrinsicIdentity,
        key: IntrinsicKeySpec,
    },
    DescriptorKindMismatch {
        holder: IntrinsicIdentity,
        key: IntrinsicKeySpec,
    },
    MissingMandatoryObject(IntrinsicObjectId),
    MissingMandatoryFunction(IntrinsicFunctionId),
    FunctionIdentityMismatch(IntrinsicFunctionId),
    ConstructabilityMismatch(IntrinsicFunctionId),
    DeclaredFunctionIdentityMismatch {
        function: IntrinsicFunctionId,
        key: PredefinedAtom,
    },
    ConstructorPrototypeMismatch(ConstructorPrototypeSpec),
    FamilyCardinality {
        family: &'static str,
        actual: usize,
        expected: usize,
    },
}

/// Proof that every reference and fixed-family assumption in a schema holds.
#[derive(Clone, Copy)]
pub(in crate::runtime) struct ValidatedIntrinsicSchema<'a>(IntrinsicSchema<'a>);

impl<'a> ValidatedIntrinsicSchema<'a> {
    pub(in crate::runtime) const fn schema(self) -> IntrinsicSchema<'a> {
        self.0
    }
}

/// Validates the complete declaration graph without allocating any Runtime
/// arena node or mutating the atom table.
pub(in crate::runtime) fn validate_intrinsic_schema(
    schema: IntrinsicSchema<'_>,
) -> Result<ValidatedIntrinsicSchema<'_>, SchemaValidationError> {
    validate_unique_identities(schema)?;
    validate_mandatory_identities(schema)?;
    validate_functions(schema)?;
    validate_prototypes(schema)?;
    validate_properties(schema)?;
    validate_constructor_prototypes(schema)?;
    validate_family_cardinalities(schema)?;
    Ok(ValidatedIntrinsicSchema(schema))
}

fn validate_unique_identities(schema: IntrinsicSchema<'_>) -> Result<(), SchemaValidationError> {
    for (index, object) in schema.objects.iter().enumerate() {
        if schema.objects[..index]
            .iter()
            .any(|candidate| candidate.id == object.id)
        {
            return Err(SchemaValidationError::DuplicateObject(object.id));
        }
    }
    for (index, function) in schema.functions.iter().enumerate() {
        if schema.functions[..index]
            .iter()
            .any(|candidate| candidate.id == function.id)
        {
            return Err(SchemaValidationError::DuplicateFunction(function.id));
        }
    }
    Ok(())
}

fn validate_mandatory_identities(schema: IntrinsicSchema<'_>) -> Result<(), SchemaValidationError> {
    for &id in schema.mandatory_objects {
        if !has_object(schema, id) {
            return Err(SchemaValidationError::MissingMandatoryObject(id));
        }
    }
    for &id in schema.mandatory_functions {
        if !has_function(schema, id) {
            return Err(SchemaValidationError::MissingMandatoryFunction(id));
        }
    }
    Ok(())
}

fn validate_functions(schema: IntrinsicSchema<'_>) -> Result<(), SchemaValidationError> {
    for function in schema.functions {
        if function.id.0 != function.implementation {
            return Err(SchemaValidationError::FunctionIdentityMismatch(function.id));
        }
        if function.constructable != function.implementation.is_constructor() {
            return Err(SchemaValidationError::ConstructabilityMismatch(function.id));
        }
        if function.identity_publication == IntrinsicIdentityPublication::Declared {
            validate_declared_function_identity(schema, function)?;
        }
    }
    Ok(())
}

fn validate_declared_function_identity(
    schema: IntrinsicSchema<'_>,
    function: &super::schema::IntrinsicFunctionSpec,
) -> Result<(), SchemaValidationError> {
    let holder = IntrinsicIdentity::Function(function.id);
    let length_matches = schema.properties.iter().any(|property| {
        property.holder == holder
            && property.key == IntrinsicKeySpec::PredefinedString(PredefinedAtom::Length)
            && matches!(
                property.descriptor,
                IntrinsicDescriptorSpec::Data { value: IntrinsicValueSpec::NumberBits(bits), .. }
                    if bits == f64::from(function.length).to_bits()
            )
    });
    if !length_matches {
        return Err(SchemaValidationError::DeclaredFunctionIdentityMismatch {
            function: function.id,
            key: PredefinedAtom::Length,
        });
    }
    let expected_name = match function.name {
        IntrinsicNameSpec::Predefined(atom) => IntrinsicStringSpec::Predefined(atom),
        IntrinsicNameSpec::RealmName(id) => IntrinsicStringSpec::RealmName(id),
        IntrinsicNameSpec::Literal(name) => IntrinsicStringSpec::Literal(name),
    };
    let name_matches = schema.properties.iter().any(|property| {
        property.holder == holder
            && property.key == IntrinsicKeySpec::PredefinedString(PredefinedAtom::Name)
            && matches!(
                property.descriptor,
                IntrinsicDescriptorSpec::Data { value: IntrinsicValueSpec::String(value), .. }
                    if value == expected_name
            )
    });
    if !name_matches {
        return Err(SchemaValidationError::DeclaredFunctionIdentityMismatch {
            function: function.id,
            key: PredefinedAtom::Name,
        });
    }
    Ok(())
}

fn validate_prototypes(schema: IntrinsicSchema<'_>) -> Result<(), SchemaValidationError> {
    for object in schema.objects {
        validate_prototype(
            schema,
            IntrinsicIdentity::Object(object.id),
            object.prototype,
        )?;
    }
    for function in schema.functions {
        validate_prototype(
            schema,
            IntrinsicIdentity::Function(function.id),
            function.prototype,
        )?;
    }
    Ok(())
}

fn validate_prototype(
    schema: IntrinsicSchema<'_>,
    owner: IntrinsicIdentity,
    prototype: PrototypeSpec,
) -> Result<(), SchemaValidationError> {
    let PrototypeSpec::Intrinsic(target) = prototype else {
        return Ok(());
    };
    if !has_identity(schema, target) {
        return Err(SchemaValidationError::DanglingPrototype { owner, target });
    }
    Ok(())
}

fn validate_properties(schema: IntrinsicSchema<'_>) -> Result<(), SchemaValidationError> {
    for (index, property) in schema.properties.iter().enumerate() {
        if !has_identity(schema, property.holder) {
            return Err(SchemaValidationError::DanglingPropertyHolder(
                property.holder,
            ));
        }
        if let IntrinsicKeySpec::WellKnownSymbol(atom) = property.key
            && atom.spec().kind != PredefinedAtomKind::Symbol
        {
            return Err(SchemaValidationError::NonSymbolWellKnownKey(property.key));
        }
        if schema.properties[..index]
            .iter()
            .any(|candidate| candidate.holder == property.holder && candidate.key == property.key)
        {
            return Err(SchemaValidationError::DuplicatePropertyKey {
                holder: property.holder,
                key: property.key,
            });
        }
        validate_descriptor(schema, property)?;
    }
    Ok(())
}

fn validate_descriptor(
    schema: IntrinsicSchema<'_>,
    property: &IntrinsicPropertySpec,
) -> Result<(), SchemaValidationError> {
    match property.descriptor {
        IntrinsicDescriptorSpec::Data { layout, value } => {
            if layout.kind() != PropertyLayoutKind::Data {
                return Err(SchemaValidationError::DescriptorKindMismatch {
                    holder: property.holder,
                    key: property.key,
                });
            }
            let target = match value {
                IntrinsicValueSpec::Object(id) => Some(IntrinsicIdentity::Object(id)),
                IntrinsicValueSpec::Function(id) => Some(IntrinsicIdentity::Function(id)),
                _ => None,
            };
            if let Some(target) = target
                && !has_identity(schema, target)
            {
                return Err(SchemaValidationError::DanglingPropertyReference {
                    holder: property.holder,
                    target,
                });
            }
        }
        IntrinsicDescriptorSpec::Accessor {
            layout,
            getter,
            setter,
        } => {
            if layout.kind() != PropertyLayoutKind::Accessor {
                return Err(SchemaValidationError::DescriptorKindMismatch {
                    holder: property.holder,
                    key: property.key,
                });
            }
            for id in [getter, setter].into_iter().flatten() {
                let target = IntrinsicIdentity::Function(id);
                if !has_identity(schema, target) {
                    return Err(SchemaValidationError::DanglingPropertyReference {
                        holder: property.holder,
                        target,
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_constructor_prototypes(
    schema: IntrinsicSchema<'_>,
) -> Result<(), SchemaValidationError> {
    for &pair in schema.constructor_prototypes {
        let constructor_property = schema.properties.iter().any(|property| {
            property.holder == IntrinsicIdentity::Function(pair.constructor)
                && matches!(
                    property.descriptor,
                    IntrinsicDescriptorSpec::Data {
                        value: IntrinsicValueSpec::Object(id),
                        ..
                    } if id == pair.prototype
                )
        });
        let prototype_property = schema.properties.iter().any(|property| {
            property.holder == IntrinsicIdentity::Object(pair.prototype)
                && matches!(
                    property.descriptor,
                    IntrinsicDescriptorSpec::Data {
                        value: IntrinsicValueSpec::Function(id),
                        ..
                    } if id == pair.constructor
                )
        });
        if !constructor_property || !prototype_property {
            return Err(SchemaValidationError::ConstructorPrototypeMismatch(pair));
        }
    }
    Ok(())
}

fn validate_family_cardinalities(schema: IntrinsicSchema<'_>) -> Result<(), SchemaValidationError> {
    for family in schema.family_cardinalities {
        if family.actual != family.expected {
            return Err(SchemaValidationError::FamilyCardinality {
                family: family.family,
                actual: family.actual,
                expected: family.expected,
            });
        }
    }
    Ok(())
}

fn has_identity(schema: IntrinsicSchema<'_>, id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(id) => has_object(schema, id),
        IntrinsicIdentity::Function(id) => has_function(schema, id),
    }
}

fn has_object(schema: IntrinsicSchema<'_>, id: IntrinsicObjectId) -> bool {
    schema.objects.iter().any(|object| object.id == id)
}

fn has_function(schema: IntrinsicSchema<'_>, id: IntrinsicFunctionId) -> bool {
    schema.functions.iter().any(|function| function.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::realm::schema::{
        FamilyCardinality, IntrinsicFunctionSpec, IntrinsicIdentityPublication, IntrinsicNameSpec,
        IntrinsicObjectKind, IntrinsicObjectSpec, IntrinsicPropertySpec, IntrinsicStringSpec,
    };
    use crate::runtime::{NativeFunctionKind, PredefinedAtom, PropertyLayout};

    const OBJECT: IntrinsicObjectId = IntrinsicObjectId::ObjectPrototype;
    const FUNCTION: IntrinsicFunctionId =
        IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype);
    const OBJECT_SPEC: IntrinsicObjectSpec = IntrinsicObjectSpec {
        id: OBJECT,
        prototype: PrototypeSpec::Null,
        kind: IntrinsicObjectKind::Ordinary,
    };
    const FUNCTION_SPEC: IntrinsicFunctionSpec = IntrinsicFunctionSpec {
        id: FUNCTION,
        implementation: NativeFunctionKind::FunctionPrototype,
        prototype: PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(OBJECT)),
        name: IntrinsicNameSpec::Literal(""),
        length: 0,
        constructable: false,
        identity_publication: IntrinsicIdentityPublication::Automatic,
    };
    const KEY: IntrinsicKeySpec = IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor);
    const PROPERTY: IntrinsicPropertySpec = IntrinsicPropertySpec {
        holder: IntrinsicIdentity::Object(OBJECT),
        key: KEY,
        descriptor: IntrinsicDescriptorSpec::Data {
            layout: PropertyLayout::data(true, false, true),
            value: IntrinsicValueSpec::Function(FUNCTION),
        },
    };

    fn schema<'a>(
        objects: &'a [IntrinsicObjectSpec],
        functions: &'a [IntrinsicFunctionSpec],
        properties: &'a [IntrinsicPropertySpec],
    ) -> IntrinsicSchema<'a> {
        IntrinsicSchema {
            objects,
            functions,
            properties,
            mandatory_objects: &[],
            mandatory_functions: &[],
            constructor_prototypes: &[],
            family_cardinalities: &[],
        }
    }

    #[test]
    fn accepts_cycles_declared_independently_from_allocation_order() {
        let objects = [IntrinsicObjectSpec {
            prototype: PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(FUNCTION)),
            ..OBJECT_SPEC
        }];
        let functions = [FUNCTION_SPEC];
        assert!(validate_intrinsic_schema(schema(&objects, &functions, &[])).is_ok());
    }

    #[test]
    fn rejects_duplicate_object_and_function_ids() {
        let objects = [OBJECT_SPEC, OBJECT_SPEC];
        assert_eq!(
            validate_intrinsic_schema(schema(&objects, &[FUNCTION_SPEC], &[])).err(),
            Some(SchemaValidationError::DuplicateObject(OBJECT))
        );
        let functions = [FUNCTION_SPEC, FUNCTION_SPEC];
        assert_eq!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &functions, &[])).err(),
            Some(SchemaValidationError::DuplicateFunction(FUNCTION))
        );
    }

    #[test]
    fn rejects_dangling_prototypes_holders_values_and_accessors() {
        let missing = IntrinsicFunctionId(NativeFunctionKind::ThrowTypeError);
        let object = IntrinsicObjectSpec {
            prototype: PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(missing)),
            ..OBJECT_SPEC
        };
        assert!(matches!(
            validate_intrinsic_schema(schema(&[object], &[FUNCTION_SPEC], &[])),
            Err(SchemaValidationError::DanglingPrototype { .. })
        ));

        let property = IntrinsicPropertySpec {
            holder: IntrinsicIdentity::Function(missing),
            ..PROPERTY
        };
        assert_eq!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[property])).err(),
            Some(SchemaValidationError::DanglingPropertyHolder(
                IntrinsicIdentity::Function(missing)
            ))
        );

        let property = IntrinsicPropertySpec {
            descriptor: IntrinsicDescriptorSpec::Data {
                layout: PropertyLayout::data(true, false, true),
                value: IntrinsicValueSpec::Function(missing),
            },
            ..PROPERTY
        };
        assert!(matches!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[property])),
            Err(SchemaValidationError::DanglingPropertyReference { .. })
        ));

        let property = IntrinsicPropertySpec {
            descriptor: IntrinsicDescriptorSpec::Accessor {
                layout: PropertyLayout::accessor(false, true),
                getter: Some(missing),
                setter: None,
            },
            ..PROPERTY
        };
        assert!(matches!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[property])),
            Err(SchemaValidationError::DanglingPropertyReference { .. })
        ));
    }

    #[test]
    fn rejects_non_symbol_well_known_keys_and_duplicate_holder_keys() {
        let invalid = IntrinsicPropertySpec {
            key: IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::Name),
            ..PROPERTY
        };
        assert!(matches!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[invalid])),
            Err(SchemaValidationError::NonSymbolWellKnownKey(_))
        ));
        assert!(matches!(
            validate_intrinsic_schema(schema(
                &[OBJECT_SPEC],
                &[FUNCTION_SPEC],
                &[PROPERTY, PROPERTY]
            )),
            Err(SchemaValidationError::DuplicatePropertyKey { .. })
        ));
    }

    #[test]
    fn rejects_descriptor_layout_mismatches() {
        let property = IntrinsicPropertySpec {
            descriptor: IntrinsicDescriptorSpec::Data {
                layout: PropertyLayout::accessor(false, true),
                value: IntrinsicValueSpec::Undefined,
            },
            ..PROPERTY
        };
        assert!(matches!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[property])),
            Err(SchemaValidationError::DescriptorKindMismatch { .. })
        ));
    }

    #[test]
    fn rejects_missing_mandatory_identities() {
        let missing_object = IntrinsicObjectId::GlobalObject;
        let value = IntrinsicSchema {
            mandatory_objects: &[missing_object],
            ..schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[])
        };
        assert_eq!(
            validate_intrinsic_schema(value).err(),
            Some(SchemaValidationError::MissingMandatoryObject(
                missing_object
            ))
        );
        let missing_function = IntrinsicFunctionId(NativeFunctionKind::ThrowTypeError);
        let value = IntrinsicSchema {
            mandatory_functions: &[missing_function],
            ..schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[])
        };
        assert_eq!(
            validate_intrinsic_schema(value).err(),
            Some(SchemaValidationError::MissingMandatoryFunction(
                missing_function
            ))
        );
    }

    #[test]
    fn rejects_function_identity_and_constructability_mismatches() {
        let function = IntrinsicFunctionSpec {
            implementation: NativeFunctionKind::ThrowTypeError,
            ..FUNCTION_SPEC
        };
        assert_eq!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[function], &[])).err(),
            Some(SchemaValidationError::FunctionIdentityMismatch(FUNCTION))
        );
        let function = IntrinsicFunctionSpec {
            constructable: true,
            ..FUNCTION_SPEC
        };
        assert_eq!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[function], &[])).err(),
            Some(SchemaValidationError::ConstructabilityMismatch(FUNCTION))
        );
    }

    #[test]
    fn rejects_missing_or_mismatched_declared_function_identity_properties() {
        let function = IntrinsicFunctionSpec {
            identity_publication: IntrinsicIdentityPublication::Declared,
            ..FUNCTION_SPEC
        };
        assert_eq!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[function], &[])).err(),
            Some(SchemaValidationError::DeclaredFunctionIdentityMismatch {
                function: FUNCTION,
                key: PredefinedAtom::Length,
            })
        );
        let length = IntrinsicPropertySpec {
            holder: IntrinsicIdentity::Function(FUNCTION),
            key: IntrinsicKeySpec::PredefinedString(PredefinedAtom::Length),
            descriptor: IntrinsicDescriptorSpec::Data {
                layout: PropertyLayout::data(false, false, true),
                value: IntrinsicValueSpec::NumberBits(0_f64.to_bits()),
            },
        };
        assert_eq!(
            validate_intrinsic_schema(schema(&[OBJECT_SPEC], &[function], &[length])).err(),
            Some(SchemaValidationError::DeclaredFunctionIdentityMismatch {
                function: FUNCTION,
                key: PredefinedAtom::Name,
            })
        );
    }

    #[test]
    fn rejects_incomplete_constructor_prototype_pairs() {
        let pair = ConstructorPrototypeSpec {
            constructor: FUNCTION,
            prototype: OBJECT,
        };
        let value = IntrinsicSchema {
            constructor_prototypes: &[pair],
            ..schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[PROPERTY])
        };
        assert_eq!(
            validate_intrinsic_schema(value).err(),
            Some(SchemaValidationError::ConstructorPrototypeMismatch(pair))
        );
    }

    #[test]
    fn rejects_family_cardinality_before_fixed_array_materialization() {
        let value = IntrinsicSchema {
            family_cardinalities: &[FamilyCardinality {
                family: "MathMethod",
                actual: 1,
                expected: 2,
            }],
            ..schema(&[OBJECT_SPEC], &[FUNCTION_SPEC], &[])
        };
        assert_eq!(
            validate_intrinsic_schema(value).err(),
            Some(SchemaValidationError::FamilyCardinality {
                family: "MathMethod",
                actual: 1,
                expected: 2,
            })
        );
    }

    #[test]
    fn schema_string_values_are_typed_without_runtime_atoms() {
        let value = IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("RealmName"));
        assert_eq!(
            value,
            IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("RealmName"))
        );
    }
}
