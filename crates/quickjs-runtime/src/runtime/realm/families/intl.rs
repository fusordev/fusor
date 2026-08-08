//! `%Intl%` namespace declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::IntlLocalePrototypeMethod;
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, METHOD_PROPERTY, NativeFunctionKind,
    PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::Intl,
        IntrinsicObjectId::IntlLocalePrototype,
    ] {
        visit(object(
            id,
            object_prototype(),
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::IntlGetCanonicalLocales,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlGetCanonicalLocales),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlLocaleConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::Locale),
        1,
    ));
    for method in IntlLocalePrototypeMethod::ALL {
        let name = if matches!(method, IntlLocalePrototypeMethod::ToString) {
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
        } else if method.is_accessor() {
            IntrinsicNameSpec::Literal(method.function_name())
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::IntlLocalePrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::IntlLocalePrototype(method),
            name,
            0,
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let intl = IntrinsicIdentity::Object(IntrinsicObjectId::Intl);
    let locale_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::IntlLocaleConstructor,
    ));
    let locale_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::IntlLocalePrototype);
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlGetCanonicalLocales),
        NativeFunctionKind::IntlGetCanonicalLocales,
    ));
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::Locale),
        NativeFunctionKind::IntlLocaleConstructor,
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

    visit(data(
        locale_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::IntlLocalePrototype),
    ));
    visit(method(
        locale_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::IntlLocaleConstructor,
    ));
    for method_id in IntlLocalePrototypeMethod::ALL {
        let key = if matches!(method_id, IntlLocalePrototypeMethod::ToString) {
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
        } else {
            IntrinsicKeySpec::InternedString(RealmNameId::IntlLocalePrototype(method_id))
        };
        if method_id.is_accessor() {
            visit(accessor(
                locale_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::IntlLocalePrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                locale_prototype,
                key,
                NativeFunctionKind::IntlLocalePrototype(method_id),
            ));
        }
    }
    visit(data(
        locale_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Intl.Locale")),
    ));
}
