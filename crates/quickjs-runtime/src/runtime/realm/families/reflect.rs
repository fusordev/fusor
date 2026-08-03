//! `%Reflect%` object and method declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    IDENTITY_PROPERTY, METHOD_PROPERTY, NativeFunctionKind, PredefinedAtom, ReflectMethod,
    schema::{
        IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId,
        IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Reflect,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for method in ReflectMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Reflect(method),
            IntrinsicNameSpec::Predefined(method.predefined_atom()),
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let reflect = IntrinsicIdentity::Object(IntrinsicObjectId::Reflect);
    for method_id in ReflectMethod::ALL {
        visit(method(
            reflect,
            IntrinsicKeySpec::PredefinedString(method_id.predefined_atom()),
            NativeFunctionKind::Reflect(method_id),
        ));
    }
    visit(data(
        reflect,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::RealmName(RealmNameId::Reflect)),
    ));
    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::InternedString(RealmNameId::Reflect),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Reflect),
    ));
}
