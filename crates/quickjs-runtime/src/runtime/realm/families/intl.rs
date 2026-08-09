//! `%Intl%` namespace declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, METHOD_PROPERTY, NativeFunctionKind,
    PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec, IntrinsicValueSpec,
        RealmNameId,
    },
};
use crate::runtime::{
    IntlCollatorPrototypeMethod, IntlDateTimeFormatPrototypeMethod, IntlLocalePrototypeMethod,
    IntlNumberFormatPrototypeMethod,
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    for id in [
        IntrinsicObjectId::Intl,
        IntrinsicObjectId::IntlCollatorPrototype,
        IntrinsicObjectId::IntlNumberFormatPrototype,
        IntrinsicObjectId::IntlDateTimeFormatPrototype,
        IntrinsicObjectId::IntlLocalePrototype,
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
    reason = "the declarative Intl function graph stays together for identity and arity audits"
)]
pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::IntlGetCanonicalLocales,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlGetCanonicalLocales),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlSupportedValuesOf,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlSupportedValuesOf),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlCollatorConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::Collator),
        0,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlCollatorSupportedLocalesOf,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlCollatorSupportedLocalesOf),
        1,
    ));
    for method in IntlCollatorPrototypeMethod::ALL {
        let name = if method.is_accessor() {
            IntrinsicNameSpec::Literal("get compare")
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::IntlCollatorPrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::IntlCollatorPrototype(method),
            name,
            0,
        ));
    }
    visit(ordinary(
        NativeFunctionKind::IntlCollatorCompare,
        IntrinsicNameSpec::Literal(""),
        2,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlNumberFormatConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlNumberFormat),
        0,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlNumberFormatSupportedLocalesOf,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlNumberFormatSupportedLocalesOf),
        1,
    ));
    for method in IntlNumberFormatPrototypeMethod::ALL {
        let name = if method.is_accessor() {
            IntrinsicNameSpec::Literal("get format")
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::IntlNumberFormatPrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::IntlNumberFormatPrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::IntlNumberFormatFormat,
        IntrinsicNameSpec::Literal(""),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlDateTimeFormatConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlDateTimeFormat),
        0,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlDateTimeFormatSupportedLocalesOf,
        IntrinsicNameSpec::RealmName(RealmNameId::IntlDateTimeFormatSupportedLocalesOf),
        1,
    ));
    for method in IntlDateTimeFormatPrototypeMethod::ALL {
        let name = if method.is_accessor() {
            IntrinsicNameSpec::Literal("get format")
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::IntlDateTimeFormatPrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::IntlDateTimeFormatPrototype(method),
            name,
            method.length(),
        ));
    }
    visit(ordinary(
        NativeFunctionKind::IntlDateTimeFormatFormat,
        IntrinsicNameSpec::Literal(""),
        1,
    ));
    visit(ordinary(
        NativeFunctionKind::IntlLocaleConstructor,
        IntrinsicNameSpec::RealmName(RealmNameId::Locale),
        1,
    ));
    for method in IntlLocalePrototypeMethod::ALL {
        let name = if matches!(method, IntlLocalePrototypeMethod::ToString) {
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString)
        } else if method.is_accessor() {
            IntrinsicNameSpec::Literal(method.function_name())
        } else {
            IntrinsicNameSpec::RealmName(RealmNameId::IntlLocalePrototype(method))
        };
        visit(ordinary(
            NativeFunctionKind::IntlLocalePrototype(method),
            name,
            0,
        ));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the declarative Intl graph stays together for descriptor and identity audits"
)]
pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let intl = IntrinsicIdentity::Object(IntrinsicObjectId::Intl);
    let collator_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::IntlCollatorConstructor,
    ));
    let collator_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::IntlCollatorPrototype);
    let number_format_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::IntlNumberFormatConstructor,
    ));
    let number_format_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::IntlNumberFormatPrototype);
    let date_time_format_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::IntlDateTimeFormatConstructor,
    ));
    let date_time_format_prototype =
        IntrinsicIdentity::Object(IntrinsicObjectId::IntlDateTimeFormatPrototype);
    let locale_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::IntlLocaleConstructor,
    ));
    let locale_prototype = IntrinsicIdentity::Object(IntrinsicObjectId::IntlLocalePrototype);
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::Collator),
        NativeFunctionKind::IntlCollatorConstructor,
    ));
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlGetCanonicalLocales),
        NativeFunctionKind::IntlGetCanonicalLocales,
    ));
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlNumberFormat),
        NativeFunctionKind::IntlNumberFormatConstructor,
    ));
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlDateTimeFormat),
        NativeFunctionKind::IntlDateTimeFormatConstructor,
    ));
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlSupportedValuesOf),
        NativeFunctionKind::IntlSupportedValuesOf,
    ));
    visit(method(
        intl,
        IntrinsicKeySpec::InternedString(RealmNameId::Locale),
        NativeFunctionKind::IntlLocaleConstructor,
    ));
    visit(data(
        intl,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::String(IntrinsicStringSpec::RealmName(RealmNameId::Intl)),
    ));
    visit(data(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::InternedString(RealmNameId::Intl),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::Intl),
    ));

    visit(data(
        collator_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::IntlCollatorPrototype),
    ));
    visit(method(
        collator_constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlCollatorSupportedLocalesOf),
        NativeFunctionKind::IntlCollatorSupportedLocalesOf,
    ));
    visit(method(
        collator_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::IntlCollatorConstructor,
    ));
    for method_id in IntlCollatorPrototypeMethod::ALL {
        let key = IntrinsicKeySpec::InternedString(RealmNameId::IntlCollatorPrototype(method_id));
        if method_id.is_accessor() {
            visit(accessor(
                collator_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::IntlCollatorPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                collator_prototype,
                key,
                NativeFunctionKind::IntlCollatorPrototype(method_id),
            ));
        }
    }
    visit(data(
        collator_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Intl.Collator")),
    ));

    visit(data(
        number_format_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::IntlNumberFormatPrototype),
    ));
    visit(method(
        number_format_constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlNumberFormatSupportedLocalesOf),
        NativeFunctionKind::IntlNumberFormatSupportedLocalesOf,
    ));
    visit(method(
        number_format_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::IntlNumberFormatConstructor,
    ));
    for method_id in IntlNumberFormatPrototypeMethod::ALL {
        let key =
            IntrinsicKeySpec::InternedString(RealmNameId::IntlNumberFormatPrototype(method_id));
        if method_id.is_accessor() {
            visit(accessor(
                number_format_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::IntlNumberFormatPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                number_format_prototype,
                key,
                NativeFunctionKind::IntlNumberFormatPrototype(method_id),
            ));
        }
    }
    visit(data(
        number_format_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Intl.NumberFormat")),
    ));

    visit(data(
        date_time_format_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::IntlDateTimeFormatPrototype),
    ));
    visit(method(
        date_time_format_constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::IntlDateTimeFormatSupportedLocalesOf),
        NativeFunctionKind::IntlDateTimeFormatSupportedLocalesOf,
    ));
    visit(method(
        date_time_format_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::IntlDateTimeFormatConstructor,
    ));
    for method_id in IntlDateTimeFormatPrototypeMethod::ALL {
        let key =
            IntrinsicKeySpec::InternedString(RealmNameId::IntlDateTimeFormatPrototype(method_id));
        if method_id.is_accessor() {
            visit(accessor(
                date_time_format_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::IntlDateTimeFormatPrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                date_time_format_prototype,
                key,
                NativeFunctionKind::IntlDateTimeFormatPrototype(method_id),
            ));
        }
    }
    visit(data(
        date_time_format_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Intl.DateTimeFormat")),
    ));

    visit(data(
        locale_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::IntlLocalePrototype),
    ));
    visit(method(
        locale_prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::IntlLocaleConstructor,
    ));
    for method_id in IntlLocalePrototypeMethod::ALL {
        let key = if matches!(method_id, IntlLocalePrototypeMethod::ToString) {
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString)
        } else {
            IntrinsicKeySpec::InternedString(RealmNameId::IntlLocalePrototype(method_id))
        };
        if method_id.is_accessor() {
            visit(accessor(
                locale_prototype,
                key,
                PropertyLayout::accessor(false, true),
                Some(IntrinsicFunctionId(
                    NativeFunctionKind::IntlLocalePrototype(method_id),
                )),
                None,
            ));
        } else {
            visit(method(
                locale_prototype,
                key,
                NativeFunctionKind::IntlLocalePrototype(method_id),
            ));
        }
    }
    visit(data(
        locale_prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolToStringTag),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::String(IntrinsicStringSpec::Literal("Intl.Locale")),
    ));
}
