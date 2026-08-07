//! `%ArrayBuffer%` constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::ArrayBufferPrototypeMethod;
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, NativeFunctionKind, PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::ArrayBufferPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::ArrayBufferConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::ArrayBuffer),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::ArrayBufferIsView,
        IntrinsicNameSpec::RealmName(RealmNameId::ArrayBufferIsView),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::ArrayBufferSpeciesGetter,
        IntrinsicNameSpec::Literal("get [Symbol.species]"),
        0,
    ));
    for method in ArrayBufferPrototypeMethod::ALL {
        let name = if method.is_accessor() {
            IntrinsicNameSpec::Literal(match method {
                ArrayBufferPrototypeMethod::ByteLength => "get byteLength",
                ArrayBufferPrototypeMethod::Detached => "get detached",
                ArrayBufferPrototypeMethod::MaxByteLength => "get maxByteLength",
                ArrayBufferPrototypeMethod::Resizable => "get resizable",
                _ => unreachable!("only ArrayBuffer accessors reach this arm"),
            })
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayBufferPrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::ArrayBufferPrototype(method),
            name,
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::ArrayBufferConstructor,
    ));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::ArrayBufferPrototype);

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::ArrayBuffer),
        NativeFunctionKind::ArrayBufferConstructor,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::ArrayBufferPrototype),
    ));
    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayBufferIsView),
        NativeFunctionKind::ArrayBufferIsView,
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::ArrayBufferSpeciesGetter,
        )),
        None,
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::ArrayBufferConstructor,
    ));
    for method_id in ArrayBufferPrototypeMethod::ALL {
        let key = match method_id {
            ArrayBufferPrototypeMethod::MaxByteLength => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::MaxByteLength)
            }
            method => IntrinsicKeySpec::InternedString(RealmNameId::ArrayBufferPrototype(method)),
        };
        if method_id.is_accessor() {
            visit(accessor(
                prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::ArrayBufferPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                prototype,
                key,
                NativeFunctionKind::ArrayBufferPrototype(method_id),
            ));
        }
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("ArrayBuffer")),
    ));
}
