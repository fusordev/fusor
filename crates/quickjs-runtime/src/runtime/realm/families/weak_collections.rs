//! `WeakMap` and `WeakSet` constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, MapMethod, NativeFunctionKind,
    PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};
use crate::runtime::{WeakMapMethod, WeakSetMethod};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::WeakMapPrototype,
        IntrinsicObjectId::WeakSetPrototype,
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
        NativeFunctionKind::WeakMapConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::WeakMap),
        0,
    ));
    for method in WeakMapMethod::ALL {
        let name = match method {
            WeakMapMethod::Set => IntrinsicNameSpec::Predefined(PredefinedAtom::SetProperty),
            WeakMapMethod::Get => IntrinsicNameSpec::Predefined(PredefinedAtom::Get),
            WeakMapMethod::GetOrInsert => {
                IntrinsicNameSpec::RealmName(RealmNameId::MapMethod(MapMethod::GetOrInsert))
            }
            WeakMapMethod::GetOrInsertComputed => {
                IntrinsicNameSpec::RealmName(RealmNameId::MapMethod(MapMethod::GetOrInsertComputed))
            }
            WeakMapMethod::Has => IntrinsicNameSpec::Predefined(PredefinedAtom::Has),
            WeakMapMethod::Delete => IntrinsicNameSpec::Predefined(PredefinedAtom::Delete),
        };
        visit(ordinary(
            NativeFunctionKind::WeakMapPrototype(method),
            name,
            method.length(),
        ));
    }

    visit(ordinary(
        NativeFunctionKind::WeakSetConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::WeakSet),
        0,
    ));
    for method in WeakSetMethod::ALL {
        let name = match method {
            WeakSetMethod::Add => IntrinsicNameSpec::Predefined(PredefinedAtom::Add),
            WeakSetMethod::Has => IntrinsicNameSpec::Predefined(PredefinedAtom::Has),
            WeakSetMethod::Delete => IntrinsicNameSpec::Predefined(PredefinedAtom::Delete),
        };
        visit(ordinary(
            NativeFunctionKind::WeakSetPrototype(method),
            name,
            WeakSetMethod::length(),
        ));
    }
}

fn weak_map_key(method: WeakMapMethod) -> IntrinsicKeySpec {
    match method {
        WeakMapMethod::Set => IntrinsicKeySpec::PredefinedString(PredefinedAtom::SetProperty),
        WeakMapMethod::Get => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Get),
        WeakMapMethod::GetOrInsert => {
            IntrinsicKeySpec::InternedString(RealmNameId::MapMethod(MapMethod::GetOrInsert))
        }
        WeakMapMethod::GetOrInsertComputed => {
            IntrinsicKeySpec::InternedString(RealmNameId::MapMethod(MapMethod::GetOrInsertComputed))
        }
        WeakMapMethod::Has => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Has),
        WeakMapMethod::Delete => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Delete),
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let weak_map_constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::WeakMapConstructor));
    let weak_map_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::WeakMapPrototype);
    visit(data(
        weak_map_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::WeakMapPrototype),
    ));
    for weak_map_method in WeakMapMethod::ALL {
        visit(method(
            weak_map_prototype,
            weak_map_key(weak_map_method),
            NativeFunctionKind::WeakMapPrototype(weak_map_method),
        ));
    }
    visit(method(
        weak_map_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::WeakMapConstructor,
    ));
    visit(data(
        weak_map_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::WeakMap)),
    ));

    let weak_set_constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::WeakSetConstructor));
    let weak_set_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::WeakSetPrototype);
    visit(data(
        weak_set_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::WeakSetPrototype),
    ));
    for weak_set_method in WeakSetMethod::ALL {
        let key = match weak_set_method {
            WeakSetMethod::Add => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Add),
            WeakSetMethod::Has => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Has),
            WeakSetMethod::Delete => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Delete),
        };
        visit(method(
            weak_set_prototype,
            key,
            NativeFunctionKind::WeakSetPrototype(weak_set_method),
        ));
    }
    visit(method(
        weak_set_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::WeakSetConstructor,
    ));
    visit(data(
        weak_set_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::WeakSet)),
    ));

    let global = IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject);
    visit(method(
        global,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::WeakMap),
        NativeFunctionKind::WeakMapConstructor,
    ));
    visit(method(
        global,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::WeakSet),
        NativeFunctionKind::WeakSetConstructor,
    ));
}
