//! Initial `%Temporal%` and `%Temporal.Instant%` declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, METHOD_PROPERTY, NativeFunctionKind, PredefinedAtom,
    PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};
use crate::runtime::{TemporalInstantPrototypeMethod, TemporalInstantStaticMethod};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::Temporal,
        IntrinsicObjectId::TemporalInstantPrototype,
    ] {
        visit(object(
            id,
            object_prototype(),
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::TemporalInstantConstructor,
        IntrinsicNameSpec::Literal("Instant"),
        1,
    ));
    for method in TemporalInstantStaticMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::TemporalInstantStatic(method),
            IntrinsicNameSpec::RealmName(RealmNameId::TemporalInstantStatic(method)),
            1,
        ));
    }
    for method in TemporalInstantPrototypeMethod::ALL {
        let name = match method {
            TemporalInstantPrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalInstantPrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalInstantPrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            TemporalInstantPrototypeMethod::EpochMilliseconds
            | TemporalInstantPrototypeMethod::EpochNanoseconds => {
                IntrinsicNameSpec::Literal(method.function_name())
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalInstantPrototype(method),
            name,
            0,
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let namespace = IntrinsicIdentity::Object(IntrinsicObjectId::Temporal);
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::TemporalInstantPrototype);
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalInstantConstructor,
    ));

    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::InternedString(RealmNameId::Temporal),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Temporal),
    ));
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::Instant),
        NativeFunctionKind::TemporalInstantConstructor,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalInstantPrototype),
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalInstantConstructor,
    ));
    for method_id in TemporalInstantStaticMethod::ALL {
        visit(method(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::TemporalInstantStatic(method_id)),
            NativeFunctionKind::TemporalInstantStatic(method_id),
        ));
    }
    for method_id in TemporalInstantPrototypeMethod::ALL {
        match method_id {
            TemporalInstantPrototypeMethod::EpochMilliseconds
            | TemporalInstantPrototypeMethod::EpochNanoseconds => visit(accessor(
                prototype,
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalInstantPrototype(method_id)),
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalInstantPrototype(method_id),
                )),
                None,
            )),
            TemporalInstantPrototypeMethod::ToString => visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString),
                NativeFunctionKind::TemporalInstantPrototype(method_id),
            )),
            TemporalInstantPrototypeMethod::ToJson => visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson),
                NativeFunctionKind::TemporalInstantPrototype(method_id),
            )),
            TemporalInstantPrototypeMethod::ValueOf => visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf),
                NativeFunctionKind::TemporalInstantPrototype(method_id),
            )),
        }
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.Instant")),
    ));
}
