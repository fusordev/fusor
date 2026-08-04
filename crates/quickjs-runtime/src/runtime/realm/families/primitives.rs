//! Boolean, Number, `BigInt`, and primitive prototype declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, data, method, object, object_prototype, ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, FROZEN_PROPERTY, IDENTITY_PROPERTY, LOCALE_STRING_METHODS,
    NUMBER_FORMAT_METHODS, NUMBER_PREDEFINED_VALUE_STATICS, NUMBER_PREDICATE_STATICS,
    NUMBER_VALUE_STATICS, NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicIdentityPublication, IntrinsicKeySpec,
        IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec,
        IntrinsicValueSpec, RealmNameId,
    },
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
            IntrinsicObjectKind::Ordinary,
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
        let mut constructor_spec = ordinary(constructor, IntrinsicNameSpec::Predefined(name), 1);
        if matches!(
            constructor,
            NativeFunctionKind::BooleanConstructor | NativeFunctionKind::NumberConstructor
        ) {
            constructor_spec.identity_publication =
                IntrinsicIdentityPublication::AutomaticAfterPrototype;
        }
        visit(constructor_spec);
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
        let mut spec = ordinary(kind, name, length);
        if kind == NativeFunctionKind::BigIntConstructor {
            spec.identity_publication = IntrinsicIdentityPublication::AutomaticAfterPrototype;
        }
        visit(spec);
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    visit_boolean_properties(visit);
    visit_number_properties(visit);
    visit_bigint_properties(visit);
    visit_locale_properties(visit);
    for (key, constructor) in [
        (
            PredefinedAtom::Boolean,
            NativeFunctionKind::BooleanConstructor,
        ),
        (
            PredefinedAtom::Number,
            NativeFunctionKind::NumberConstructor,
        ),
        (
            PredefinedAtom::BigInt,
            NativeFunctionKind::BigIntConstructor,
        ),
        (
            PredefinedAtom::String,
            NativeFunctionKind::StringConstructor,
        ),
    ] {
        visit(method(
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            IntrinsicKeySpec::PredefinedString(key),
            constructor,
        ));
    }
}

fn visit_boolean_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::BooleanPrototype);
    for (key, function) in [
        (
            PredefinedAtom::Constructor,
            NativeFunctionKind::BooleanConstructor,
        ),
        (
            PredefinedAtom::ToString,
            NativeFunctionKind::BooleanPrototypeToString,
        ),
        (
            PredefinedAtom::ValueOf,
            NativeFunctionKind::BooleanPrototypeValueOf,
        ),
    ] {
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
    visit(constructor_prototype(
        NativeFunctionKind::BooleanConstructor,
        IntrinsicObjectId::BooleanPrototype,
    ));
}

fn visit_number_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::NumberPrototype);
    for (key, function) in [
        (
            PredefinedAtom::Constructor,
            NativeFunctionKind::NumberConstructor,
        ),
        (
            PredefinedAtom::ToString,
            NativeFunctionKind::NumberPrototypeToString,
        ),
        (
            PredefinedAtom::ValueOf,
            NativeFunctionKind::NumberPrototypeValueOf,
        ),
    ] {
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::NumberConstructor));
    visit(constructor_prototype(
        NativeFunctionKind::NumberConstructor,
        IntrinsicObjectId::NumberPrototype,
    ));
    for (atom, bits) in NUMBER_PREDEFINED_VALUE_STATICS {
        visit(data(
            constructor,
            IntrinsicKeySpec::PredefinedString(atom),
            FROZEN_PROPERTY,
            IntrinsicValueSpec::NumberBits(bits),
        ));
    }
    for (name, bits) in NUMBER_VALUE_STATICS {
        visit(data(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::NumberValue(name)),
            FROZEN_PROPERTY,
            IntrinsicValueSpec::NumberBits(bits),
        ));
    }
    for (_, predicate) in NUMBER_PREDICATE_STATICS {
        visit(method(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::NumberPredicate(predicate)),
            NativeFunctionKind::NumberPredicateStatic(predicate),
        ));
    }
    for format in NUMBER_FORMAT_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::NumberFormat(format)),
            NativeFunctionKind::NumberPrototypeFormat(format),
        ));
    }
}

fn visit_bigint_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::BigIntPrototype);
    for (key, function) in [
        (
            PredefinedAtom::Constructor,
            NativeFunctionKind::BigIntConstructor,
        ),
        (
            PredefinedAtom::ToString,
            NativeFunctionKind::BigIntPrototypeToString,
        ),
        (
            PredefinedAtom::ValueOf,
            NativeFunctionKind::BigIntPrototypeValueOf,
        ),
    ] {
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::BigInt)),
    ));
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::BigIntConstructor));
    visit(constructor_prototype(
        NativeFunctionKind::BigIntConstructor,
        IntrinsicObjectId::BigIntPrototype,
    ));
    for function in [
        NativeFunctionKind::BigIntAsIntN,
        NativeFunctionKind::BigIntAsUintN,
    ] {
        visit(method(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::BigIntStatic(function)),
            function,
        ));
    }
}

fn visit_locale_properties(visit: PropertySink<'_>) {
    for (method_id, holder) in LOCALE_STRING_METHODS.into_iter().skip(1).zip([
        IntrinsicObjectId::NumberPrototype,
        IntrinsicObjectId::BigIntPrototype,
        IntrinsicObjectId::ArrayPrototype,
    ]) {
        visit(method(
            IntrinsicIdentity::Object(holder),
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString),
            NativeFunctionKind::LocaleString(method_id),
        ));
    }
}

const fn constructor_prototype(
    constructor: NativeFunctionKind,
    prototype: IntrinsicObjectId,
) -> crate::runtime::realm::schema::IntrinsicPropertySpec {
    data(
        IntrinsicIdentity::Function(IntrinsicFunctionId(constructor)),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(prototype),
    )
}
