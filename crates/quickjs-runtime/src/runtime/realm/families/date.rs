//! UTC/time-value `%Date%` intrinsic declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicValueSpec, RealmNameId,
    },
};
use crate::runtime::{DatePrototypeMethod, DateStaticMethod};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::DatePrototype,
        object_prototype(),
        IntrinsicObjectKind::DatePrototype,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::DateConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::Date),
        7,
    ));
    for method in DateStaticMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::DateStatic(method),
            IntrinsicNameSpec::RealmName(RealmNameId::DateStatic(method)),
            method.length(),
        ));
    }
    for method in DatePrototypeMethod::ALL {
        let name = match method {
            DatePrototypeMethod::ValueOf => IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            DatePrototypeMethod::ToString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
            }
            DatePrototypeMethod::ToIsoString => {
                IntrinsicNameSpec::Predefined(PredefinedAtom::ToIsoString)
            }
            DatePrototypeMethod::ToUtcString
            | DatePrototypeMethod::ToDateString
            | DatePrototypeMethod::ToTimeString
            | DatePrototypeMethod::GetTimezoneOffset
            | DatePrototypeMethod::GetTime
            | DatePrototypeMethod::GetFullYear
            | DatePrototypeMethod::GetUtcFullYear
            | DatePrototypeMethod::GetMonth
            | DatePrototypeMethod::GetUtcMonth
            | DatePrototypeMethod::GetDate
            | DatePrototypeMethod::GetUtcDate
            | DatePrototypeMethod::GetHours
            | DatePrototypeMethod::GetUtcHours
            | DatePrototypeMethod::GetMinutes
            | DatePrototypeMethod::GetUtcMinutes
            | DatePrototypeMethod::GetSeconds
            | DatePrototypeMethod::GetUtcSeconds
            | DatePrototypeMethod::GetMilliseconds
            | DatePrototypeMethod::GetUtcMilliseconds
            | DatePrototypeMethod::GetDay
            | DatePrototypeMethod::GetUtcDay
            | DatePrototypeMethod::SetTime => {
                IntrinsicNameSpec::RealmName(RealmNameId::DatePrototype(method))
            }
        };
        visit(ordinary(
            NativeFunctionKind::DatePrototype(method),
            name,
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::DateConstructor));
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::DatePrototype);

    for static_method in DateStaticMethod::ALL {
        visit(method(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::DateStatic(static_method)),
            NativeFunctionKind::DateStatic(static_method),
        ));
    }
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::DatePrototype),
    ));

    for method_id in DatePrototypeMethod::ALL {
        let key = match method_id {
            DatePrototypeMethod::ValueOf => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf)
            }
            DatePrototypeMethod::ToString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
            }
            DatePrototypeMethod::ToIsoString => {
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToIsoString)
            }
            DatePrototypeMethod::ToUtcString
            | DatePrototypeMethod::ToDateString
            | DatePrototypeMethod::ToTimeString
            | DatePrototypeMethod::GetTimezoneOffset
            | DatePrototypeMethod::GetTime
            | DatePrototypeMethod::GetFullYear
            | DatePrototypeMethod::GetUtcFullYear
            | DatePrototypeMethod::GetMonth
            | DatePrototypeMethod::GetUtcMonth
            | DatePrototypeMethod::GetDate
            | DatePrototypeMethod::GetUtcDate
            | DatePrototypeMethod::GetHours
            | DatePrototypeMethod::GetUtcHours
            | DatePrototypeMethod::GetMinutes
            | DatePrototypeMethod::GetUtcMinutes
            | DatePrototypeMethod::GetSeconds
            | DatePrototypeMethod::GetUtcSeconds
            | DatePrototypeMethod::GetMilliseconds
            | DatePrototypeMethod::GetUtcMilliseconds
            | DatePrototypeMethod::GetDay
            | DatePrototypeMethod::GetUtcDay
            | DatePrototypeMethod::SetTime => {
                IntrinsicKeySpec::InternedString(RealmNameId::DatePrototype(method_id))
            }
        };
        visit(method(
            prototype,
            key,
            NativeFunctionKind::DatePrototype(method_id),
        ));
    }
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::DateConstructor,
    ));
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Date),
        NativeFunctionKind::DateConstructor,
    ));
}
