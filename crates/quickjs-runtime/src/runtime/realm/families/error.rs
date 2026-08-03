//! Error constructor and prototype declarations.

use super::{FunctionSink, ObjectSink, function_prototype, object, object_prototype, ordinary};
use crate::runtime::realm::{
    ErrorIntrinsicKind, NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicNameSpec, IntrinsicObjectId,
        IntrinsicObjectKind, PrototypeSpec, RealmNameId,
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
            NativeFunctionKind::ErrorIsError,
            IntrinsicNameSpec::RealmName(RealmNameId::IsError),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}
