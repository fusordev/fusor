//! String method and factory declarations.

use super::{FunctionSink, PropertySink, data, method, ordinary};
use crate::runtime::StringMethod;
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, IDENTITY_PROPERTY, NativeFunctionKind, PredefinedAtom,
    STRING_FROM_STATICS, STRING_PROTOTYPE_METHODS,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicValueSpec, RealmNameId,
    },
};

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for method in STRING_PROTOTYPE_METHODS {
        let name = method.predefined_name.map_or(
            IntrinsicNameSpec::RealmName(RealmNameId::StringMethod(method.method)),
            IntrinsicNameSpec::Predefined,
        );
        visit(ordinary(
            NativeFunctionKind::StringPrototypeMethod(method.method),
            name,
            method.length,
        ));
    }
    for (_, method) in STRING_FROM_STATICS {
        visit(ordinary(
            NativeFunctionKind::StringPrototypeMethod(method),
            IntrinsicNameSpec::RealmName(RealmNameId::StringStatic(method)),
            1,
        ));
    }
    visit(ordinary(
        NativeFunctionKind::StringRaw,
        IntrinsicNameSpec::Predefined(PredefinedAtom::Raw),
        1,
    ));
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::StringPrototype);
    visit(data(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Length),
        IDENTITY_PROPERTY,
        IntrinsicValueSpec::NumberBits(0_f64.to_bits()),
    ));
    for method_spec in STRING_PROTOTYPE_METHODS {
        if method_spec.method == StringMethod::ToLowerCase {
            for (key, function) in [
                (
                    PredefinedAtom::ToString,
                    NativeFunctionKind::StringPrototypeToString,
                ),
                (
                    PredefinedAtom::ValueOf,
                    NativeFunctionKind::StringPrototypeValueOf,
                ),
            ] {
                visit(method(
                    prototype,
                    IntrinsicKeySpec::PredefinedString(key),
                    function,
                ));
            }
        }
        if method_spec.method == StringMethod::Normalize {
            visit(method(
                prototype,
                IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
                NativeFunctionKind::StringConstructor,
            ));
        }
        let key = method_spec.predefined_name.map_or(
            IntrinsicKeySpec::InternedString(RealmNameId::StringMethod(method_spec.method)),
            IntrinsicKeySpec::PredefinedString,
        );
        visit(method(
            prototype,
            key,
            NativeFunctionKind::StringPrototypeMethod(method_spec.method),
        ));
        let alias = match method_spec.method {
            StringMethod::TrimEnd => Some("trimRight"),
            StringMethod::TrimStart => Some("trimLeft"),
            _ => None,
        };
        if let Some(alias) = alias {
            visit(method(
                prototype,
                IntrinsicKeySpec::InternedString(RealmNameId::StringAlias(alias)),
                NativeFunctionKind::StringPrototypeMethod(method_spec.method),
            ));
        }
    }
    visit(method(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolIterator),
        NativeFunctionKind::StringPrototypeIterator,
    ));
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::StringConstructor));
    for (_, method_id) in STRING_FROM_STATICS {
        visit(method(
            constructor,
            IntrinsicKeySpec::InternedString(RealmNameId::StringStatic(method_id)),
            NativeFunctionKind::StringPrototypeMethod(method_id),
        ));
    }
    visit(method(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Raw),
        NativeFunctionKind::StringRaw,
    ));
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::StringPrototype),
    ));
}
