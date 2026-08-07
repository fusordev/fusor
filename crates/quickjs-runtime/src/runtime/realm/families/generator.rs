//! Synchronous generator intrinsic declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, function_prototype, method, object, ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        PrototypeSpec,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::GeneratorFunctionPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype())),
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::GeneratorPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::IteratorPrototype,
        )),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::GeneratorFunctionConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::GeneratorFunction),
        1,
    ));
    for (kind, name) in [
        (
            NativeFunctionKind::GeneratorPrototypeNext,
            PredefinedAtom::Next,
        ),
        (
            NativeFunctionKind::GeneratorPrototypeReturn,
            PredefinedAtom::Return,
        ),
        (
            NativeFunctionKind::GeneratorPrototypeThrow,
            PredefinedAtom::Throw,
        ),
    ] {
        visit(ordinary(kind, IntrinsicNameSpec::Predefined(name), 1));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::GeneratorFunctionConstructor,
    ));
    let function_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::GeneratorFunctionPrototype);
    let generator_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::GeneratorPrototype);

    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::GeneratorFunctionPrototype),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(
            NativeFunctionKind::GeneratorFunctionConstructor,
        )),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::GeneratorPrototype),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::GeneratorFunction,
        )),
    ));

    for (key, implementation) in [
        (
            PredefinedAtom::Next,
            NativeFunctionKind::GeneratorPrototypeNext,
        ),
        (
            PredefinedAtom::Return,
            NativeFunctionKind::GeneratorPrototypeReturn,
        ),
        (
            PredefinedAtom::Throw,
            NativeFunctionKind::GeneratorPrototypeThrow,
        ),
    ] {
        visit(method(
            generator_prototype,
            IntrinsicKeySpec::PredefinedString(key),
            implementation,
        ));
    }
    visit(data(
        generator_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::GeneratorFunctionPrototype),
    ));
    visit(data(
        generator_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Generator)),
    ));
}
