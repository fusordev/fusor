//! `%Atomics%` namespace declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::{
    AtomicsMethod,
    realm::{
        IDENTITY_PROPERTY, METHOD_PROPERTY, NativeFunctionKind,
        schema::{
            IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId,
            IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec, RealmNameId,
        },
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Atomics,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for method_id in AtomicsMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Atomics(method_id),
            IntrinsicNameSpec::RealmName(RealmNameId::AtomicsMethod(method_id)),
            method_id.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let atomics = IntrinsicIdentity::Object(IntrinsicObjectId::Atomics);
    for method_id in AtomicsMethod::ALL {
        visit(method(
            atomics,
            IntrinsicKeySpec::InternedString(RealmNameId::AtomicsMethod(method_id)),
            NativeFunctionKind::Atomics(method_id),
        ));
    }
    visit(data(
        atomics,
        IntrinsicKeySpec::WellKnownSymbol(crate::PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Atomics")),
    ));
    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::InternedString(RealmNameId::Atomics),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Atomics),
    ));
}
