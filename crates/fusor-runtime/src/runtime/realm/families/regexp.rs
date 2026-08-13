//! `RegExp` constructor, prototype, accessor, and protocol declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, NativeFunctionKind, PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicValueSpec, RealmNameId,
    },
};
use crate::runtime::{RegExpFlag, RegExpSymbolMethod};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::RegExpPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::RegExpConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::RegExp),
            2,
        ),
        (
            NativeFunctionKind::RegExpEscape,
            IntrinsicNameSpec::RealmName(RealmNameId::RegExpEscape),
            1,
        ),
        (
            NativeFunctionKind::RegExpSpeciesGetter,
            IntrinsicNameSpec::Literal("get [Symbol.species]"),
            0,
        ),
        (
            NativeFunctionKind::RegExpPrototypeFlags,
            IntrinsicNameSpec::Literal("get flags"),
            0,
        ),
        (
            NativeFunctionKind::RegExpPrototypeSource,
            IntrinsicNameSpec::Literal("get source"),
            0,
        ),
        (
            NativeFunctionKind::RegExpPrototypeExec,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Exec),
            1,
        ),
        (
            NativeFunctionKind::RegExpPrototypeCompile,
            IntrinsicNameSpec::RealmName(RealmNameId::RegExpCompile),
            2,
        ),
        (
            NativeFunctionKind::RegExpPrototypeTest,
            IntrinsicNameSpec::RealmName(RealmNameId::RegExpTest),
            1,
        ),
        (
            NativeFunctionKind::RegExpPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for flag in RegExpFlag::ALL {
        visit(ordinary(
            NativeFunctionKind::RegExpPrototypeFlag(flag),
            IntrinsicNameSpec::Literal(match flag {
                RegExpFlag::Global => "get global",
                RegExpFlag::IgnoreCase => "get ignoreCase",
                RegExpFlag::Multiline => "get multiline",
                RegExpFlag::DotAll => "get dotAll",
                RegExpFlag::Unicode => "get unicode",
                RegExpFlag::UnicodeSets => "get unicodeSets",
                RegExpFlag::Sticky => "get sticky",
                RegExpFlag::HasIndices => "get hasIndices",
            }),
            0,
        ));
    }
    for method in RegExpSymbolMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::RegExpPrototypeSymbol(method),
            IntrinsicNameSpec::Literal(method.name()),
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::RegExpConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::RegExpPrototype);

    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::RegExpEscape),
        NativeFunctionKind::RegExpEscape,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::RegExpPrototype),
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(NativeFunctionKind::RegExpSpeciesGetter)),
        None,
    ));

    for (key, getter) in [
        (
            PredefinedAtom::Flags,
            NativeFunctionKind::RegExpPrototypeFlags,
        ),
        (
            PredefinedAtom::Source,
            NativeFunctionKind::RegExpPrototypeSource,
        ),
    ] {
        visit(accessor(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            PropertyLayout::accessor(false, true),
            Some(IntrinsicFunctionId(getter)),
            None,
        ));
    }
    for flag in RegExpFlag::ALL {
        visit(accessor(
            prototype,
            IntrinsicKeySpec::PredefinedString(flag.atom()),
            PropertyLayout::accessor(false, true),
            Some(IntrinsicFunctionId(
                NativeFunctionKind::RegExpPrototypeFlag(flag),
            )),
            None,
        ));
    }
    for (key, function) in [
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Exec),
            NativeFunctionKind::RegExpPrototypeExec,
        ),
        (
            IntrinsicKeySpec::InternedString(RealmNameId::RegExpCompile),
            NativeFunctionKind::RegExpPrototypeCompile,
        ),
        (
            IntrinsicKeySpec::InternedString(RealmNameId::RegExpTest),
            NativeFunctionKind::RegExpPrototypeTest,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString),
            NativeFunctionKind::RegExpPrototypeToString,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
            NativeFunctionKind::RegExpConstructor,
        ),
    ] {
        visit(method(prototype, key, function));
    }
    for method_id in RegExpSymbolMethod::ALL {
        visit(method(
            prototype,
            IntrinsicKeySpec::WellKnownSymbol(method_id.atom()),
            NativeFunctionKind::RegExpPrototypeSymbol(method_id),
        ));
    }

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::RegExp),
        NativeFunctionKind::RegExpConstructor,
    ));
}
