//! Specification-ordered intrinsic family declarations.

use super::schema::{
    FamilyCardinality, IntrinsicFunctionId, IntrinsicFunctionSpec, IntrinsicIdentity,
    IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, IntrinsicObjectSpec,
    IntrinsicSchema, PrototypeSpec, RealmNameId,
};
use super::validation::{SchemaValidationError, validate_intrinsic_schema};
use super::{
    ARRAY_CALLBACK_METHODS, ARRAY_COPIER_METHODS, ARRAY_FLATTEN_METHODS, ARRAY_MUTATOR_METHODS,
    ARRAY_PREDEFINED_COPIERS, ARRAY_REDUCTION_METHODS, ARRAY_SEARCH_METHODS, ARRAY_SORT_METHODS,
    ErrorIntrinsicKind, GLOBAL_NUMERIC_FUNCTIONS, LOCALE_STRING_METHODS, MathMethod,
    NUMBER_FORMAT_METHODS, NUMBER_PREDICATE_STATICS, NativeFunctionKind,
    OBJECT_PROTOTYPE_REFLECTION, OBJECT_STATIC_METHODS, PredefinedAtom, RuntimeError,
    RuntimeResource, STRING_FROM_STATICS, STRING_PROTOTYPE_METHODS, URI_FUNCTIONS,
    allocation_failed,
};

/// Owned complete function declaration table used before Runtime mutation.
pub(super) struct RealmFunctionSchema {
    objects: [IntrinsicObjectSpec; 23],
    specs: Vec<IntrinsicFunctionSpec>,
    mandatory_functions: Vec<IntrinsicFunctionId>,
}

impl RealmFunctionSchema {
    pub(super) fn try_new() -> Result<Self, RuntimeError> {
        let mut count = Some(0_usize);
        visit_function_specs(|_| {
            count = count.and_then(|value| value.checked_add(1));
        });
        let count =
            count.ok_or_else(|| allocation_failed(RuntimeResource::HeapFunctions, usize::MAX))?;
        let mut specs = Vec::new();
        specs
            .try_reserve_exact(count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, count))?;
        visit_function_specs(|spec| specs.push(spec));
        debug_assert_eq!(specs.len(), count);
        let mut mandatory_functions = Vec::new();
        mandatory_functions
            .try_reserve_exact(count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, count))?;
        mandatory_functions.extend(specs.iter().map(|spec| spec.id));
        Ok(Self {
            objects: object_specs(),
            specs,
            mandatory_functions,
        })
    }

    pub(super) fn specs(&self) -> &[IntrinsicFunctionSpec] {
        &self.specs
    }

    pub(super) fn spec(&self, id: IntrinsicFunctionId) -> &IntrinsicFunctionSpec {
        self.specs
            .iter()
            .find(|spec| spec.id == id)
            .expect("the complete intrinsic function schema contains every allocated ID")
    }

    pub(super) const fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(super) fn function_count(&self) -> usize {
        self.specs.len()
    }

    pub(super) fn validate(&self) -> Result<(), SchemaValidationError> {
        let cardinalities = [FamilyCardinality {
            family: "Realm native functions",
            actual: self.specs.len(),
            expected: 219,
        }];
        validate_intrinsic_schema(IntrinsicSchema {
            objects: &self.objects,
            functions: &self.specs,
            properties: &[],
            mandatory_objects: &IntrinsicObjectId::ALL,
            mandatory_functions: &self.mandatory_functions,
            constructor_prototypes: &[],
            family_cardinalities: &cardinalities,
        })
        .map(|_| ())
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the complete stable object identity table remains visible until family files own their entries"
)]
pub(super) const fn object_specs() -> [IntrinsicObjectSpec; 23] {
    let ordinary = IntrinsicObjectKind::Ordinary;
    let object_prototype = PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
        IntrinsicObjectId::ObjectPrototype,
    ));
    let iterator_prototype = PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
        IntrinsicObjectId::IteratorPrototype,
    ));
    [
        object(
            IntrinsicObjectId::ObjectPrototype,
            PrototypeSpec::Null,
            ordinary,
        ),
        object(IntrinsicObjectId::GlobalObject, object_prototype, ordinary),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::Error),
            object_prototype,
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::EvalError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::RangeError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::ReferenceError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::SyntaxError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::TypeError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::UriError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::InternalError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::AggregateError),
            error_prototype(),
            ordinary,
        ),
        object(
            IntrinsicObjectId::BooleanPrototype,
            object_prototype,
            IntrinsicObjectKind::BooleanPrototype,
        ),
        object(
            IntrinsicObjectId::NumberPrototype,
            object_prototype,
            IntrinsicObjectKind::NumberPrototype,
        ),
        object(
            IntrinsicObjectId::BigIntPrototype,
            object_prototype,
            IntrinsicObjectKind::BigIntPrototype,
        ),
        object(
            IntrinsicObjectId::StringPrototype,
            object_prototype,
            IntrinsicObjectKind::StringPrototype,
        ),
        object(
            IntrinsicObjectId::ArrayPrototype,
            object_prototype,
            IntrinsicObjectKind::ArrayPrototype,
        ),
        object(
            IntrinsicObjectId::IteratorPrototype,
            object_prototype,
            ordinary,
        ),
        object(
            IntrinsicObjectId::ArrayIteratorPrototype,
            iterator_prototype,
            ordinary,
        ),
        object(
            IntrinsicObjectId::StringIteratorPrototype,
            iterator_prototype,
            ordinary,
        ),
        object(
            IntrinsicObjectId::SymbolPrototype,
            object_prototype,
            ordinary,
        ),
        object(IntrinsicObjectId::Reflect, object_prototype, ordinary),
        object(IntrinsicObjectId::Json, object_prototype, ordinary),
        object(IntrinsicObjectId::Math, object_prototype, ordinary),
    ]
}

