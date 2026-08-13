//! Async function intrinsic declarations.

use super::{FunctionSink, ObjectSink, PropertySink, data, function, function_prototype, object};
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
        IntrinsicObjectId::AsyncFunctionPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype())),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(function(
        NativeFunctionKind::AsyncFunctionConstructor,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(IntrinsicFunctionId(
            NativeFunctionKind::OrdinaryFunctionConstructor,
        ))),
        IntrinsicNameSpec::Predefined(PredefinedAtom::AsyncFunction),
        1,
    ));
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::AsyncFunctionConstructor,
    ));
    let function_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::AsyncFunctionPrototype);

    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::AsyncFunctionPrototype),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(
            NativeFunctionKind::AsyncFunctionConstructor,
        )),
    ));
    visit(data(
        function_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::AsyncFunction,
        )),
    ));
}
