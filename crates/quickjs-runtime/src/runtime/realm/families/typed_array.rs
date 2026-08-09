//! Concrete typed-array constructors and their shared prototype surface.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, function, function_prototype, method,
    object, object_prototype, ordinary,
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
    runtime::{ArrayStatic, PredefinedAtom, PropertyLayout, TypedArrayPrototypeMethod},
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
    visit(function(
        NativeFunctionKind::TypedArrayBaseConstructor,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype())),
        IntrinsicNameSpec::Literal("TypedArray"),
        0,
    ));
    for method_id in [ArrayStatic::From, ArrayStatic::Of] {
        visit(ordinary(
            NativeFunctionKind::TypedArrayStatic(method_id),
            IntrinsicNameSpec::Predefined(
                method_id
                    .predefined_atom()
                    .expect("TypedArray static factories have predefined names"),
            ),
            method_id.length(),
        ));
    }
    let abstract_constructor = IntrinsicFunctionId(NativeFunctionKind::TypedArrayBaseConstructor);
    for element in TypedArrayElementType::ALL {
        visit(function(
            NativeFunctionKind::TypedArrayConstructor(element),
            PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(abstract_constructor)),
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
    visit(ordinary(
        NativeFunctionKind::TypedArraySpeciesGetter,
        IntrinsicNameSpec::Literal("get [Symbol.species]"),
        0,
    ));
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    visit_abstract_constructor_properties(&mut *visit);
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::TypedArrayPrototype);
    visit_typed_array_prototype_properties(&mut *visit, prototype);

    for element in TypedArrayElementType::ALL {
        visit_concrete_typed_array_properties(&mut *visit, element);
    }
}

fn visit_abstract_constructor_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::TypedArrayPrototype);
    let abstract_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TypedArrayBaseConstructor,
    ));
    visit(data(
        abstract_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TypedArrayPrototype),
    ));
    for method_id in [ArrayStatic::From, ArrayStatic::Of] {
        visit(method(
            abstract_constructor,
            IntrinsicKeySpec::PredefinedString(
                method_id
                    .predefined_atom()
                    .expect("TypedArray static factories have predefined names"),
            ),
            NativeFunctionKind::TypedArrayStatic(method_id),
        ));
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        PropertyLayout::data(true, false, true),
        IntrinsicValueSpec::Function(IntrinsicFunctionId(
            NativeFunctionKind::TypedArrayBaseConstructor,
        )),
    ));
}

fn visit_typed_array_prototype_properties(visit: PropertySink<'_>, prototype: IntrinsicIdentity) {
    for prototype_method in TypedArrayPrototypeMethod::ALL {
        let key = match prototype_method {
            TypedArrayPrototypeMethod::ToStringTag => {
                IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag)
            }
            TypedArrayPrototypeMethod::Entries => {
                IntrinsicKeySpec::InternedString(RealmNameId::Entries)
            }
            TypedArrayPrototypeMethod::Keys => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Keys)
            }
            TypedArrayPrototypeMethod::Values => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Values)
            }
            TypedArrayPrototypeMethod::Join => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Join)
            }
            _ => {
                IntrinsicKeySpec::InternedString(RealmNameId::TypedArrayPrototype(prototype_method))
            }
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
    visit(method(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
        NativeFunctionKind::TypedArrayPrototype(TypedArrayPrototypeMethod::Values),
    ));
}

fn visit_concrete_typed_array_properties(visit: PropertySink<'_>, element: TypedArrayElementType) {
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
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::TypedArraySpeciesGetter,
        )),
        None,
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
