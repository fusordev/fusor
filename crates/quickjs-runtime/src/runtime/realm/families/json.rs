//! `%JSON%` object and method declarations.

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
        IntrinsicObjectId::Json,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::JsonIsRawJson,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonIsRawJson),
            1,
        ),
        (
            NativeFunctionKind::JsonParse,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonParse),
            2,
        ),
        (
            NativeFunctionKind::JsonRawJson,
            IntrinsicNameSpec::Predefined(PredefinedAtom::RawJson),
            1,
        ),
        (
            NativeFunctionKind::JsonStringify,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonStringify),
            3,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let json = IntrinsicIdentity::Object(IntrinsicObjectId::Json);
    for (key, kind) in [
        (
            IntrinsicKeySpec::InternedString(RealmNameId::JsonIsRawJson),
            NativeFunctionKind::JsonIsRawJson,
        ),
        (
            IntrinsicKeySpec::InternedString(RealmNameId::JsonParse),
            NativeFunctionKind::JsonParse,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::RawJson),
            NativeFunctionKind::JsonRawJson,
        ),
        (
            IntrinsicKeySpec::InternedString(RealmNameId::JsonStringify),
            NativeFunctionKind::JsonStringify,
        ),
    ] {
        visit(method(json, key, kind));
    }
    visit(data(
        json,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Json)),
    ));
    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Json),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Json),
    ));
}
