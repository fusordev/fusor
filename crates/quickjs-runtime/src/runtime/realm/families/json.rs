//! `%JSON%` object and method declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, PredefinedAtom,
    schema::{IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, RealmNameId},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::Json,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::JsonIsRawJson,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonIsRawJson),
            1,
        ),
        (
            NativeFunctionKind::JsonParse,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonParse),
            2,
        ),
        (
            NativeFunctionKind::JsonRawJson,
            IntrinsicNameSpec::Predefined(PredefinedAtom::RawJson),
            1,
        ),
        (
            NativeFunctionKind::JsonStringify,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonStringify),
            3,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}