const fn object(
    id: IntrinsicObjectId,
    prototype: PrototypeSpec,
    kind: IntrinsicObjectKind,
) -> IntrinsicObjectSpec {
    IntrinsicObjectSpec {
        id,
        prototype,
        kind,
    }
}

const fn error_prototype() -> PrototypeSpec {
    PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
        IntrinsicObjectId::ErrorPrototype(ErrorIntrinsicKind::Error),
    ))
}

#[expect(
    clippy::too_many_lines,
    reason = "the complete function declaration is kept ordered while its family slices migrate into focused modules"
)]
fn visit_function_specs(mut visit: impl FnMut(IntrinsicFunctionSpec)) {
    let function_prototype = IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype);
    visit(function(
        NativeFunctionKind::FunctionPrototype,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
            IntrinsicObjectId::ObjectPrototype,
        )),
        IntrinsicNameSpec::Predefined(PredefinedAtom::EmptyString),
        0,
    ));
    for (kind, name, length) in [
        (
            NativeFunctionKind::ThrowTypeError,
            IntrinsicNameSpec::Predefined(PredefinedAtom::EmptyString),
            0,
        ),
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
        visit(ordinary(kind, name, length, function_prototype));
    }
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
        visit(ordinary(
            method.kind,
            name,
            method.length,
            function_prototype,
        ));
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
    ] {
        visit(ordinary(kind, name, length, function_prototype));
    }
    for (_, kind, length) in OBJECT_PROTOTYPE_REFLECTION {
        visit(ordinary(
            kind,
            IntrinsicNameSpec::RealmName(RealmNameId::ObjectPrototypeMethod(kind)),
            length,
            function_prototype,
        ));
    }
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
        visit(ordinary(kind, name, length, function_prototype));
    }

    let error_constructor = IntrinsicFunctionId(NativeFunctionKind::ErrorConstructor(
        ErrorIntrinsicKind::Error,
    ));
    for kind in ErrorIntrinsicKind::ALL {
        let prototype = if kind == ErrorIntrinsicKind::Error {
            function_prototype
        } else {
            error_constructor
        };
        visit(ordinary(
            NativeFunctionKind::ErrorConstructor(kind),
            IntrinsicNameSpec::Predefined(kind.predefined_atom()),
            i32::from(kind == ErrorIntrinsicKind::AggregateError) + 1,
            prototype,
        ));
    }
    for (kind, name, length) in [
        (
            NativeFunctionKind::ErrorPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::ErrorIsError,
            IntrinsicNameSpec::RealmName(RealmNameId::IsError),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length, function_prototype));
    }

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
        visit(ordinary(
            constructor,
            IntrinsicNameSpec::Predefined(name),
            1,
            function_prototype,
        ));
        visit(ordinary(
            to_string,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            to_string_length,
            function_prototype,
        ));
        visit(ordinary(
            value_of,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
            function_prototype,
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
        visit(ordinary(kind, name, length, function_prototype));
    }

    for method in STRING_PROTOTYPE_METHODS {
        let name = method.predefined_name.map_or(
            IntrinsicNameSpec::RealmName(RealmNameId::StringMethod(method.method)),
            IntrinsicNameSpec::Predefined,
        );
        visit(ordinary(
            NativeFunctionKind::StringPrototypeMethod(method.method),
            name,
            method.length,
            function_prototype,
        ));
    }
    for (_, method) in STRING_FROM_STATICS {
        visit(ordinary(
            NativeFunctionKind::StringPrototypeMethod(method),
            IntrinsicNameSpec::RealmName(RealmNameId::StringStatic(method)),
            1,
            function_prototype,
        ));
    }
    visit(ordinary(
        NativeFunctionKind::StringRaw,
        IntrinsicNameSpec::Predefined(PredefinedAtom::Raw),
        1,
        function_prototype,
    ));

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
        visit(ordinary(kind, name, length, function_prototype));
    }

    for (kind, name) in [
        (
            NativeFunctionKind::IteratorPrototypeIterator,
            IntrinsicNameSpec::Literal("[Symbol.iterator]"),
        ),
        (
            NativeFunctionKind::ArrayIteratorNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
        ),
        (
            NativeFunctionKind::ArrayPrototypeValues,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Values),
        ),
        (
            NativeFunctionKind::ArrayPrototypeKeys,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Keys),
        ),
        (
            NativeFunctionKind::ArrayPrototypeEntries,
            IntrinsicNameSpec::RealmName(RealmNameId::Entries),
        ),
        (
            NativeFunctionKind::StringIteratorNext,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Next),
        ),
        (
            NativeFunctionKind::StringPrototypeIterator,
            IntrinsicNameSpec::Literal("[Symbol.iterator]"),
        ),
    ] {
        visit(ordinary(kind, name, 0, function_prototype));
    }

    for (kind, name, length) in [
        (
            NativeFunctionKind::SymbolConstructor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::Symbol),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeToString,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToString),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeValueOf,
            IntrinsicNameSpec::Predefined(PredefinedAtom::ValueOf),
            0,
        ),
        (
            NativeFunctionKind::SymbolPrototypeToPrimitive,
            IntrinsicNameSpec::Literal("[Symbol.toPrimitive]"),
            1,
        ),
        (
            NativeFunctionKind::SymbolPrototypeDescription,
            IntrinsicNameSpec::Literal("get description"),
            0,
        ),
        (
            NativeFunctionKind::SymbolFor,
            IntrinsicNameSpec::Predefined(PredefinedAtom::For),
            1,
        ),
        (
            NativeFunctionKind::SymbolKeyFor,
            IntrinsicNameSpec::RealmName(RealmNameId::KeyFor),
            1,
        ),
    ] {
        visit(ordinary(kind, name, length, function_prototype));
    }

    for method in super::ReflectMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Reflect(method),
            IntrinsicNameSpec::Predefined(method.predefined_atom()),
            method.length(),
            function_prototype,
        ));
    }
    for (kind, name, length) in [
        (
            NativeFunctionKind::JsonIsRawJson,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonIsRawJson),
            1,
        ),
        (
            NativeFunctionKind::JsonParse,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonParse),
            2,
        ),
        (
            NativeFunctionKind::JsonRawJson,
            IntrinsicNameSpec::Predefined(PredefinedAtom::RawJson),
            1,
        ),
        (
            NativeFunctionKind::JsonStringify,
            IntrinsicNameSpec::RealmName(RealmNameId::JsonStringify),
            3,
        ),
    ] {
        visit(ordinary(kind, name, length, function_prototype));
    }
    for method in MathMethod::ALL {
        visit(ordinary(
            NativeFunctionKind::Math(method),
            IntrinsicNameSpec::RealmName(RealmNameId::MathMethod(method)),
            method.length(),
            function_prototype,
        ));
    }

    for (name, predicate) in NUMBER_PREDICATE_STATICS {
        let _ = name;
        visit(ordinary(
            NativeFunctionKind::NumberPredicateStatic(predicate),
            IntrinsicNameSpec::RealmName(RealmNameId::NumberPredicate(predicate)),
            1,
            function_prototype,
        ));
    }
    for (kind, length) in GLOBAL_NUMERIC_FUNCTIONS {
        let name = match kind {
            super::GlobalNumericFunction::IsFinite => {
                RealmNameId::NumberPredicate(super::NumberPredicate::IsFinite)
            }
            super::GlobalNumericFunction::IsNaN => {
                RealmNameId::NumberPredicate(super::NumberPredicate::IsNaN)
            }
            super::GlobalNumericFunction::ParseFloat => RealmNameId::ParseFloat,
            super::GlobalNumericFunction::ParseInt => RealmNameId::ParseInt,
        };
        visit(ordinary(
            NativeFunctionKind::GlobalNumeric(kind),
            IntrinsicNameSpec::RealmName(name),
            length,
            function_prototype,
        ));
    }
    for (_, kind) in URI_FUNCTIONS {
        visit(ordinary(
            NativeFunctionKind::GlobalUri(kind),
            IntrinsicNameSpec::RealmName(RealmNameId::Uri(kind)),
            1,
            function_prototype,
        ));
    }
    for (_, search) in ARRAY_SEARCH_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeSearch(search),
            IntrinsicNameSpec::RealmName(RealmNameId::ArraySearch(search)),
            1,
            function_prototype,
        ));
    }
    for method in ARRAY_MUTATOR_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeMutator(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayMutator(method)),
            method.arity(),
            function_prototype,
        ));
    }
    for method in ARRAY_COPIER_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeCopier(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayCopier(method)),
            method.arity(),
            function_prototype,
        ));
    }
    for (atom, method) in ARRAY_PREDEFINED_COPIERS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeCopier(method),
            IntrinsicNameSpec::Predefined(atom),
            method.arity(),
            function_prototype,
        ));
    }
    for method in ARRAY_SORT_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeSort(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArraySort(method)),
            1,
            function_prototype,
        ));
    }
    for method in ARRAY_FLATTEN_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeFlatten(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayFlatten(method)),
            method.arity(),
            function_prototype,
        ));
    }
    for method in LOCALE_STRING_METHODS {
        visit(ordinary(
            NativeFunctionKind::LocaleString(method),
            IntrinsicNameSpec::Predefined(PredefinedAtom::ToLocaleString),
            0,
            function_prototype,
        ));
    }
    for method in NUMBER_FORMAT_METHODS {
        visit(ordinary(
            NativeFunctionKind::NumberPrototypeFormat(method),
            IntrinsicNameSpec::RealmName(RealmNameId::NumberFormat(method)),
            1,
            function_prototype,
        ));
    }
    for method in ARRAY_CALLBACK_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeCallback(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayCallback(method)),
            1,
            function_prototype,
        ));
    }
    for method in ARRAY_REDUCTION_METHODS {
        visit(ordinary(
            NativeFunctionKind::ArrayPrototypeReduction(method),
            IntrinsicNameSpec::RealmName(RealmNameId::ArrayReduction(method)),
            1,
            function_prototype,
        ));
    }
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
        visit(ordinary(kind, name, length, function_prototype));
    }
    for method in super::ArrayStatic::ALL {
        visit(ordinary(
            NativeFunctionKind::ArrayStatic(method),
            IntrinsicNameSpec::Predefined(method.predefined_atom()),
            method.length(),
            function_prototype,
        ));
    }
}

