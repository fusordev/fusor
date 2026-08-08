//! `%Intl%` namespace declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    IDENTITY_PROPERTY, METHOD_PROPERTY, NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId,
        IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Intl,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::IntlGetCanonicalLocales,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlGetCanonicalLocales),
        1,
    ));
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let intl = IntrinsicIdentity::Object(IntrinsicObjectId::Intl);
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlGetCanonicalLocales),
        NativeFunctionKind::IntlGetCanonicalLocales,
    ));
    visit(data(
        intl,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::RealmName(RealmNameId::Intl)),
    ));
    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::InternedString(RealmNameId::Intl),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Intl),
    ));
}
