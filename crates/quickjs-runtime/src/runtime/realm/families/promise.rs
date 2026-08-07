//! Promise constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom,
    PromiseStatic, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::PromisePrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::PromiseConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Promise),
            1,
        ),
        (
            NativeFunctionKind::PromiseResolve,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Resolve),
            1,
        ),
        (
            NativeFunctionKind::PromiseReject,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Reject),
            1,
        ),
        (
            NativeFunctionKind::PromiseSpeciesGetter,
            IntrinsicNameSpec::Literal("get [Symbol.species]"),
            0,
        ),
        (
            NativeFunctionKind::PromisePrototypeThen,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Then),
            2,
        ),
        (
            NativeFunctionKind::PromisePrototypeCatch,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Catch),
            1,
        ),
        (
            NativeFunctionKind::PromisePrototypeFinally,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Finally),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for method in PromiseStatic::ALL {
        let name = if method == PromiseStatic::Try {
            IntrinsicNameSpec::Predefined(PredefinedAtom::Try)
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::PromiseStatic(method))
        };
        visit(ordinary(
            NativeFunctionKind::PromiseStatic(method),
            name,
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::PromiseConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::PromisePrototype);

    for (key, function) in [
        (PredefinedAtom::Resolve, NativeFunctionKind::PromiseResolve),
        (PredefinedAtom::Reject, NativeFunctionKind::PromiseReject),
    ] {
        visit(method(
            constructor,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
    for method_id in PromiseStatic::ALL {
        let key = if method_id == PromiseStatic::Try {
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Try)
        } else {
            IntrinsicKeySpec::InternedString(RealmNameId::PromiseStatic(method_id))
        };
        visit(method(
            constructor,
            key,
            NativeFunctionKind::PromiseStatic(method_id),
        ));
    }
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::PromisePrototype),
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::PromiseSpeciesGetter,
        )),
        None,
    ));

    for (key, function) in [
        (
            PredefinedAtom::Then,
            NativeFunctionKind::PromisePrototypeThen,
        ),
        (
            PredefinedAtom::Catch,
            NativeFunctionKind::PromisePrototypeCatch,
        ),
        (
            PredefinedAtom::Finally,
            NativeFunctionKind::PromisePrototypeFinally,
        ),
        (
            PredefinedAtom::Constructor,
            NativeFunctionKind::PromiseConstructor,
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
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Promise)),
    ));

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Promise),
        NativeFunctionKind::PromiseConstructor,
    ));
}
