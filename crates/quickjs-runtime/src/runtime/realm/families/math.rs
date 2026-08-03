//! `%Math%` object and method declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    MathMethod, NativeFunctionKind,
    schema::{IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, RealmNameId},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Math,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for method in MathMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Math(method),
            IntrinsicNameSpec::RealmName(RealmNameId::MathMethod(method)),
            method.length(),
        ));
    }
}
