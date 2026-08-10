//! Array constructor, prototype, iterator-facing, and method declarations.

use super::{
    FunctionSink, ObjectSink, PropertySink, accessor, data, method, object, object_prototype,
    ordinary,
};
use crate::runtime::realm::{
    ARRAY_CALLBACK_METHODS, ARRAY_COPIER_METHODS, ARRAY_FLATTEN_METHODS, ARRAY_LENGTH_PROPERTY,
    ARRAY_MUTATOR_METHODS, ARRAY_PREDEFINED_COPIERS, ARRAY_REDUCTION_METHODS, ARRAY_SEARCH_METHODS,
    ARRAY_SORT_METHODS, ArrayCallback, ArrayCopier, ArrayFlatten, ArrayMutator, ArraySearch,
    ArraySort, ArrayStatic, CONSTRUCTOR_PROTOTYPE_PROPERTY, LOCALE_STRING_METHODS,
    NUMBER_FORMAT_METHODS, NativeFunctionKind, PredefinedAtom, PropertyLayout,
    schema::{
        IntrinsicFunctionId, IntrinsicIdentity, IntrinsicKeySpec, IntrinsicNameSpec,
        IntrinsicObjectId, IntrinsicObjectKind, IntrinsicValueSpec, PrototypeSpec, RealmNameId,
    },
};

pub(super) fn visit_objects(visit: ObjectSink<'_>) {
    visit(object(
        IntrinsicObjectId::ArrayPrototype,
        object_prototype(),
        IntrinsicObjectKind::ArrayPrototype,
    ));
    visit(object(
        IntrinsicObjectId::ArrayUnscopables,
        PrototypeSpec::Null,
        IntrinsicObjectKind::Ordinary,
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
        let name = match method.predefined_atom() {
            Some(name) => IntrinsicNameSpec::Predefined(name),
            None => IntrinsicNameSpec::RealmName(RealmNameId::ArrayFromAsync),
        };
        visit(ordinary(
            NativeFunctionKind::ArrayStatic(method),
            name,
            method.length(),
        ));
    }
}

pub(super) fn visit_properties(visit: PropertySink<'_>) {
    let prototype = IntrinsicIdentity::Object(IntrinsicObjectId::ArrayPrototype);
    visit(data(
        prototype,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Length),
        ARRAY_LENGTH_PROPERTY,
        IntrinsicValueSpec::NumberBits(0_f64.to_bits()),
    ));

    let constructor =
        IntrinsicIdentity::Function(IntrinsicFunctionId(NativeFunctionKind::ArrayConstructor));
    visit(method(
        constructor,
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayIsArray),
        NativeFunctionKind::ArrayIsArray,
    ));
    for method_id in ArrayStatic::ALL {
        let key = match method_id.predefined_atom() {
            Some(name) => IntrinsicKeySpec::PredefinedString(name),
            None => IntrinsicKeySpec::InternedString(RealmNameId::ArrayFromAsync),
        };
        visit(method(
            constructor,
            key,
            NativeFunctionKind::ArrayStatic(method_id),
        ));
    }

    visit_prototype_methods(prototype, visit);
    for (key, function) in [
        (
            PredefinedAtom::Constructor,
            NativeFunctionKind::ArrayConstructor,
        ),
        (PredefinedAtom::Join, NativeFunctionKind::ArrayPrototypeJoin),
        (
            PredefinedAtom::ToString,
            NativeFunctionKind::ArrayPrototypeToString,
        ),
    ] {
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            function,
        ));
    }
    visit_unscopables(prototype, visit);
    visit(data(
        constructor,
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Prototype),
        CONSTRUCTOR_PROTOTYPE_PROPERTY,
        IntrinsicValueSpec::Object(IntrinsicObjectId::ArrayPrototype),
    ));
    visit(accessor(
        constructor,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolSpecies),
        PropertyLayout::accessor(false, true),
        Some(IntrinsicFunctionId(NativeFunctionKind::ArraySpeciesGetter)),
        None,
    ));
    visit(method(
        IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Array),
        NativeFunctionKind::ArrayConstructor,
    ));
}

fn visit_unscopables(prototype: IntrinsicIdentity, visit: PropertySink<'_>) {
    let table = IntrinsicIdentity::Object(IntrinsicObjectId::ArrayUnscopables);
    let entry_layout = PropertyLayout::data(true, true, true);
    for key in [
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCopier(ArrayCopier::At)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayMutator(ArrayMutator::CopyWithin)),
        IntrinsicKeySpec::InternedString(RealmNameId::Entries),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayMutator(ArrayMutator::Fill)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCallback(ArrayCallback::Find)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCallback(ArrayCallback::FindIndex)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCallback(ArrayCallback::FindLast)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCallback(ArrayCallback::FindLastIndex)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayFlatten(ArrayFlatten::Flat)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayFlatten(ArrayFlatten::FlatMap)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArraySearch(ArraySearch::Includes)),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Keys),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCopier(ArrayCopier::ToReversed)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArraySort(ArraySort::ToSorted)),
        IntrinsicKeySpec::InternedString(RealmNameId::ArrayCopier(ArrayCopier::ToSpliced)),
        IntrinsicKeySpec::PredefinedString(PredefinedAtom::Values),
    ] {
        visit(data(
            table,
            key,
            entry_layout,
            IntrinsicValueSpec::Boolean(true),
        ));
    }
    visit(data(
        prototype,
        IntrinsicKeySpec::WellKnownSymbol(PredefinedAtom::SymbolUnscopables),
        PropertyLayout::data(false, false, true),
        IntrinsicValueSpec::Object(IntrinsicObjectId::ArrayUnscopables),
    ));
}

fn visit_prototype_methods(prototype: IntrinsicIdentity, visit: PropertySink<'_>) {
    for (_, search) in ARRAY_SEARCH_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArraySearch(search)),
            NativeFunctionKind::ArrayPrototypeSearch(search),
        ));
    }
    for method_id in ARRAY_MUTATOR_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArrayMutator(method_id)),
            NativeFunctionKind::ArrayPrototypeMutator(method_id),
        ));
    }
    for method_id in ARRAY_COPIER_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArrayCopier(method_id)),
            NativeFunctionKind::ArrayPrototypeCopier(method_id),
        ));
    }
    for (key, method_id) in ARRAY_PREDEFINED_COPIERS {
        visit(method(
            prototype,
            IntrinsicKeySpec::PredefinedString(key),
            NativeFunctionKind::ArrayPrototypeCopier(method_id),
        ));
    }
    for method_id in ARRAY_SORT_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArraySort(method_id)),
            NativeFunctionKind::ArrayPrototypeSort(method_id),
        ));
    }
    for method_id in ARRAY_FLATTEN_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArrayFlatten(method_id)),
            NativeFunctionKind::ArrayPrototypeFlatten(method_id),
        ));
    }
    for method_id in ARRAY_CALLBACK_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArrayCallback(method_id)),
            NativeFunctionKind::ArrayPrototypeCallback(method_id),
        ));
    }
    for method_id in ARRAY_REDUCTION_METHODS {
        visit(method(
            prototype,
            IntrinsicKeySpec::InternedString(RealmNameId::ArrayReduction(method_id)),
            NativeFunctionKind::ArrayPrototypeReduction(method_id),
        ));
    }
    visit(method(
        prototype,
        IntrinsicKeySpec::InternedString(RealmNameId::ArraySplice),
        NativeFunctionKind::ArrayPrototypeSplice,
    ));
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
