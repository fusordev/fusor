//! String method and factory declarations.

use super::{FunctionSink, ordinary};
use crate::runtime::realm::{
    NativeFunctionKind, PredefinedAtom, STRING_FROM_STATICS, STRING_PROTOTYPE_METHODS,
    schema::{IntrinsicNameSpec, RealmNameId},
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
