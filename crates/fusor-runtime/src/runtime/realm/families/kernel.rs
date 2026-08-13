//! Object/Function bootstrap kernel declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, function, method, object,
    object_prototype, ordinary,
};
use crate::runtime::realm::{
    CONSTRUCTOR_PROTOTYPE_PROPERTY, FROZEN_PROPERTY, LocaleStringMethod, METHOD_PROPERTY,
    NativeFunctionKind, OBJECT_PROTOTYPE_LEGACY_ACCESSORS, OBJECT_PROTOTYPE_REFLECTION,
    OBJECT_STATIC_METHODS, PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicIdentityPublication, IntrinsicKeySpec,
        IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, IntrinsicStringSpec,
        IntrinsicValueSpec, PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::ObjectPrototype,
        PrototypeSpec::Null,
        IntrinsicObjectKind::Ordinary,
    ));
    visit(object(
        IntrinsicObjectId::GlobalObject,
        object_prototype(),
        IntrinsicObjectKind::Ordinary,
    ));
}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(function(
        NativeFunctionKind::FunctionPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::ObjectPrototype,
        )),
        IntrinsicNameSpec::Predefined(PredefinedAtom::EmptyString),
        0,
    ));
    let mut throw_type_error = ordinary(
        NativeFunctionKind::ThrowTypeError,
        IntrinsicNameSpec::Predefined(PredefinedAtom::EmptyString),
        0,
    );
    throw_type_error.identity_publication = IntrinsicIdentityPublication::Declared;
    visit(throw_type_error);
    for (kind, name, length) in [
        (
            NativeFunctionKind::OrdinaryFunctionConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Function),
            1,
        ),
        (
            NativeFunctionKind::ObjectConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Object),
            1,
        ),
    ] {
        let mut spec = ordinary(kind, name, length);
        if kind == NativeFunctionKind::OrdinaryFunctionConstructor {
            spec.identity_publication = IntrinsicIdentityPublication::AutomaticAfterPrototype;
        }
        visit(spec);
    }
    visit_object_functions(visit);
    for (kind, name, length) in [
        (
            NativeFunctionKind::FunctionPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::FunctionPrototypeCall,
            IntrinsicNameSpec::RealmName(RealmNameId::Call),
            1,
        ),
        (
            NativeFunctionKind::FunctionPrototypeApply,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Apply),
            2,
        ),
        (
            NativeFunctionKind::FunctionPrototypeBind,
            IntrinsicNameSpec::RealmName(RealmNameId::Bind),
            1,
        ),
        (
            NativeFunctionKind::FunctionPrototypeHasInstance,
            IntrinsicNameSpec::Literal("[Symbol.hasInstance]"),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}

fn visit_object_functions(visit: FunctionSink<'_>) {
    for method in OBJECT_STATIC_METHODS {
        let name = method.predefined_name.map_or_else(
            || {
                IntrinsicNameSpec::RealmName(
                    method
                        .realm_name
                        .unwrap_or(RealmNameId::ObjectStatic(method.kind)),
                )
            },
            IntrinsicNameSpec::Predefined,
        );
        visit(ordinary(method.kind, name, method.length));
    }
    for (kind, name, length) in [
        (
            NativeFunctionKind::ObjectPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::ObjectPrototypeValueOf,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ),
        (
            NativeFunctionKind::ObjectPrototypeProtoGetter,
            IntrinsicNameSpec::Literal("get __proto__"),
            0,
        ),
        (
            NativeFunctionKind::ObjectPrototypeProtoSetter,
            IntrinsicNameSpec::Literal("set __proto__"),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for (_, kind, length) in OBJECT_PROTOTYPE_REFLECTION {
        visit(ordinary(
            kind,
            IntrinsicNameSpec::RealmName(RealmNameId::ObjectPrototypeMethod(kind)),
            length,
        ));
    }
    for (_, kind, length) in OBJECT_PROTOTYPE_LEGACY_ACCESSORS {
        visit(ordinary(
            kind,
            IntrinsicNameSpec::RealmName(RealmNameId::ObjectPrototypeMethod(kind)),
            length,
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    visit_object_prototype_properties(visit);
    visit_function_prototype_properties(visit);
    visit_constructor_properties(visit);
    visit_global_properties(visit);
}

fn visit_object_prototype_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::ObjectPrototype);
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString),
        NativeFunctionKind::ObjectPrototypeToString,
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToLocaleString),
        NativeFunctionKind::LocaleString(LocaleStringMethod::Object),
    ));
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::ValueOf),
        NativeFunctionKind::ObjectPrototypeValueOf,
    ));
    for (_, function, _) in OBJECT_PROTOTYPE_REFLECTION {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ObjectPrototypeMethod(function)),
            function,
        ));
    }
    visit(accessor(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Proto),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::ObjectPrototypeProtoGetter,
        )),
        Some(IntrinsicFunctionId(
            NativeFunctionKind::ObjectPrototypeProtoSetter,
        )),
    ));
    for (_, function, _) in OBJECT_PROTOTYPE_LEGACY_ACCESSORS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ObjectPrototypeMethod(function)),
            function,
        ));
    }
    visit(method(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
        NativeFunctionKind::ObjectConstructor,
    ));
}

