//! Symbol constructor, prototype, registry, and accessor declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, PredefinedAtom,
    schema::{IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, RealmNameId},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::SymbolPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::SymbolConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Symbol),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeValueOf,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeToPrimitive,
            IntrinsicNameSpec::Literal("[Symbol.toPrimitive]"),
            1,
        ),
        (
            NativeFunctionKind::SymbolPrototypeDescription,
            IntrinsicNameSpec::Literal("get description"),
            0,
        ),
        (
            NativeFunctionKind::SymbolFor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::For),
            1,
        ),
        (
            NativeFunctionKind::SymbolKeyFor,
            IntrinsicNameSpec::RealmName(RealmNameId::KeyFor),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}