const fn ordinary(
    implementation: NativeFunctionKind,
    name: IntrinsicNameSpec,
    length: i32,
    function_prototype: IntrinsicFunctionId,
) -> IntrinsicFunctionSpec {
    function(
        implementation,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype)),
        name,
        length,
    )
}

const fn function(
    implementation: NativeFunctionKind,
    prototype: PrototypeSpec,
    name: IntrinsicNameSpec,
    length: i32,
) -> IntrinsicFunctionSpec {
    IntrinsicFunctionSpec {
        id: IntrinsicFunctionId(implementation),
        implementation,
        prototype,
        name,
        length,
        constructable: implementation.is_constructor(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_function_schema_has_characterized_cardinality_and_unique_ids() {
        let schema = RealmFunctionSchema::try_new().expect("function schema");
        assert_eq!(schema.specs().len(), 219);
        for (index, spec) in schema.specs().iter().enumerate() {
            assert!(
                schema.specs()[..index]
                    .iter()
                    .all(|candidate| candidate.id != spec.id)
            );
            assert_eq!(spec.constructable, spec.implementation.is_constructor());
        }
    }

    #[test]
    fn complete_object_schema_has_every_stable_identity_once() {
        let objects = object_specs();
        assert_eq!(objects.len(), IntrinsicObjectId::ALL.len());
        for id in IntrinsicObjectId::ALL {
            assert_eq!(objects.iter().filter(|spec| spec.id == id).count(), 1);
        }
    }
}
