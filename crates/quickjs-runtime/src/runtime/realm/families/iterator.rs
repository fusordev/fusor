//! Iterator prototype and iterator function declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom,
    PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        PrototypeSpec, RealmNameId,
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
        IntrinsicObjectId::WrapForValidIteratorPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::IteratorPrototype,
        )),
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
        IntrinsicObjectId::RegExpStringIteratorPrototype,
    ] {
        visit(object(
            id,
            iterator_prototype,
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the specification-ordered Iterator intrinsic functions are audited as one declaration list"
)]
pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::IteratorConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Iterator),
            0,
        ),
        (
            NativeFunctionKind::IteratorFrom,
            IntrinsicNameSpec::Predefined(PredefinedAtom::From),
            1,
        ),
        (
            NativeFunctionKind::IteratorPrototypeToArray,
            IntrinsicNameSpec::RealmName(RealmNameId::IteratorToArray),
            0,
        ),
        (
            NativeFunctionKind::IteratorPrototypeConstructorGetter,
            IntrinsicNameSpec::Literal("get constructor"),
            0,
        ),
        (
            NativeFunctionKind::IteratorPrototypeConstructorSetter,
            IntrinsicNameSpec::Literal("set constructor"),
            1,
        ),
        (
            NativeFunctionKind::IteratorPrototypeToStringTagGetter,
            IntrinsicNameSpec::Literal("get [Symbol.toStringTag]"),
            0,
        ),
        (
            NativeFunctionKind::IteratorPrototypeToStringTagSetter,
            IntrinsicNameSpec::Literal("set [Symbol.toStringTag]"),
            1,
        ),
        (
            NativeFunctionKind::IteratorWrapperNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
            0,
        ),
        (
            NativeFunctionKind::IteratorWrapperReturn,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Return),
            0,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
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
            NativeFunctionKind::RegExpStringIteratorNext,
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

#[allow(
    clippy::too_many_lines,
    reason = "the specification-ordered Iterator intrinsic surface is audited as one declaration list"
)]
pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::IteratorConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::IteratorPrototype);
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::IteratorPrototype),
    ));
    visit(method(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::From),
        NativeFunctionKind::IteratorFrom,
    ));
    visit(accessor(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::IteratorPrototypeConstructorGetter,
        )),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::IteratorPrototypeConstructorSetter,
        )),
    ));
    visit(accessor(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::IteratorPrototypeToStringTagGetter,
        )),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::IteratorPrototypeToStringTagSetter,
        )),
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
        NativeFunctionKind::IteratorPrototypeIterator,
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::InternedString(RealmNameId::IteratorToArray),
        NativeFunctionKind::IteratorPrototypeToArray,
    ));
    let wrapper = IntrinsicIdentity::Object(IntrinsicObjectId::WrapForValidIteratorPrototype);
    visit(method(
        wrapper,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Next),
        NativeFunctionKind::IteratorWrapperNext,
    ));
    visit(method(
        wrapper,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Return),
        NativeFunctionKind::IteratorWrapperReturn,
    ));
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Iterator),
        NativeFunctionKind::IteratorConstructor,
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

    let regexp_string_iterator =
        IntrinsicIdentity::Object(IntrinsicObjectId::RegExpStringIteratorPrototype);
    visit(method(
        regexp_string_iterator,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Next),
        NativeFunctionKind::RegExpStringIteratorNext,
    ));
    visit(data(
        regexp_string_iterator,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(
            PredefinedAtom::RegExpStringIterator,
        )),
    ));
}
