//! Map constructor, prototype, and iterator declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, MapMethod, NativeFunctionKind,
    PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::MapPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::MapIteratorPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::IteratorPrototype,
        )),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::MapConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Map),
            0,
        ),
        (
            NativeFunctionKind::MapGroupBy,
            IntrinsicNameSpec::RealmName(RealmNameId::ObjectStatic(
                NativeFunctionKind::ObjectGroupBy,
            )),
            2,
        ),
        (
            NativeFunctionKind::MapSpeciesGetter,
            IntrinsicNameSpec::Literal("get [Symbol.species]"),
            0,
        ),
        (
            NativeFunctionKind::MapIteratorNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
            0,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for method_id in MapMethod::ALL {
        let name = match method_id {
            MapMethod::Get => IntrinsicNameSpec::Predefined(PredefinedAtom::Get),
            MapMethod::Has => IntrinsicNameSpec::Predefined(PredefinedAtom::Has),
            MapMethod::Delete => IntrinsicNameSpec::Predefined(PredefinedAtom::Delete),
            MapMethod::Size => IntrinsicNameSpec::Literal("get size"),
            MapMethod::Values => IntrinsicNameSpec::Predefined(PredefinedAtom::Values),
            MapMethod::Keys => IntrinsicNameSpec::Predefined(PredefinedAtom::Keys),
            MapMethod::Entries => IntrinsicNameSpec::RealmName(RealmNameId::Entries),
            MapMethod::Set
            | MapMethod::GetOrInsert
            | MapMethod::GetOrInsertComputed
            | MapMethod::Clear
            | MapMethod::ForEach => IntrinsicNameSpec::RealmName(RealmNameId::MapMethod(method_id)),
        };
        visit(ordinary(
            NativeFunctionKind::MapPrototype(method_id),
            name,
            method_id.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::MapConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::MapPrototype);

    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::ObjectStatic(
            NativeFunctionKind::ObjectGroupBy,
        )),
        NativeFunctionKind::MapGroupBy,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::MapPrototype),
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(NativeFunctionKind::MapSpeciesGetter)),
        None,
    ));

    for method_id in MapMethod::ALL {
        let key = match method_id {
            MapMethod::Set => IntrinsicKeySpec::InternedString(RealmNameId::MapMethod(method_id)),
            MapMethod::Get => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Get),
            MapMethod::Has => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Has),
            MapMethod::Delete => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Delete),
            MapMethod::Size => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Size),
            MapMethod::Values => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Values),
            MapMethod::Keys => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Keys),
            MapMethod::Entries => IntrinsicKeySpec::InternedString(RealmNameId::Entries),
            MapMethod::GetOrInsert
            | MapMethod::GetOrInsertComputed
            | MapMethod::Clear
            | MapMethod::ForEach => {
                IntrinsicKeySpec::InternedString(RealmNameId::MapMethod(method_id))
            }
        };
        if method_id == MapMethod::Size {
            visit(accessor(
                prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(NativeFunctionKind::MapPrototype(
                    method_id,
                ))),
                None,
            ));
        } else {
            visit(method(
                prototype,
                key,
                NativeFunctionKind::MapPrototype(method_id),
            ));
        }
    }
    visit(method(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
        NativeFunctionKind::MapPrototype(MapMethod::Entries),
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::MapConstructor,
    ));
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Map)),
    ));

    let iterator = IntrinsicIdentity::Object(IntrinsicObjectId::MapIteratorPrototype);
    visit(method(
        iterator,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Next),
        NativeFunctionKind::MapIteratorNext,
    ));
    visit(data(
        iterator,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::MapIterator)),
    ));

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Map),
        NativeFunctionKind::MapConstructor,
    ));
}
