//! Iterator prototype and iterator function declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicIdentity, IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind,
        PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::IteratorPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
    let iterator_prototype = PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
        IntrinsicObjectId::IteratorPrototype,
    ));
    for id in [
        IntrinsicObjectId::ArrayIteratorPrototype,
        IntrinsicObjectId::StringIteratorPrototype,
    ] {
        visit(object(
            id,
            iterator_prototype,
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name) in [
        (
            NativeFunctionKind::IteratorPrototypeIterator,
            IntrinsicNameSpec::Literal("[Symbol.iterator]"),
        ),
        (
            NativeFunctionKind::ArrayIteratorNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
        ),
        (
            NativeFunctionKind::ArrayPrototypeValues,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Values),
        ),
        (
            NativeFunctionKind::ArrayPrototypeKeys,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Keys),
        ),
        (
            NativeFunctionKind::ArrayPrototypeEntries,
            IntrinsicNameSpec::RealmName(RealmNameId::Entries),
        ),
        (
            NativeFunctionKind::StringIteratorNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
        ),
        (
            NativeFunctionKind::StringPrototypeIterator,
            IntrinsicNameSpec::Literal("[Symbol.iterator]"),
        ),
    ] {
        visit(ordinary(kind, name, 0));
    }
}
