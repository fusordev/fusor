//! `%DataView%` constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, NativeFunctionKind,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};
use crate::runtime::{DataViewPrototypeMethod, PredefinedAtom, PropertyLayout};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::DataViewPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::DataViewConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::DataView),
        1,
    ));
    for method in DataViewPrototypeMethod::ALL {
        let name = if method.is_accessor() {
            IntrinsicNameSpec::Literal(match method {
                DataViewPrototypeMethod::Buffer => "get buffer",
                DataViewPrototypeMethod::ByteLength => "get byteLength",
                DataViewPrototypeMethod::ByteOffset => "get byteOffset",
                _ => unreachable!("only DataView accessors reach this arm"),
            })
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::DataViewPrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::DataViewPrototype(method),
            name,
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::DataViewConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::DataViewPrototype);

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::DataView),
        NativeFunctionKind::DataViewConstructor,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::DataViewPrototype),
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::DataViewConstructor,
    ));
    for method_id in DataViewPrototypeMethod::ALL {
        let key = IntrinsicKeySpec::InternedString(RealmNameId::DataViewPrototype(method_id));
        if method_id.is_accessor() {
            visit(accessor(
                prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(NativeFunctionKind::DataViewPrototype(
                    method_id,
                ))),
                None,
            ));
        } else {
            visit(method(
                prototype,
                key,
                NativeFunctionKind::DataViewPrototype(method_id),
            ));
        }
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("DataView")),
    ));
}
