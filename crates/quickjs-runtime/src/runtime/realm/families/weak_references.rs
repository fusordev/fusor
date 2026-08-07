//! `WeakRef` and `FinalizationRegistry` constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};
use crate::runtime::{FinalizationRegistryMethod, NativeFunctionKind, PredefinedAtom};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::WeakRefPrototype,
        IntrinsicObjectId::FinalizationRegistryPrototype,
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
        NativeFunctionKind::WeakRefConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::WeakRef),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::WeakRefPrototypeDeref,
        IntrinsicNameSpec::RealmName(RealmNameId::Deref),
        0,
    ));
    visit(ordinary(
        NativeFunctionKind::FinalizationRegistryConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::FinalizationRegistry),
        1,
    ));
    for method in FinalizationRegistryMethod::ALL {
        let name = match method {
            FinalizationRegistryMethod::Register => RealmNameId::Register,
            FinalizationRegistryMethod::Unregister => RealmNameId::Unregister,
        };
        visit(ordinary(
            NativeFunctionKind::FinalizationRegistryPrototype(method),
            IntrinsicNameSpec::RealmName(name),
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let weak_ref_constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::WeakRefConstructor));
    let weak_ref_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::WeakRefPrototype);
    visit(data(
        weak_ref_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::WeakRefPrototype),
    ));
    visit(method(
        weak_ref_prototype,
        IntrinsicKeySpec::InternedString(RealmNameId::Deref),
        NativeFunctionKind::WeakRefPrototypeDeref,
    ));
    visit(method(
        weak_ref_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::WeakRefConstructor,
    ));
    visit(data(
        weak_ref_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::WeakRef)),
    ));

    let registry_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::FinalizationRegistryConstructor,
    ));
    let registry_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::FinalizationRegistryPrototype);
    visit(data(
        registry_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::FinalizationRegistryPrototype),
    ));
    for method_id in FinalizationRegistryMethod::ALL {
        let name = match method_id {
            FinalizationRegistryMethod::Register => RealmNameId::Register,
            FinalizationRegistryMethod::Unregister => RealmNameId::Unregister,
        };
        visit(method(
            registry_prototype,
            IntrinsicKeySpec::InternedString(name),
            NativeFunctionKind::FinalizationRegistryPrototype(method_id),
        ));
    }
    visit(method(
        registry_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::FinalizationRegistryConstructor,
    ));
    visit(data(
        registry_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::FinalizationRegistry,
        )),
    ));

    let global = IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject);
    visit(method(
        global,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::WeakRef),
        NativeFunctionKind::WeakRefConstructor,
    ));
    visit(method(
        global,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::FinalizationRegistry),
        NativeFunctionKind::FinalizationRegistryConstructor,
    ));
}
