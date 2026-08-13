//! Error constructor and prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, function_prototype, method, object,
    object_prototype, ordinary,
};
use crate::runtime::PropertyLayout;
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, ErrorIntrinsicKind, METHOD_PROPERTY, NativeFunctionKind,
    PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for kind in ErrorIntrinsicKind::ALL {
        let prototype = if kind == ErrorIntrinsicKind::Error {
            object_prototype()
        } else {
            PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
                IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::Error),
            ))
        };
        visit(object(
            IntrinsicObjectId::ErrorPrototype(kind),
            prototype,
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    for kind in ErrorIntrinsicKind::ALL {
        let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::ErrorPrototype(kind));
        if kind == ErrorIntrinsicKind::Error {
            visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString),
                NativeFunctionKind::ErrorPrototypeToString,
            ));
            visit(accessor(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Stack),
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::ErrorPrototypeStackGetter,
                )),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::ErrorPrototypeStackSetter,
                )),
            ));
        }
        visit(data(
            prototype,
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Name),
            METHOD_PROPERTY,
            IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(kind.predefined_atom())),
        ));
        visit(data(
            prototype,
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Message),
            METHOD_PROPERTY,
            IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
                PredefinedAtom::EmptyString,
            )),
        ));
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
            NativeFunctionKind::ErrorConstructor(kind),
        ));
    }
    for kind in ErrorIntrinsicKind::ALL {
        let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
            NativeFunctionKind::ErrorConstructor(kind),
        ));
        if kind == ErrorIntrinsicKind::Error {
            visit(method(
                constructor,
                IntrinsicKeySpec::InternedString(RealmNameId::IsError),
                NativeFunctionKind::ErrorIsError,
            ));
        }
        visit(data(
            constructor,
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
            CONSTRUCTOR_PROTOTYPE_PROPERTY,
            IntrinsicValueSpec::Object(IntrinsicObjectId::ErrorPrototype(kind)),
        ));
    }
    for kind in ErrorIntrinsicKind::ALL {
        visit(method(
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            IntrinsicKeySpec::PredefinedString(kind.predefined_atom()),
            NativeFunctionKind::ErrorConstructor(kind),
        ));
    }
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    let error_constructor = IntrinsicFunctionId(NativeFunctionKind::ErrorConstructor(
        ErrorIntrinsicKind::Error,
    ));
    for kind in ErrorIntrinsicKind::ALL {
        let prototype = if kind == ErrorIntrinsicKind::Error {
            function_prototype()
        } else {
            error_constructor
        };
        visit(super::function(
            NativeFunctionKind::ErrorConstructor(kind),
            PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(prototype)),
            IntrinsicNameSpec::Predefined(kind.predefined_atom()),
            i32::from(kind == ErrorIntrinsicKind::AggregateError) + 1,
        ));
    }
    for (kind, name, length) in [
        (
            NativeFunctionKind::ErrorPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::ErrorPrototypeStackGetter,
            IntrinsicNameSpec::Literal("get stack"),
            0,
        ),
        (
            NativeFunctionKind::ErrorPrototypeStackSetter,
            IntrinsicNameSpec::Literal("set stack"),
            1,
        ),
        (
            NativeFunctionKind::ErrorIsError,
            IntrinsicNameSpec::RealmName(RealmNameId::IsError),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}
