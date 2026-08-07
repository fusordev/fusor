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
use crate::runtime::{
    TemporalDurationPrototypeMethod, TemporalDurationStaticMethod, TemporalInstantPrototypeMethod,
    TemporalInstantStaticMethod, TemporalPlainDatePrototypeMethod, TemporalPlainDateStaticMethod,
    TemporalPlainDateTimePrototypeMethod,
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::Temporal,
        IntrinsicObjectId::TemporalDurationPrototype,
        IntrinsicObjectId::TemporalInstantPrototype,
        IntrinsicObjectId::TemporalPlainDatePrototype,
        IntrinsicObjectId::TemporalPlainDateTimePrototype,
    ] {
        visit(object(
            id,
            object_prototype(),
            IntrinsicObjectKind::Ordinary,
        ));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the Temporal family keeps its complete native-function topology in one auditable declaration"
)]
pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::TemporalDurationConstructor,
        IntrinsicNameSpec::Literal("Duration"),
        0,
    ));
    for method in TemporalDurationStaticMethod::ALL {
        let name = match method {
            TemporalDurationStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalDurationStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalDurationStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalDurationStatic(method),
            name,
            method.length(),
        ));
    }
    for method in TemporalDurationPrototypeMethod::ALL {
        let name = match method {
            TemporalDurationPrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalDurationPrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalDurationPrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalDurationPrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => IntrinsicNameSpec::RealmName(RealmNameId::TemporalDurationPrototype(method)),
        };
        visit(ordinary(
            NativeFunctionKind::TemporalDurationPrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalInstantConstructor,
        IntrinsicNameSpec::Literal("Instant"),
        1,
    ));
    for method in TemporalInstantStaticMethod::ALL {
        let name = match method {
            TemporalInstantStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalInstantStaticMethod::Compare
            | TemporalInstantStaticMethod::FromEpochMilliseconds
            | TemporalInstantStaticMethod::FromEpochNanoseconds => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalInstantStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalInstantStatic(method),
            name,
            method.length(),
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
            TemporalInstantPrototypeMethod::Add
            | TemporalInstantPrototypeMethod::Subtract
            | TemporalInstantPrototypeMethod::Until
            | TemporalInstantPrototypeMethod::Since
            | TemporalInstantPrototypeMethod::Round
            | TemporalInstantPrototypeMethod::Equals => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalInstantPrototype(method))
            }
            TemporalInstantPrototypeMethod::EpochMilliseconds
            | TemporalInstantPrototypeMethod::EpochNanoseconds => {
                IntrinsicNameSpec::Literal(method.function_name())
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalInstantPrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalPlainDateConstructor,
        IntrinsicNameSpec::Literal("PlainDate"),
        3,
    ));
    for method in TemporalPlainDateStaticMethod::ALL {
        let name = match method {
            TemporalPlainDateStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalPlainDateStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainDateStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainDateStatic(method),
            name,
            method.length(),
        ));
    }
    for method in TemporalPlainDatePrototypeMethod::ALL {
        let name = match method {
            TemporalPlainDatePrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalPlainDatePrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalPlainDatePrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainDatePrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainDatePrototype(method)),
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainDatePrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalPlainDateTimeConstructor,
        IntrinsicNameSpec::Literal("PlainDateTime"),
        3,
    ));
    for method in TemporalPlainDateTimePrototypeMethod::ALL {
        let name = match method {
            TemporalPlainDateTimePrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalPlainDateTimePrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalPlainDateTimePrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainDateTimePrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainDateTimePrototype(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainDateTimePrototype(method),
            name,
            TemporalPlainDateTimePrototypeMethod::length(),
        ));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the Temporal schema keeps its namespace and prototype descriptors in one auditable declaration"
)]
pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let namespace = IntrinsicIdentity::Object(IntrinsicObjectId::Temporal);
    let duration_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalDurationPrototype);
    let duration_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalDurationConstructor,
    ));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::TemporalInstantPrototype);
    let constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalInstantConstructor,
    ));
    let plain_date_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalPlainDatePrototype);
    let plain_date_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalPlainDateConstructor,
    ));
    let plain_date_time_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalPlainDateTimePrototype);
    let plain_date_time_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalPlainDateTimeConstructor,
    ));

    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::InternedString(RealmNameId::Temporal),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Temporal),
    ));
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::Duration),
        NativeFunctionKind::TemporalDurationConstructor,
    ));
    visit(data(
        duration_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalDurationPrototype),
    ));
    visit(method(
        duration_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalDurationConstructor,
    ));
    for method_id in TemporalDurationStaticMethod::ALL {
        let key = match method_id {
            TemporalDurationStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalDurationStaticMethod::Compare => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalDurationStatic(method_id))
            }
        };
        visit(method(
            duration_constructor,
            key,
            NativeFunctionKind::TemporalDurationStatic(method_id),
        ));
    }
    for method_id in TemporalDurationPrototypeMethod::ALL {
        let key = match method_id {
            TemporalDurationPrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalDurationPrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalDurationPrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalDurationPrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalDurationPrototype(method))
            }
        };
        if method_id.is_accessor() {
            visit(accessor(
                duration_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalDurationPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                duration_prototype,
                key,
                NativeFunctionKind::TemporalDurationPrototype(method_id),
            ));
        }
    }
    visit(data(
        duration_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.Duration")),
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
        let key = match method_id {
            TemporalInstantStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalInstantStaticMethod::Compare
            | TemporalInstantStaticMethod::FromEpochMilliseconds
            | TemporalInstantStaticMethod::FromEpochNanoseconds => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalInstantStatic(method_id))
            }
        };
        visit(method(
            constructor,
            key,
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
            TemporalInstantPrototypeMethod::Add
            | TemporalInstantPrototypeMethod::Subtract
            | TemporalInstantPrototypeMethod::Until
            | TemporalInstantPrototypeMethod::Since
            | TemporalInstantPrototypeMethod::Round
            | TemporalInstantPrototypeMethod::Equals => visit(method(
                prototype,
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalInstantPrototype(method_id)),
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
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::PlainDate),
        NativeFunctionKind::TemporalPlainDateConstructor,
    ));
    visit(data(
        plain_date_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalPlainDatePrototype),
    ));
    visit(method(
        plain_date_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalPlainDateConstructor,
    ));
    for method_id in TemporalPlainDateStaticMethod::ALL {
        let key = match method_id {
            TemporalPlainDateStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalPlainDateStaticMethod::Compare => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalPlainDateStatic(method_id))
            }
        };
        visit(method(
            plain_date_constructor,
            key,
            NativeFunctionKind::TemporalPlainDateStatic(method_id),
        ));
    }
    for method_id in TemporalPlainDatePrototypeMethod::ALL {
        let key = match method_id {
            TemporalPlainDatePrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalPlainDatePrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalPlainDatePrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainDatePrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalPlainDatePrototype(method))
            }
        };
        if method_id.is_accessor() {
            visit(accessor(
                plain_date_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalPlainDatePrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                plain_date_prototype,
                key,
                NativeFunctionKind::TemporalPlainDatePrototype(method_id),
            ));
        }
    }
    visit(data(
        plain_date_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.PlainDate")),
    ));
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::PlainDateTime),
        NativeFunctionKind::TemporalPlainDateTimeConstructor,
    ));
    visit(data(
        plain_date_time_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalPlainDateTimePrototype),
    ));
    visit(method(
        plain_date_time_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalPlainDateTimeConstructor,
    ));
    for method_id in TemporalPlainDateTimePrototypeMethod::ALL {
        let key = match method_id {
            TemporalPlainDateTimePrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalPlainDateTimePrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalPlainDateTimePrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainDateTimePrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalPlainDateTimePrototype(method),
            ),
        };
        if method_id.is_accessor() {
            visit(accessor(
                plain_date_time_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalPlainDateTimePrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                plain_date_time_prototype,
                key,
                NativeFunctionKind::TemporalPlainDateTimePrototype(method_id),
            ));
        }
    }
    visit(data(
        plain_date_time_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.PlainDateTime")),
    ));
}
