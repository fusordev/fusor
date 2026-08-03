//! Array constructor, prototype, iterator-facing, and method declarations.

use super::{FunctionSink, ObjectSink, object, object_prototype, ordinary};
use crate::runtime::realm::{
    ARRAY_CALLBACK_METHODS, ARRAY_COPIER_METHODS, ARRAY_FLATTEN_METHODS, ARRAY_MUTATOR_METHODS,
    ARRAY_PREDEFINED_COPIERS, ARRAY_REDUCTION_METHODS, ARRAY_SEARCH_METHODS, ARRAY_SORT_METHODS,
    ArrayStatic, LOCALE_STRING_METHODS, NUMBER_FORMAT_METHODS, NativeFunctionKind, PredefinedAtom,
    schema::{IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, RealmNameId},
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::ArrayPrototype,
        object_prototype(),
        IntrinsicObjectKind::ArrayPrototype,
    ));
}

pub(super) fn visit_kernel_functions(visit: FunctionSink<'_>) {
    for (kind, name, length) in [
        (
            NativeFunctionKind::ArrayConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Array),
            1,
        ),
        (
            NativeFunctionKind::ArraySpeciesGetter,
            IntrinsicNameSpec::Literal("get [Symbol.species]"),
            0,
        ),
        (
            NativeFunctionKind::ArrayPrototypeJoin,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Join),
            1,
        ),
        (
            NativeFunctionKind::ArrayPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
}

pub(super) fn visit_method_functions(visit: FunctionSink<'_>) {
    visit_search_and_mutation_functions(visit);
    visit_copy_and_order_functions(visit);
    visit_callback_and_format_functions(visit);
    for (kind, name, length) in [
        (
            NativeFunctionKind::ArrayPrototypeSplice,
            IntrinsicNameSpec::RealmName(RealmNameId::ArraySplice),
            2,
        ),
        (
            NativeFunctionKind::ArrayIsArray,
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayIsArray),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length));
    }
    for method in ArrayStatic::ALL {
        visit(ordinary(
            NativeFunctionKind::ArrayStatic(method),
            IntrinsicNameSpec::Predefined(method.predefined_atom()),
            method.length(),
        ));
    }
}

fn visit_search_and_mutation_functions(visit: FunctionSink<'_>) {
    for (_, search) in ARRAY_SEARCH_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeSearch(search),
            IntrinsicNameSpec::RealmName(RealmNameId::ArraySearch(search)),
            1,
        ));
    }
    for method in ARRAY_MUTATOR_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeMutator(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayMutator(method)),
            method.arity(),
        ));
    }
}

fn visit_copy_and_order_functions(visit: FunctionSink<'_>) {
    for method in ARRAY_COPIER_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeCopier(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayCopier(method)),
            method.arity(),
        ));
    }
    for (atom, method) in ARRAY_PREDEFINED_COPIERS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeCopier(method),
            IntrinsicNameSpec::Predefined(atom),
            method.arity(),
        ));
    }
    for method in ARRAY_SORT_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeSort(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArraySort(method)),
            1,
        ));
    }
    for method in ARRAY_FLATTEN_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeFlatten(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayFlatten(method)),
            method.arity(),
        ));
    }
}

fn visit_callback_and_format_functions(visit: FunctionSink<'_>) {
    for method in LOCALE_STRING_METHODS {
        visit(ordinary(
            NativeFunctionKind::LocaleString(method),
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString),
            0,
        ));
    }
    for method in NUMBER_FORMAT_METHODS {
        visit(ordinary(
            NativeFunctionKind::NumberPrototypeFormat(method),
            IntrinsicNameSpec::RealmName(RealmNameId::NumberFormat(method)),
            1,
        ));
    }
    for method in ARRAY_CALLBACK_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeCallback(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayCallback(method)),
            1,
        ));
    }
    for method in ARRAY_REDUCTION_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeReduction(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayReduction(method)),
            1,
        ));
    }
}
