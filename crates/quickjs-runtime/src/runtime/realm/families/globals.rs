//! Number predicate, numeric global, and URI function declarations.

use super::{FunctionSink, PropertySink, method, ordinary};
use crate::runtime::realm::{
    GLOBAL_NUMERIC_FUNCTIONS, GlobalNumericFunction, NUMBER_PREDICATE_STATICS, NativeFunctionKind,
    NumberPredicate, PredefinedAtom, URI_FUNCTIONS,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, RealmNameId,
    },
};

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    visit(ordinary(
        NativeFunctionKind::Eval,
        IntrinsicNameSpec::Predefined(PredefinedAtom::Eval),
        1,
    ));
    for (_, predicate) in NUMBER_PREDICATE_STATICS {
        visit(ordinary(
            NativeFunctionKind::NumberPredicateStatic(predicate),
            IntrinsicNameSpec::RealmName(RealmNameId::NumberPredicate(predicate)),
            1,
        ));
    }
    for (kind, length) in GLOBAL_NUMERIC_FUNCTIONS {
        let name = numeric_name(kind);
        visit(ordinary(
            NativeFunctionKind::GlobalNumeric(kind),
            IntrinsicNameSpec::RealmName(name),
            length,
        ));
    }
    for (_, kind) in URI_FUNCTIONS {
        visit(ordinary(
            NativeFunctionKind::GlobalUri(kind),
            IntrinsicNameSpec::RealmName(RealmNameId::Uri(kind)),
            1,
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Eval),
        NativeFunctionKind::Eval,
    ));
    for (kind, _) in GLOBAL_NUMERIC_FUNCTIONS {
        let name = numeric_name(kind);
        let function = NativeFunctionKind::GlobalNumeric(kind);
        visit(method(
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            IntrinsicKeySpec::InternedString(name),
            function,
        ));
        if matches!(
            kind,
            GlobalNumericFunction::ParseFloat | GlobalNumericFunction::ParseInt
        ) {
            visit(method(
                IntrinsicIdentity::Function(IntrinsicFunctionId(
                    NativeFunctionKind::NumberConstructor,
                )),
                IntrinsicKeySpec::InternedString(name),
                function,
            ));
        }
    }
    for (_, kind) in URI_FUNCTIONS {
        visit(method(
            IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
            IntrinsicKeySpec::InternedString(RealmNameId::Uri(kind)),
            NativeFunctionKind::GlobalUri(kind),
        ));
    }
}

const fn numeric_name(kind: GlobalNumericFunction) -> RealmNameId {
    match kind {
        GlobalNumericFunction::IsFinite => RealmNameId::NumberPredicate(NumberPredicate::IsFinite),
        GlobalNumericFunction::IsNaN => RealmNameId::NumberPredicate(NumberPredicate::IsNaN),
        GlobalNumericFunction::ParseFloat => RealmNameId::ParseFloat,
        GlobalNumericFunction::ParseInt => RealmNameId::ParseInt,
    }
}
