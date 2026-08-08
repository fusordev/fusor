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
    TemporalPlainDateTimePrototypeMethod, TemporalPlainDateTimeStaticMethod,
    TemporalPlainMonthDayPrototypeMethod, TemporalPlainMonthDayStaticMethod,
    TemporalPlainTimePrototypeMethod, TemporalPlainTimeStaticMethod,
    TemporalPlainYearMonthPrototypeMethod, TemporalPlainYearMonthStaticMethod,
    TemporalZonedDateTimePrototypeMethod, TemporalZonedDateTimeStaticMethod,
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::Temporal,
        IntrinsicObjectId::TemporalDurationPrototype,
        IntrinsicObjectId::TemporalInstantPrototype,
        IntrinsicObjectId::TemporalPlainDatePrototype,
        IntrinsicObjectId::TemporalPlainDateTimePrototype,
        IntrinsicObjectId::TemporalPlainTimePrototype,
        IntrinsicObjectId::TemporalPlainMonthDayPrototype,
        IntrinsicObjectId::TemporalPlainYearMonthPrototype,
        IntrinsicObjectId::TemporalZonedDateTimePrototype,
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
            TemporalInstantPrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalInstantPrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            TemporalInstantPrototypeMethod::Add
            | TemporalInstantPrototypeMethod::Subtract
            | TemporalInstantPrototypeMethod::Until
            | TemporalInstantPrototypeMethod::Since
            | TemporalInstantPrototypeMethod::Round
            | TemporalInstantPrototypeMethod::ToZonedDateTimeISO
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
    for method in TemporalPlainDateTimeStaticMethod::ALL {
        let name = match method {
            TemporalPlainDateTimeStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalPlainDateTimeStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainDateTimeStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainDateTimeStatic(method),
            name,
            method.length(),
        ));
    }
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
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalPlainTimeConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::PlainTime),
        0,
    ));
    for method in TemporalPlainTimeStaticMethod::ALL {
        let name = match method {
            TemporalPlainTimeStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalPlainTimeStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainTimeStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainTimeStatic(method),
            name,
            method.length(),
        ));
    }
    for method in TemporalPlainTimePrototypeMethod::ALL {
        let name = match method {
            TemporalPlainTimePrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalPlainTimePrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalPlainTimePrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainTimePrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainTimePrototype(method)),
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainTimePrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalPlainMonthDayConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::PlainMonthDay),
        2,
    ));
    for method in TemporalPlainMonthDayStaticMethod::ALL {
        let name = match method {
            TemporalPlainMonthDayStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalPlainMonthDayStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainMonthDayStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainMonthDayStatic(method),
            name,
            method.length(),
        ));
    }
    for method in TemporalPlainMonthDayPrototypeMethod::ALL {
        let name = match method {
            TemporalPlainMonthDayPrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalPlainMonthDayPrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalPlainMonthDayPrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainMonthDayPrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainMonthDayPrototype(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainMonthDayPrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalPlainYearMonthConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::PlainYearMonth),
        2,
    ));
    for method in TemporalPlainYearMonthStaticMethod::ALL {
        let name = match method {
            TemporalPlainYearMonthStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalPlainYearMonthStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainYearMonthStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainYearMonthStatic(method),
            name,
            method.length(),
        ));
    }
    for method in TemporalPlainYearMonthPrototypeMethod::ALL {
        let name = match method {
            TemporalPlainYearMonthPrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalPlainYearMonthPrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalPlainYearMonthPrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainYearMonthPrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalPlainYearMonthPrototype(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalPlainYearMonthPrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::TemporalZonedDateTimeConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::ZonedDateTime),
        2,
    ));
    for method in TemporalZonedDateTimeStaticMethod::ALL {
        let name = match method {
            TemporalZonedDateTimeStaticMethod::From => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::From)
            }
            TemporalZonedDateTimeStaticMethod::Compare => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalZonedDateTimeStatic(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalZonedDateTimeStatic(method),
            name,
            method.length(),
        ));
    }
    for method in TemporalZonedDateTimePrototypeMethod::ALL {
        let name = match method {
            TemporalZonedDateTimePrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            TemporalZonedDateTimePrototypeMethod::ToJson => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToJson)
            }
            TemporalZonedDateTimePrototypeMethod::ToLocaleString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString)
            }
            TemporalZonedDateTimePrototypeMethod::ValueOf => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf)
            }
            method if method.is_accessor() => IntrinsicNameSpec::Literal(method.function_name()),
            method => {
                IntrinsicNameSpec::RealmName(RealmNameId::TemporalZonedDateTimePrototype(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::TemporalZonedDateTimePrototype(method),
            name,
            method.length(),
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
    let plain_time_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalPlainTimePrototype);
    let plain_time_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalPlainTimeConstructor,
    ));
    let plain_month_day_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalPlainMonthDayPrototype);
    let plain_month_day_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalPlainMonthDayConstructor,
    ));
    let plain_year_month_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalPlainYearMonthPrototype);
    let plain_year_month_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalPlainYearMonthConstructor,
    ));
    let zoned_date_time_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::TemporalZonedDateTimePrototype);
    let zoned_date_time_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::TemporalZonedDateTimeConstructor,
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
            TemporalInstantPrototypeMethod::ToLocaleString => visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString),
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
            | TemporalInstantPrototypeMethod::ToZonedDateTimeISO
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
    for method_id in TemporalPlainDateTimeStaticMethod::ALL {
        let key = match method_id {
            TemporalPlainDateTimeStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalPlainDateTimeStaticMethod::Compare => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalPlainDateTimeStatic(method_id),
            ),
        };
        visit(method(
            plain_date_time_constructor,
            key,
            NativeFunctionKind::TemporalPlainDateTimeStatic(method_id),
        ));
    }
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
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::PlainTime),
        NativeFunctionKind::TemporalPlainTimeConstructor,
    ));
    visit(data(
        plain_time_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalPlainTimePrototype),
    ));
    visit(method(
        plain_time_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalPlainTimeConstructor,
    ));
    for method_id in TemporalPlainTimeStaticMethod::ALL {
        let key = match method_id {
            TemporalPlainTimeStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalPlainTimeStaticMethod::Compare => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalPlainTimeStatic(method_id))
            }
        };
        visit(method(
            plain_time_constructor,
            key,
            NativeFunctionKind::TemporalPlainTimeStatic(method_id),
        ));
    }
    for method_id in TemporalPlainTimePrototypeMethod::ALL {
        let key = match method_id {
            TemporalPlainTimePrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalPlainTimePrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalPlainTimePrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainTimePrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => {
                IntrinsicKeySpec::InternedString(RealmNameId::TemporalPlainTimePrototype(method))
            }
        };
        if method_id.is_accessor() {
            visit(accessor(
                plain_time_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalPlainTimePrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                plain_time_prototype,
                key,
                NativeFunctionKind::TemporalPlainTimePrototype(method_id),
            ));
        }
    }
    visit(data(
        plain_time_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.PlainTime")),
    ));
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::PlainMonthDay),
        NativeFunctionKind::TemporalPlainMonthDayConstructor,
    ));
    visit(data(
        plain_month_day_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalPlainMonthDayPrototype),
    ));
    visit(method(
        plain_month_day_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalPlainMonthDayConstructor,
    ));
    for method_id in TemporalPlainMonthDayStaticMethod::ALL {
        let key = match method_id {
            TemporalPlainMonthDayStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalPlainMonthDayStaticMethod::Compare => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalPlainMonthDayStatic(method_id),
            ),
        };
        visit(method(
            plain_month_day_constructor,
            key,
            NativeFunctionKind::TemporalPlainMonthDayStatic(method_id),
        ));
    }
    for method_id in TemporalPlainMonthDayPrototypeMethod::ALL {
        let key = match method_id {
            TemporalPlainMonthDayPrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalPlainMonthDayPrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalPlainMonthDayPrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainMonthDayPrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalPlainMonthDayPrototype(method),
            ),
        };
        if method_id.is_accessor() {
            visit(accessor(
                plain_month_day_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalPlainMonthDayPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                plain_month_day_prototype,
                key,
                NativeFunctionKind::TemporalPlainMonthDayPrototype(method_id),
            ));
        }
    }
    visit(data(
        plain_month_day_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.PlainMonthDay")),
    ));
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::PlainYearMonth),
        NativeFunctionKind::TemporalPlainYearMonthConstructor,
    ));
    visit(data(
        plain_year_month_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalPlainYearMonthPrototype),
    ));
    visit(method(
        plain_year_month_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalPlainYearMonthConstructor,
    ));
    for method_id in TemporalPlainYearMonthStaticMethod::ALL {
        let key = match method_id {
            TemporalPlainYearMonthStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalPlainYearMonthStaticMethod::Compare => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalPlainYearMonthStatic(method_id),
            ),
        };
        visit(method(
            plain_year_month_constructor,
            key,
            NativeFunctionKind::TemporalPlainYearMonthStatic(method_id),
        ));
    }
    for method_id in TemporalPlainYearMonthPrototypeMethod::ALL {
        let key = match method_id {
            TemporalPlainYearMonthPrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalPlainYearMonthPrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalPlainYearMonthPrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalPlainYearMonthPrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalPlainYearMonthPrototype(method),
            ),
        };
        if method_id.is_accessor() {
            visit(accessor(
                plain_year_month_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalPlainYearMonthPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                plain_year_month_prototype,
                key,
                NativeFunctionKind::TemporalPlainYearMonthPrototype(method_id),
            ));
        }
    }
    visit(data(
        plain_year_month_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.PlainYearMonth")),
    ));
    visit(method(
        namespace,
        IntrinsicKeySpec::InternedString(RealmNameId::ZonedDateTime),
        NativeFunctionKind::TemporalZonedDateTimeConstructor,
    ));
    visit(data(
        zoned_date_time_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::TemporalZonedDateTimePrototype),
    ));
    visit(method(
        zoned_date_time_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::TemporalZonedDateTimeConstructor,
    ));
    for method_id in TemporalZonedDateTimeStaticMethod::ALL {
        let key = match method_id {
            TemporalZonedDateTimeStaticMethod::From => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::From)
            }
            TemporalZonedDateTimeStaticMethod::Compare => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalZonedDateTimeStatic(method_id),
            ),
        };
        visit(method(
            zoned_date_time_constructor,
            key,
            NativeFunctionKind::TemporalZonedDateTimeStatic(method_id),
        ));
    }
    for method_id in TemporalZonedDateTimePrototypeMethod::ALL {
        let key = match method_id {
            TemporalZonedDateTimePrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            TemporalZonedDateTimePrototypeMethod::ToJson => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToJson)
            }
            TemporalZonedDateTimePrototypeMethod::ToLocaleString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString)
            }
            TemporalZonedDateTimePrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            method => IntrinsicKeySpec::InternedString(
                RealmNameId::TemporalZonedDateTimePrototype(method),
            ),
        };
        if method_id.is_accessor() {
            visit(accessor(
                zoned_date_time_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::TemporalZonedDateTimePrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                zoned_date_time_prototype,
                key,
                NativeFunctionKind::TemporalZonedDateTimePrototype(method_id),
            ));
        }
    }
    visit(data(
        zoned_date_time_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Temporal.ZonedDateTime")),
    ));
}
