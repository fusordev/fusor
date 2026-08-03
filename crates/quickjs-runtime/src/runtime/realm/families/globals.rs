//! Number predicate, numeric global, and URI function declarations.

use super::{FunctionSink, ordinary};
use crate::runtime::realm::{
    GLOBAL_NUMERIC_FUNCTIONS, GlobalNumericFunction, NUMBER_PREDICATE_STATICS, NativeFunctionKind,
    NumberPredicate, URI_FUNCTIONS,
    schema::{IntrinsicNameSpec, RealmNameId},
};

pub(super) fn visit_functions(visit: FunctionSink<'_>) {
    for (_, predicate) in NUMBER_PREDICATE_STATICS {
        visit(ordinary(
            NativeFunctionKind::NumberPredicateStatic(predicate),
            IntrinsicNameSpec::RealmName(RealmNameId::NumberPredicate(predicate)),
            1,
        ));
    }
    for (kind, length) in GLOBAL_NUMERIC_FUNCTIONS {
        let name = match kind {
            GlobalNumericFunction::IsFinite => {
                RealmNameId::NumberPredicate(NumberPredicate::IsFinite)
            }
            GlobalNumericFunction::IsNaN => RealmNameId::NumberPredicate(NumberPredicate::IsNaN),
            GlobalNumericFunction::ParseFloat => RealmNameId::ParseFloat,
            GlobalNumericFunction::ParseInt => RealmNameId::ParseInt,
        };
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
