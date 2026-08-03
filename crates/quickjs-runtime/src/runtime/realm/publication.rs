//! Generic descriptor publication from validated intrinsic specifications.

use std::collections::TryReserveError;

use crate::ArrayIndex;

use super::{
    AtomError, JsNumber, JsString, ObjectRecord, PredefinedAtom, PropertyKey,
    RealmBuildTransaction, RuntimeError, StoredValue,
    families::{DeclarativeBatch, RealmFunctionSchema, function_batch, property_batch},
    property_allocation_failed,
    schema::{
        IntrinsicDescriptorSpec, IntrinsicFunctionId, IntrinsicFunctionSpec, IntrinsicIdentity,
        IntrinsicIdentityPublication, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicStringSpec,
        IntrinsicValueSpec, RealmNameId,
    },
};

/// Failure while resolving or appending a schema descriptor.
pub(super) enum RealmPublicationError {
    PropertyAllocation,
    Runtime(RuntimeError),
}

impl RealmPublicationError {
    pub(super) fn into_runtime_error(self) -> RuntimeError {
        match self {
            Self::PropertyAllocation => property_allocation_failed(1),
            Self::Runtime(error) => error,
        }
    }
}

impl From<TryReserveError> for RealmPublicationError {
    fn from(_: TryReserveError) -> Self {
        Self::PropertyAllocation
    }
}

enum ResolvedDescriptor {
    Data {
        layout: super::PropertyLayout,
        value: StoredValue,
    },
    Accessor {
        layout: super::PropertyLayout,
        getter: Option<super::FunctionId>,
        setter: Option<super::FunctionId>,
    },
}

