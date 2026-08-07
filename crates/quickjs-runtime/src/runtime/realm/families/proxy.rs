//! `%Proxy%` constructor declarations.

use super::{FunctionSink, ObjectSink, PropertySink, method, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, PredefinedAtom,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(_: ObjectSink<'_>) {}

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::ProxyConstructor,
        IntrinsicNameSpec::Predefined(PredefinedAtom::Proxy),
        2,
    ));
    visit(ordinary(
        NativeFunctionKind::ProxyRevocable,
        IntrinsicNameSpec::RealmName(RealmNameId::ProxyRevocable),
        2,
    ));
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::ProxyConstructor));
    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::ProxyRevocable),
        NativeFunctionKind::ProxyRevocable,
    ));
    visit(method(
        IntrinsicIdentity::Object(super::IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Proxy),
        NativeFunctionKind::ProxyConstructor,
    ));
}
