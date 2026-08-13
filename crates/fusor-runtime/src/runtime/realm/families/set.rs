//! Set constructor, prototype, and iterator declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom,
    PropertyLayout, SetMethod,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::SetPrototype,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::SetIteratorPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::IteratorPrototype,
        )),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::SetConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Set),
            0,
        ),
        (
            NativeFunctionKind::SetGroupBy,
            IntrinsicNameSpec::RealmName(RealmNameId::ObjectStatic(
                NativeFunctionKind::ObjectGroupBy,
            )),
            2,
        ),
        (
            NativeFunctionKind::SetSpeciesGetter,
            IntrinsicNameSpec::Literal("get [Symbol.species]"),
            0,
        ),
        (
            NativeFunctionKind::SetIteratorNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
            0,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for method in SetMethod::ALL {
        let name = match method {
            SetMethod::Add => IntrinsicNameSpec::Predefined(PredefinedAtom::Add),
            SetMethod::Has => IntrinsicNameSpec::Predefined(PredefinedAtom::Has),
            SetMethod::Delete => IntrinsicNameSpec::Predefined(PredefinedAtom::Delete),
            SetMethod::Size => IntrinsicNameSpec::Literal("get size"),
            SetMethod::Values => IntrinsicNameSpec::Predefined(PredefinedAtom::Values),
            SetMethod::Entries => IntrinsicNameSpec::RealmName(RealmNameId::Entries),
            SetMethod::Clear
            | SetMethod::ForEach
            | SetMethod::IsDisjointFrom
            | SetMethod::IsSubsetOf
            | SetMethod::IsSupersetOf
            | SetMethod::Intersection
            | SetMethod::Difference
            | SetMethod::SymmetricDifference
            | SetMethod::Union => IntrinsicNameSpec::RealmName(RealmNameId::SetMethod(method)),
        };
        visit(ordinary(
            NativeFunctionKind::SetPrototype(method),
            name,
            method.length(),
        ));
    }
}

fn method_key(method: SetMethod) -> IntrinsicKeySpec {
    match method {
        SetMethod::Add => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Add),
        SetMethod::Has => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Has),
        SetMethod::Delete => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Delete),
        SetMethod::Size => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Size),
        SetMethod::Values => IntrinsicKeySpec::PredefinedString(PredefinedAtom::Values),
        SetMethod::Entries => IntrinsicKeySpec::InternedString(RealmNameId::Entries),
        SetMethod::Clear
        | SetMethod::ForEach
        | SetMethod::IsDisjointFrom
        | SetMethod::IsSubsetOf
        | SetMethod::IsSupersetOf
        | SetMethod::Intersection
        | SetMethod::Difference
        | SetMethod::SymmetricDifference
        | SetMethod::Union => IntrinsicKeySpec::InternedString(RealmNameId::SetMethod(method)),
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::SetConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::SetPrototype);

    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::ObjectStatic(
            NativeFunctionKind::ObjectGroupBy,
        )),
        NativeFunctionKind::SetGroupBy,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::SetPrototype),
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(NativeFunctionKind::SetSpeciesGetter)),
        None,
    ));

    for method_id in SetMethod::ALL {
        if method_id == SetMethod::Size {
            visit(accessor(
                prototype,
                method_key(method_id),
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(NativeFunctionKind::SetPrototype(
                    method_id,
                ))),
                None,
            ));
        } else {
            visit(method(
                prototype,
                method_key(method_id),
                NativeFunctionKind::SetPrototype(method_id),
            ));
        }
        if method_id == SetMethod::Values {
            visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Keys),
                NativeFunctionKind::SetPrototype(SetMethod::Values),
            ));
        }
    }
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::SetConstructor,
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
        NativeFunctionKind::SetPrototype(SetMethod::Values),
    ));
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::Set)),
    ));

    let iterator = IntrinsicIdentity::Object(IntrinsicObjectId::SetIteratorPrototype);
    visit(method(
        iterator,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Next),
        NativeFunctionKind::SetIteratorNext,
    ));
    visit(data(
        iterator,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::SetIterator)),
    ));

    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Set),
        NativeFunctionKind::SetConstructor,
    ));
}