impl RealmBuildTransaction<'_> {
    /// Publishes the function identities and descriptors owned by the
    /// currently migrated declarative families.
    pub(super) fn publish_intrinsic_schema_batch(
        &mut self,
        schema: &RealmFunctionSchema,
        atoms: &super::RealmAtomBindings,
        batch: DeclarativeBatch,
    ) -> Result<(), RealmPublicationError> {
        for function in schema.specs().iter().filter(|function| {
            function_batch(function.id) == batch
                && function.identity_publication == IntrinsicIdentityPublication::Automatic
        }) {
            self.publish_intrinsic_function_identity(function, atoms)?;
        }
        for property in schema
            .properties()
            .iter()
            .filter(|property| property_batch(**property) == batch)
        {
            self.publish_intrinsic_property(property, atoms)?;
        }
        Ok(())
    }

    /// Applies the sole post-publication kernel invariant that is not an
    /// ordinary property descriptor: `%ThrowTypeError%` is non-extensible.
    pub(super) fn finalize_realm_kernel(&mut self) {
        let function = self.allocated.function(IntrinsicFunctionId(
            super::NativeFunctionKind::ThrowTypeError,
        ));
        self.functions
            .get_mut(function)
            .expect("the allocated %ThrowTypeError% remains live")
            .object
            .prevent_extensions();
    }

    /// Resolves and appends one complete descriptor in declaration order.
    pub(super) fn publish_intrinsic_property(
        &mut self,
        property: &super::schema::IntrinsicPropertySpec,
        atoms: &super::RealmAtomBindings,
    ) -> Result<(), RealmPublicationError> {
        let key = self.resolve_intrinsic_key(property.key, atoms);
        let descriptor = self.resolve_intrinsic_descriptor(property.descriptor, atoms)?;
        match property.holder {
            IntrinsicIdentity::Object(id) => {
                let object = self.allocated.object(id);
                let record = &mut self
                    .objects
                    .get_mut(object)
                    .expect("allocated intrinsic object remains live")
                    .record;
                append_descriptor(record, key, descriptor)?;
            }
            IntrinsicIdentity::Function(id) => {
                let function = self.allocated.function(id);
                let record = &mut self
                    .functions
                    .get_mut(function)
                    .expect("allocated intrinsic function remains live")
                    .object;
                append_descriptor(record, key, descriptor)?;
            }
        }
        Ok(())
    }

    /// Publishes the ordinary non-writable `length` and `name` properties
    /// derived from one function specification.
    pub(super) fn publish_intrinsic_function_identity(
        &mut self,
        function: &IntrinsicFunctionSpec,
        atoms: &super::RealmAtomBindings,
    ) -> Result<(), RealmPublicationError> {
        let function_id = self.allocated.function(function.id);
        let name = self.resolve_intrinsic_function_name(function.name, atoms)?;
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let name_key = self.predefined_property_key(PredefinedAtom::Name);
        let record = &mut self
            .functions
            .get_mut(function_id)
            .expect("allocated intrinsic function remains live")
            .object;
        record.append_data(
            length_key,
            super::IDENTITY_PROPERTY,
            StoredValue::Number(JsNumber::from_i32(function.length)),
        )?;
        record.append_data(
            name_key,
            super::IDENTITY_PROPERTY,
            StoredValue::String(name),
        )?;
        Ok(())
    }

    fn resolve_intrinsic_key(
        &self,
        key: IntrinsicKeySpec,
        atoms: &super::RealmAtomBindings,
    ) -> PropertyKey {
        match key {
            IntrinsicKeySpec::PredefinedString(atom) => self.predefined_property_key(atom),
            IntrinsicKeySpec::InternedString(id) | IntrinsicKeySpec::RealmCreatedName(id) => {
                PropertyKey::from_validated_atom(atoms.atom(id).clone())
            }
            IntrinsicKeySpec::WellKnownSymbol(atom) => {
                PropertyKey::from_validated_symbol(self.atoms.predefined(atom))
            }
            IntrinsicKeySpec::ArrayIndex(index) => PropertyKey::from_index(
                ArrayIndex::new(index).expect("schema array indices exclude the u32 sentinel"),
            ),
        }
    }

    fn resolve_intrinsic_descriptor(
        &self,
        descriptor: IntrinsicDescriptorSpec,
        atoms: &super::RealmAtomBindings,
    ) -> Result<ResolvedDescriptor, RealmPublicationError> {
        Ok(match descriptor {
            IntrinsicDescriptorSpec::Data { layout, value } => ResolvedDescriptor::Data {
                layout,
                value: self.resolve_intrinsic_value(value, atoms)?,
            },
            IntrinsicDescriptorSpec::Accessor {
                layout,
                getter,
                setter,
            } => ResolvedDescriptor::Accessor {
                layout,
                getter: getter.map(|id| self.allocated.function(id)),
                setter: setter.map(|id| self.allocated.function(id)),
            },
        })
    }

    fn resolve_intrinsic_value(
        &self,
        value: IntrinsicValueSpec,
        atoms: &super::RealmAtomBindings,
    ) -> Result<StoredValue, RealmPublicationError> {
        Ok(match value {
            IntrinsicValueSpec::Undefined => StoredValue::Undefined,
            IntrinsicValueSpec::Null => StoredValue::Null,
            IntrinsicValueSpec::Boolean(value) => StoredValue::Boolean(value),
            IntrinsicValueSpec::NumberBits(bits) => {
                StoredValue::Number(JsNumber::from_f64(f64::from_bits(bits)))
            }
            IntrinsicValueSpec::String(value) => {
                StoredValue::String(self.resolve_intrinsic_string(value, atoms)?)
            }
            IntrinsicValueSpec::Object(id) => StoredValue::Object(self.allocated.object(id)),
            IntrinsicValueSpec::Function(id) => StoredValue::Function(self.allocated.function(id)),
            IntrinsicValueSpec::WellKnownSymbol(atom) => {
                StoredValue::Symbol(self.atoms.predefined(atom))
            }
        })
    }

    fn resolve_intrinsic_function_name(
        &self,
        name: IntrinsicNameSpec,
        atoms: &super::RealmAtomBindings,
    ) -> Result<JsString, RealmPublicationError> {
        match name {
            IntrinsicNameSpec::Predefined(atom) => Ok(super::predefined_string(&self.atoms, atom)),
            IntrinsicNameSpec::RealmName(id) => Ok(realm_name(atoms, id)),
            IntrinsicNameSpec::Literal(name) => JsString::from_utf8(name)
                .map_err(AtomError::from)
                .map_err(RuntimeError::from)
                .map_err(RealmPublicationError::Runtime),
        }
    }

    fn resolve_intrinsic_string(
        &self,
        value: IntrinsicStringSpec,
        atoms: &super::RealmAtomBindings,
    ) -> Result<JsString, RealmPublicationError> {
        match value {
            IntrinsicStringSpec::Predefined(atom) => {
                Ok(super::predefined_string(&self.atoms, atom))
            }
            IntrinsicStringSpec::RealmName(id) => Ok(realm_name(atoms, id)),
            IntrinsicStringSpec::Literal(value) => JsString::from_utf8(value)
                .map_err(AtomError::from)
                .map_err(RuntimeError::from)
                .map_err(RealmPublicationError::Runtime),
        }
    }
}

fn realm_name(atoms: &super::RealmAtomBindings, id: RealmNameId) -> JsString {
    atoms
        .atom(id)
        .description()
        .expect("Realm string atoms have descriptions")
        .clone()
}

fn append_descriptor(
    record: &mut ObjectRecord,
    key: PropertyKey,
    descriptor: ResolvedDescriptor,
) -> Result<(), TryReserveError> {
    match descriptor {
        ResolvedDescriptor::Data { layout, value } => record.append_data(key, layout, value),
        ResolvedDescriptor::Accessor {
            layout,
            getter,
            setter,
        } => record.append_accessor(key, layout, getter, setter),
    }
}

// Keep the type relationship explicit for the generic resolver.
const _: Option<IntrinsicFunctionId> = None;
