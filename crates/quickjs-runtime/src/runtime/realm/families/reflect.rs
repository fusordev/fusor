//! `%Reflect%` object and method declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, ReflectMethod,
    schema::{IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Reflect,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for method in ReflectMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Reflect(method),
            IntrinsicNameSpec::Predefined(method.predefined_atom()),
            method.length(),
        ));
    }
}
