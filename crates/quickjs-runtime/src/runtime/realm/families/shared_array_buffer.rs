//! `%SharedArrayBuffer%` constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::SharedArrayBufferPrototypeMethod;
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
        IntrinsicObjectId::SharedArrayBufferPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::SharedArrayBufferConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::SharedArrayBuffer),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::SharedArrayBufferSpeciesGetter,
        IntrinsicNameSpec::Literal("get [Symbol.species]"),
        0,
    ));
    for method in SharedArrayBufferPrototypeMethod::ALL {
        let name = if method.is_accessor() {
            IntrinsicNameSpec::Literal(match method {
                SharedArrayBufferPrototypeMethod::ByteLength => "get byteLength",
                SharedArrayBufferPrototypeMethod::Growable => "get growable",
                SharedArrayBufferPrototypeMethod::MaxByteLength => "get maxByteLength",
                SharedArrayBufferPrototypeMethod::Grow
                | SharedArrayBufferPrototypeMethod::Slice => {
                    unreachable!("only SharedArrayBuffer accessors reach this arm")
                }
            })
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::SharedArrayBufferPrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::SharedArrayBufferPrototype(method),
            name,
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::SharedArrayBufferConstructor,
    ));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::SharedArrayBufferPrototype);

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::SharedArrayBuffer),
        NativeFunctionKind::SharedArrayBufferConstructor,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::SharedArrayBufferPrototype),
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::SharedArrayBufferSpeciesGetter,
        )),
        None,
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::SharedArrayBufferConstructor,
    ));
    for method_id in SharedArrayBufferPrototypeMethod::ALL {
        let key = match method_id {
            SharedArrayBufferPrototypeMethod::MaxByteLength => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::MaxByteLength)
            }
            method => {
                IntrinsicKeySpec::InternedString(RealmNameId::SharedArrayBufferPrototype(method))
            }
        };
        if method_id.is_accessor() {
            visit(accessor(
                prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::SharedArrayBufferPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                prototype,
                key,
                NativeFunctionKind::SharedArrayBufferPrototype(method_id),
            ));
        }
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("SharedArrayBuffer")),
    ));
}