fn visit_function_prototype_properties(visit: PropertySink<'_>) {
    let throw_type_error = IntrinsicFunctionId(NativeFunctionKind::ThrowTypeError);
    let throw = IntrinsicIdentity::Function(throw_type_error);
    for key in [PredefinedAtom::Length, PredefinedAtom::Name] {
        let value = if key == PredefinedAtom::Length {
            IntrinsicValueSpec::NumberBits(0_f64.to_bits())
        } else {
            IntrinsicValueSpec::String(IntrinsicStringSpec::Predefined(PredefinedAtom::EmptyString))
        };
        visit(data(
            throw,
            IntrinsicKeySpec::PredefinedString(key),
            FROZEN_PROPERTY,
            value,
        ));
    }

    let prototype =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype));
    for key in [PredefinedAtom::Caller, PredefinedAtom::ArgumentsIdentifier] {
        visit(accessor(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            PropertyLayout::accessor(false, true),
            Some(throw_type_error),
            Some(throw_type_error),
        ));
    }
    for (key, function) in [
        (
            IntrinsicKeySpec::InternedString(RealmNameId::Call),
            NativeFunctionKind::FunctionPrototypeCall,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Apply),
            NativeFunctionKind::FunctionPrototypeApply,
        ),
        (
            IntrinsicKeySpec::InternedString(RealmNameId::Bind),
            NativeFunctionKind::FunctionPrototypeBind,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::ToString),
            NativeFunctionKind::FunctionPrototypeToString,
        ),
        (
            IntrinsicKeySpec::PredefinedString(PredefinedAtom::Constructor),
            NativeFunctionKind::OrdinaryFunctionConstructor,
        ),
    ] {
        visit(method(prototype, key, function));
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolHasInstance),
        FROZEN_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(
            NativeFunctionKind::FunctionPrototypeHasInstance,
        )),
    ));
}

fn visit_constructor_properties(visit: PropertySink<'_>) {
    let function_constructor = IntrinsicIdentity::Function(IntrinsicFunctionId(
        NativeFunctionKind::OrdinaryFunctionConstructor,
    ));
    visit(data(
        function_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype)),
    ));

    let object_constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::ObjectConstructor));
    for method_spec in OBJECT_STATIC_METHODS {
        let key = if let Some(atom) = method_spec.predefined_name {
            IntrinsicKeySpec::PredefinedString(atom)
        } else if let Some(id) = method_spec.realm_name {
            IntrinsicKeySpec::InternedString(id)
        } else {
            IntrinsicKeySpec::InternedString(RealmNameId::ObjectStatic(method_spec.kind))
        };
        visit(method(object_constructor, key, method_spec.kind));
    }
    visit(data(
        object_constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::ObjectPrototype),
    ));
}

fn visit_global_properties(visit: PropertySink<'_>) {
    let global = IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject);
    for (key, value) in [
        (PredefinedAtom::Undefined, IntrinsicValueSpec::Undefined),
        (
            PredefinedAtom::Nan,
            IntrinsicValueSpec::NumberBits(f64::NAN.to_bits()),
        ),
        (
            PredefinedAtom::Infinity,
            IntrinsicValueSpec::NumberBits(f64::INFINITY.to_bits()),
        ),
    ] {
        visit(data(
            global,
            IntrinsicKeySpec::PredefinedString(key),
            FROZEN_PROPERTY,
            value,
        ));
    }
    visit(data(
        global,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::GlobalThis),
        METHOD_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::GlobalObject),
    ));
    for (key, function) in [
        (
            PredefinedAtom::Function,
            NativeFunctionKind::OrdinaryFunctionConstructor,
        ),
        (
            PredefinedAtom::Object,
            NativeFunctionKind::ObjectConstructor,
        ),
    ] {
        visit(method(
            global,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
}
