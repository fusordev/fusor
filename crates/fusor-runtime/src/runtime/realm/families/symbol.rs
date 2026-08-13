//! Symbol constructor, prototype, registry, and accessor declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, DYNAMIC_SYMBOL_STATIC_PROPERTIES, FROZEN_PROPERTY,
    IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicIdentityPublication, IntrinsicKeySpec,
        IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec,
        IntrinsicValueSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::SymbolPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    let mut constructor = ordinary(
        NativeFunctionKind::SymbolConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::Symbol),
        0,
    );
    constructor.identity_publication = IntrinsicIdentityPublication::Declared;
    visit(constructor);
    for (kind, name, length) in [
        (
            NativeFunctionKind::SymbolPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeValueOf,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeToPrimitive,
            IntrinsicNameSpec::Literal("[Symbol.toPrimitive]"),
            1,
        ),
        (
            NativeFunctionKind::SymbolPrototypeDescription,
            IntrinsicNameSpec::Literal("get description"),
            0,
        ),
        (
            NativeFunctionKind::SymbolFor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::For),
            1,
        ),
        (
            NativeFunctionKind::SymbolKeyFor,
            IntrinsicNameSpec::RealmName(RealmNameId::KeyFor),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::SymbolPrototype);
    for (key, function) in [
        (
            PredefinedAtom::Constructor,
            NativeFunctionKind::SymbolConstructor,
        ),
        (
            PredefinedAtom::ToString,
            NativeFunctionKind::SymbolPrototypeToString,
        ),
        (
            PredefinedAtom::ValueOf,
            NativeFunctionKind::SymbolPrototypeValueOf,
        ),
    ] {
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToPrimitive),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(
            NativeFunctionKind::SymbolPrototypeToPrimitive,
        )),
    ));
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Symbol)),
    ));
    visit(accessor(
        prototype,
        IntrinsicKeySpec::InternedString(RealmNameId::Description),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::SymbolPrototypeDescription,
        )),
        None,
    ));
    visit_constructor_properties(visit);
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Symbol),
        NativeFunctionKind::SymbolConstructor,
    ));
}

fn visit_constructor_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::SymbolConstructor));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::SymbolPrototype),
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Length),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::NumberBits(0_f64.to_bits()),
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Name),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Symbol)),
    ));
    visit(method(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::For),
        NativeFunctionKind::SymbolFor,
    ));
    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::KeyFor),
        NativeFunctionKind::SymbolKeyFor,
    ));
    for (index, (_, symbol)) in DYNAMIC_SYMBOL_STATIC_PROPERTIES.into_iter().enumerate() {
        if index == 6 {
            visit(data(
                constructor,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Split),
                FROZEN_PROPERTY,
                IntrinsicValueSpec::WellKnownSymbol(PredefinedAtom::SymbolSplit),
            ));
        }
        visit(data(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::SymbolStatic(symbol)),
            FROZEN_PROPERTY,
            IntrinsicValueSpec::WellKnownSymbol(symbol),
        ));
    }
}
