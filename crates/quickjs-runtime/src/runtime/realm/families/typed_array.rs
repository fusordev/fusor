//! Concrete typed-array constructors and their shared prototype surface.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, NativeFunctionKind,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicValueSpec, PrototypeSpec, RealmNameId,
    },
};
use crate::{
    object::TypedArrayElementType,
    runtime::{PredefinedAtom, PropertyLayout, TypedArrayPrototypeMethod},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::TypedArrayPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
    for element in TypedArrayElementType::ALL {
        visit(object(
            IntrinsicObjectId::TypedArrayInstancePrototype(element),
            PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
                IntrinsicObjectId::TypedArrayPrototype,
            )),
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for element in TypedArrayElementType::ALL {
        visit(ordinary(
            NativeFunctionKind::TypedArrayConstructor(element),
            IntrinsicNameSpec::Predefined(constructor_atom(element)),
            3,
        ));
    }
    for method in TypedArrayPrototypeMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::TypedArrayPrototype(method),
            IntrinsicNameSpec::Literal(method.accessor_name()),
            method.arity(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::TypedArrayPrototype);
    for prototype_method in TypedArrayPrototypeMethod::ALL {
        let key = if prototype_method == TypedArrayPrototypeMethod::ToStringTag {
            IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag)
        } else {
            IntrinsicKeySpec::InternedString(RealmNameId::TypedArrayPrototype(prototype_method))
        };
        if prototype_method.is_accessor() {
            visit(accessor(
                prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TypedArrayPrototype(prototype_method),
                )),
                None,
            ));
        } else {
            visit(method(
                prototype,
                key,
                NativeFunctionKind::TypedArrayPrototype(prototype_method),
            ));
        }
    }

    for element in TypedArrayElementType::ALL {
        let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
            NativeFunctionKind::TypedArrayConstructor(element),
        ));
        let prototype =
            IntrinsicIdentity::Object(IntrinsicObjectId::TypedArrayInstancePrototype(element));
        let width = IntrinsicValueSpec::NumberBits(element_width(element).to_bits());

        visit(method(
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            IntrinsicKeySpec::PredefinedString(constructor_atom(element)),
            NativeFunctionKind::TypedArrayConstructor(element),
        ));
        visit(data(
            constructor,
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
            CONSTRUCTOR_PROTOTYPE_PROPERTY,
            IntrinsicValueSpec::Object(IntrinsicObjectId::TypedArrayInstancePrototype(element)),
        ));
        visit(data(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::TypedArrayBytesPerElement),
            PropertyLayout::data(false, false, false),
            width,
        ));
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
            NativeFunctionKind::TypedArrayConstructor(element),
        ));
        visit(data(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::TypedArrayBytesPerElement),
            PropertyLayout::data(false, false, false),
            width,
        ));
    }
}

const fn constructor_atom(element: TypedArrayElementType) -> PredefinedAtom {
    match element {
        TypedArrayElementType::Int8 => PredefinedAtom::Int8Array,
        TypedArrayElementType::Uint8 => PredefinedAtom::Uint8Array,
        TypedArrayElementType::Uint8Clamped => PredefinedAtom::Uint8ClampedArray,
        TypedArrayElementType::Int16 => PredefinedAtom::Int16Array,
        TypedArrayElementType::Uint16 => PredefinedAtom::Uint16Array,
        TypedArrayElementType::Int32 => PredefinedAtom::Int32Array,
        TypedArrayElementType::Uint32 => PredefinedAtom::Uint32Array,
        TypedArrayElementType::BigInt64 => PredefinedAtom::BigInt64Array,
        TypedArrayElementType::BigUint64 => PredefinedAtom::BigUint64Array,
        TypedArrayElementType::Float16 => PredefinedAtom::Float16Array,
        TypedArrayElementType::Float32 => PredefinedAtom::Float32Array,
        TypedArrayElementType::Float64 => PredefinedAtom::Float64Array,
    }
}

const fn element_width(element: TypedArrayElementType) -> f64 {
    match element {
        TypedArrayElementType::Int8
        | TypedArrayElementType::Uint8
        | TypedArrayElementType::Uint8Clamped => 1.0,
        TypedArrayElementType::Int16
        | TypedArrayElementType::Uint16
        | TypedArrayElementType::Float16 => 2.0,
        TypedArrayElementType::Int32
        | TypedArrayElementType::Uint32
        | TypedArrayElementType::Float32 => 4.0,
        TypedArrayElementType::BigInt64
        | TypedArrayElementType::BigUint64
        | TypedArrayElementType::Float64 => 8.0,
    }
}
