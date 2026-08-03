//! `%Math%` object and method declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    FROZEN_PROPERTY, IDENTITY_PROPERTY, MATH_CONSTANTS, METHOD_PROPERTY, MathMethod,
    NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId,
        IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Math,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for method in MathMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Math(method),
            IntrinsicNameSpec::RealmName(RealmNameId::MathMethod(method)),
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let math = IntrinsicIdentity::Object(IntrinsicObjectId::Math);
    for method_id in MathMethod::ALL {
        visit(method(
            math,
            IntrinsicKeySpec::InternedString(RealmNameId::MathMethod(method_id)),
            NativeFunctionKind::Math(method_id),
        ));
    }
    for (name, bits) in MATH_CONSTANTS {
        visit(data(
            math,
            IntrinsicKeySpec::InternedString(RealmNameId::MathConstant(name)),
            FROZEN_PROPERTY,
            IntrinsicValueSpec::NumberBits(bits),
        ));
    }
    visit(data(
        math,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Math)),
    ));
    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Math),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Math),
    ));
}
