//! Asynchronous generator intrinsic declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, function, function_prototype, method, object,
    ordinary,
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
        IntrinsicObjectId::AsyncGeneratorFunctionPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype())),
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::AsyncGeneratorPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::AsyncIteratorPrototype,
        )),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(function(
        NativeFunctionKind::AsyncGeneratorFunctionConstructor,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(IntrinsicFunctionId(
            NativeFunctionKind::OrdinaryFunctionConstructor,
        ))),
        IntrinsicNameSpec::Predefined(PredefinedAtom::AsyncGeneratorFunction),
        1,
    ));
    for (kind, name) in [
        (
            NativeFunctionKind::AsyncGeneratorPrototypeNext,
            PredefinedAtom::Next,
        ),
        (
            NativeFunctionKind::AsyncGeneratorPrototypeReturn,
            PredefinedAtom::Return,
        ),
        (
            NativeFunctionKind::AsyncGeneratorPrototypeThrow,
            PredefinedAtom::Throw,
        ),
    ] {
        visit(ordinary(kind, IntrinsicNameSpec::Predefined(name), 1));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::AsyncGeneratorFunctionConstructor,
    ));
    let function_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::AsyncGeneratorFunctionPrototype);
    let generator_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::AsyncGeneratorPrototype);

    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::AsyncGeneratorFunctionPrototype),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(
            NativeFunctionKind::AsyncGeneratorFunctionConstructor,
        )),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::AsyncGeneratorPrototype),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::AsyncGeneratorFunction,
        )),
    ));
    for (key, implementation) in [
        (
            PredefinedAtom::Next,
            NativeFunctionKind::AsyncGeneratorPrototypeNext,
        ),
        (
            PredefinedAtom::Return,
            NativeFunctionKind::AsyncGeneratorPrototypeReturn,
        ),
        (
            PredefinedAtom::Throw,
            NativeFunctionKind::AsyncGeneratorPrototypeThrow,
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
        IntrinsicValueSpec::Object(IntrinsicObjectId::AsyncGeneratorFunctionPrototype),
    ));
    visit(data(
        generator_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::AsyncGenerator,
        )),
    ));
}
