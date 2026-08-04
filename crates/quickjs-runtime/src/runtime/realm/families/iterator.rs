//! Iterator prototype and iterator function declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicObjectId,
        IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec, PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::IteratorPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::AsyncIteratorPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::AsyncFromSyncIteratorPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::AsyncIteratorPrototype,
        )),
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
            NativeFunctionKind::AsyncIteratorPrototypeAsyncIterator,
            IntrinsicNameSpec::Literal("[Symbol.asyncIterator]"),
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
    for (kind, name) in [
        (
            NativeFunctionKind::AsyncFromSyncIteratorNext,
            PredefinedAtom::Next,
        ),
        (
            NativeFunctionKind::AsyncFromSyncIteratorReturn,
            PredefinedAtom::Return,
        ),
        (
            NativeFunctionKind::AsyncFromSyncIteratorThrow,
            PredefinedAtom::Throw,
        ),
    ] {
        visit(ordinary(kind, IntrinsicNameSpec::Predefined(name), 1));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::IteratorPrototype),
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
        NativeFunctionKind::IteratorPrototypeIterator,
    ));
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::AsyncIteratorPrototype),
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolAsyncIterator),
        NativeFunctionKind::AsyncIteratorPrototypeAsyncIterator,
    ));
    let async_from_sync =
        IntrinsicIdentity::Object(IntrinsicObjectId::AsyncFromSyncIteratorPrototype);
    for (key, function) in [
        (
            PredefinedAtom::Next,
            NativeFunctionKind::AsyncFromSyncIteratorNext,
        ),
        (
            PredefinedAtom::Return,
            NativeFunctionKind::AsyncFromSyncIteratorReturn,
        ),
        (
            PredefinedAtom::Throw,
            NativeFunctionKind::AsyncFromSyncIteratorThrow,
        ),
    ] {
        visit(method(
            async_from_sync,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }

    let array_iterator = IntrinsicIdentity::Object(IntrinsicObjectId::ArrayIteratorPrototype);
    visit(method(
        array_iterator,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Next),
        NativeFunctionKind::ArrayIteratorNext,
    ));
    visit(data(
        array_iterator,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::ArrayIterator,
        )),
    ));

    let array = IntrinsicIdentity::Object(IntrinsicObjectId::ArrayPrototype);
    for (key, function) in [
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Values),
            NativeFunctionKind::ArrayPrototypeValues,
        ),
        (
            IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
            NativeFunctionKind::ArrayPrototypeValues,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Keys),
            NativeFunctionKind::ArrayPrototypeKeys,
        ),
        (
            IntrinsicKeySpec::InternedString(RealmNameId::Entries),
            NativeFunctionKind::ArrayPrototypeEntries,
        ),
    ] {
        visit(method(array, key, function));
    }

    let string_iterator = IntrinsicIdentity::Object(IntrinsicObjectId::StringIteratorPrototype);
    visit(method(
        string_iterator,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Next),
        NativeFunctionKind::StringIteratorNext,
    ));
    visit(data(
        string_iterator,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::StringIterator,
        )),
    ));
}
