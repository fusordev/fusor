//! Boolean, Number, `BigInt`, and primitive prototype declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, PredefinedAtom,
    schema::{IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, RealmNameId},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for (id, kind) in [
        (
            IntrinsicObjectId::BooleanPrototype,
            IntrinsicObjectKind::BooleanPrototype,
        ),
        (
            IntrinsicObjectId::NumberPrototype,
            IntrinsicObjectKind::NumberPrototype,
        ),
        (
            IntrinsicObjectId::BigIntPrototype,
            IntrinsicObjectKind::BigIntPrototype,
        ),
        (
            IntrinsicObjectId::StringPrototype,
            IntrinsicObjectKind::StringPrototype,
        ),
    ] {
        visit(object(id, object_prototype(), kind));
    }
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (constructor, name, to_string, to_string_length, value_of) in [
        (
            NativeFunctionKind::BooleanConstructor,
            PredefinedAtom::Boolean,
            NativeFunctionKind::BooleanPrototypeToString,
            0,
            NativeFunctionKind::BooleanPrototypeValueOf,
        ),
        (
            NativeFunctionKind::NumberConstructor,
            PredefinedAtom::Number,
            NativeFunctionKind::NumberPrototypeToString,
            1,
            NativeFunctionKind::NumberPrototypeValueOf,
        ),
        (
            NativeFunctionKind::StringConstructor,
            PredefinedAtom::String,
            NativeFunctionKind::StringPrototypeToString,
            0,
            NativeFunctionKind::StringPrototypeValueOf,
        ),
    ] {
        visit(ordinary(
            constructor,
            IntrinsicNameSpec::Predefined(name),
            1,
        ));
        visit(ordinary(
            to_string,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            to_string_length,
        ));
        visit(ordinary(
            value_of,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ));
    }

    for (kind, name, length) in [
        (
            NativeFunctionKind::BigIntConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::BigInt),
            1,
        ),
        (
            NativeFunctionKind::BigIntPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::BigIntPrototypeValueOf,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ),
        (
            NativeFunctionKind::BigIntAsIntN,
            IntrinsicNameSpec::RealmName(RealmNameId::BigIntStatic(
                NativeFunctionKind::BigIntAsIntN,
            )),
            2,
        ),
        (
            NativeFunctionKind::BigIntAsUintN,
            IntrinsicNameSpec::RealmName(RealmNameId::BigIntStatic(
                NativeFunctionKind::BigIntAsUintN,
            )),
            2,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}
