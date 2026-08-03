//! Object/Function bootstrap kernel declarations.

use super::{FunctionSink, ObjectSink, function, object, object_prototype, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, OBJECT_PROTOTYPE_REFLECTION, OBJECT_STATIC_METHODS, PredefinedAtom,
    schema::{
        IntrinsicIdentity, IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind,
        PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::ObjectPrototype,
        PrototypeSpec::Null,
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::GlobalObject,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(function(
        NativeFunctionKind::FunctionPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::ObjectPrototype,
        )),
        IntrinsicNameSpec::Predefined(PredefinedAtom::EmptyString),
        0,
    ));
    for (kind, name, length) in [
        (
            NativeFunctionKind::ThrowTypeError,
            IntrinsicNameSpec::Predefined(PredefinedAtom::EmptyString),
            0,
        ),
        (
            NativeFunctionKind::OrdinaryFunctionConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Function),
            1,
        ),
        (
            NativeFunctionKind::ObjectConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Object),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for method in OBJECT_STATIC_METHODS {
        let name = method.predefined_name.map_or_else(
            || {
                IntrinsicNameSpec::RealmName(
                    method
                        .realm_name
                        .unwrap_or(RealmNameId::ObjectStatic(method.kind)),
                )
            },
            IntrinsicNameSpec::Predefined,
        );
        visit(ordinary(method.kind, name, method.length));
    }
    for (kind, name, length) in [
        (
            NativeFunctionKind::ObjectPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::ObjectPrototypeValueOf,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for (_, kind, length) in OBJECT_PROTOTYPE_REFLECTION {
        visit(ordinary(
            kind,
            IntrinsicNameSpec::RealmName(RealmNameId::ObjectPrototypeMethod(kind)),
            length,
        ));
    }
    for (kind, name, length) in [
        (
            NativeFunctionKind::FunctionPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::FunctionPrototypeCall,
            IntrinsicNameSpec::RealmName(RealmNameId::Call),
            1,
        ),
        (
            NativeFunctionKind::FunctionPrototypeApply,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Apply),
            2,
        ),
        (
            NativeFunctionKind::FunctionPrototypeBind,
            IntrinsicNameSpec::RealmName(RealmNameId::Bind),
            1,
        ),
        (
            NativeFunctionKind::FunctionPrototypeHasInstance,
            IntrinsicNameSpec::Literal("[Symbol.hasInstance]"),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}
