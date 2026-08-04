//! Specification-ordered intrinsic family declarations.

mod array;
mod async_function;
mod error;
mod generator;
mod globals;
mod iterator;
mod json;
mod kernel;
mod math;
mod primitives;
mod promise;
mod reflect;
mod string;
mod symbol;

use super::schema::{
    ConstructorPrototypeSpec, FamilyCardinality, IntrinsicDescriptorSpec, IntrinsicFunctionId,
    IntrinsicFunctionSpec, IntrinsicIdentity, IntrinsicIdentityPublication, IntrinsicKeySpec,
    IntrinsicNameSpec, IntrinsicObjectId, IntrinsicObjectKind, IntrinsicObjectSpec,
    IntrinsicPropertySpec, IntrinsicSchema, IntrinsicValueSpec, PrototypeSpec,
};
use super::validation::{SchemaValidationError, validate_intrinsic_schema};
use super::{NativeFunctionKind, RuntimeError, RuntimeResource, allocation_failed};

type ObjectSink<'a> = &'a mut dyn FnMut(IntrinsicObjectSpec);
type FunctionSink<'a> = &'a mut dyn FnMut(IntrinsicFunctionSpec);
type PropertySink<'a> = &'a mut dyn FnMut(IntrinsicPropertySpec);

/// Owned complete declaration table used before Runtime mutation.
pub(super) struct RealmIntrinsicSchema {
    objects: Vec<IntrinsicObjectSpec>,
    specs: Vec<IntrinsicFunctionSpec>,
    properties: Vec<IntrinsicPropertySpec>,
    mandatory_functions: Vec<IntrinsicFunctionId>,
    constructor_prototypes: Vec<ConstructorPrototypeSpec>,
}

impl RealmIntrinsicSchema {
    pub(super) fn try_new() -> Result<Self, RuntimeError> {
        let object_count = count_specs(visit_object_specs, RuntimeResource::HeapObjects)?;
        let function_count = count_specs(visit_function_specs, RuntimeResource::HeapFunctions)?;
        let property_count = count_specs(visit_property_specs, RuntimeResource::ObjectProperties)?;
        let mut objects = Vec::new();
        objects
            .try_reserve_exact(object_count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapObjects, object_count))?;
        visit_object_specs(&mut |spec| objects.push(spec));
        let mut specs = Vec::new();
        specs
            .try_reserve_exact(function_count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, function_count))?;
        visit_function_specs(&mut |spec| specs.push(spec));
        let mut properties = Vec::new();
        properties
            .try_reserve_exact(property_count)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, property_count))?;
        visit_property_specs(&mut |property| properties.push(property));
        let mut mandatory_functions = Vec::new();
        mandatory_functions
            .try_reserve_exact(function_count)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, function_count))?;
        mandatory_functions.extend(specs.iter().map(|spec| spec.id));
        let mut constructor_prototypes = Vec::new();
        constructor_prototypes
            .try_reserve_exact(specs.len())
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, specs.len()))?;
        for property in &properties {
            let IntrinsicIdentity::Function(constructor) = property.holder else {
                continue;
            };
            if property.key != IntrinsicKeySpec::PredefinedString(super::PredefinedAtom::Prototype)
            {
                continue;
            }
            let prototype = match property.descriptor {
                IntrinsicDescriptorSpec::Data {
                    value: IntrinsicValueSpec::Object(id),
                    ..
                } => IntrinsicIdentity::Object(id),
                IntrinsicDescriptorSpec::Data {
                    value: IntrinsicValueSpec::Function(id),
                    ..
                } => IntrinsicIdentity::Function(id),
                _ => continue,
            };
            constructor_prototypes.push(ConstructorPrototypeSpec {
                constructor,
                prototype,
            });
        }
        Ok(Self {
            objects,
            specs,
            properties,
            mandatory_functions,
            constructor_prototypes,
        })
    }

    pub(super) fn specs(&self) -> &[IntrinsicFunctionSpec] {
        &self.specs
    }

    pub(super) fn objects(&self) -> &[IntrinsicObjectSpec] {
        &self.objects
    }

    pub(super) fn properties(&self) -> &[IntrinsicPropertySpec] {
        &self.properties
    }

    pub(super) fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(super) fn function_count(&self) -> usize {
        self.specs.len()
    }

    pub(super) fn validate(&self) -> Result<(), SchemaValidationError> {
        let cardinalities = [
            FamilyCardinality {
                family: "Realm intrinsic objects",
                actual: self.objects.len(),
                expected: 27,
            },
            FamilyCardinality {
                family: "Realm native functions",
                actual: self.specs.len(),
                expected: 256,
            },
        ];
        validate_intrinsic_schema(IntrinsicSchema {
            objects: &self.objects,
            functions: &self.specs,
            properties: &self.properties,
            mandatory_objects: &IntrinsicObjectId::ALL,
            mandatory_functions: &self.mandatory_functions,
            constructor_prototypes: &self.constructor_prototypes,
            family_cardinalities: &cardinalities,
        })
    }
}

pub(super) const fn is_declarative_object(id: IntrinsicObjectId) -> bool {
    matches!(
        id,
        IntrinsicObjectId::ErrorPrototype(_)
            | IntrinsicObjectId::BooleanPrototype
            | IntrinsicObjectId::NumberPrototype
            | IntrinsicObjectId::BigIntPrototype
            | IntrinsicObjectId::StringPrototype
            | IntrinsicObjectId::ArrayPrototype
            | IntrinsicObjectId::IteratorPrototype
            | IntrinsicObjectId::ArrayIteratorPrototype
            | IntrinsicObjectId::StringIteratorPrototype
            | IntrinsicObjectId::GeneratorFunctionPrototype
            | IntrinsicObjectId::GeneratorPrototype
            | IntrinsicObjectId::AsyncFunctionPrototype
            | IntrinsicObjectId::SymbolPrototype
            | IntrinsicObjectId::PromisePrototype
            | IntrinsicObjectId::Reflect
            | IntrinsicObjectId::Json
            | IntrinsicObjectId::Math
    )
}

pub(super) const fn is_declarative_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::ErrorConstructor(_)
            | NativeFunctionKind::ErrorPrototypeToString
            | NativeFunctionKind::ErrorIsError
            | NativeFunctionKind::BooleanConstructor
            | NativeFunctionKind::BooleanPrototypeToString
            | NativeFunctionKind::BooleanPrototypeValueOf
            | NativeFunctionKind::NumberConstructor
            | NativeFunctionKind::NumberPrototypeToString
            | NativeFunctionKind::NumberPrototypeValueOf
            | NativeFunctionKind::NumberPredicateStatic(_)
            | NativeFunctionKind::NumberPrototypeFormat(_)
            | NativeFunctionKind::BigIntConstructor
            | NativeFunctionKind::BigIntPrototypeToString
            | NativeFunctionKind::BigIntPrototypeValueOf
            | NativeFunctionKind::BigIntAsIntN
            | NativeFunctionKind::BigIntAsUintN
            | NativeFunctionKind::StringConstructor
            | NativeFunctionKind::StringPrototypeToString
            | NativeFunctionKind::StringPrototypeValueOf
            | NativeFunctionKind::StringPrototypeMethod(_)
            | NativeFunctionKind::StringRaw
            | NativeFunctionKind::LocaleString(
                super::LocaleStringMethod::Number
                    | super::LocaleStringMethod::BigInt
                    | super::LocaleStringMethod::Array
            )
            | NativeFunctionKind::ArrayConstructor
            | NativeFunctionKind::ArraySpeciesGetter
            | NativeFunctionKind::ArrayPrototypeJoin
            | NativeFunctionKind::ArrayPrototypeToString
            | NativeFunctionKind::ArrayPrototypeSearch(_)
            | NativeFunctionKind::ArrayPrototypeMutator(_)
            | NativeFunctionKind::ArrayPrototypeCopier(_)
            | NativeFunctionKind::ArrayPrototypeSort(_)
            | NativeFunctionKind::ArrayPrototypeFlatten(_)
            | NativeFunctionKind::ArrayPrototypeCallback(_)
            | NativeFunctionKind::ArrayPrototypeReduction(_)
            | NativeFunctionKind::ArrayPrototypeSplice
            | NativeFunctionKind::ArrayIsArray
            | NativeFunctionKind::ArrayStatic(_)
            | NativeFunctionKind::IteratorPrototypeIterator
            | NativeFunctionKind::ArrayIteratorNext
            | NativeFunctionKind::ArrayPrototypeValues
            | NativeFunctionKind::ArrayPrototypeKeys
            | NativeFunctionKind::ArrayPrototypeEntries
            | NativeFunctionKind::StringIteratorNext
            | NativeFunctionKind::StringPrototypeIterator
            | NativeFunctionKind::GeneratorFunctionConstructor
            | NativeFunctionKind::AsyncFunctionConstructor
            | NativeFunctionKind::GeneratorPrototypeNext
            | NativeFunctionKind::GeneratorPrototypeReturn
            | NativeFunctionKind::GeneratorPrototypeThrow
            | NativeFunctionKind::SymbolConstructor
            | NativeFunctionKind::SymbolPrototypeToString
            | NativeFunctionKind::SymbolPrototypeValueOf
            | NativeFunctionKind::SymbolPrototypeToPrimitive
            | NativeFunctionKind::SymbolPrototypeDescription
            | NativeFunctionKind::SymbolFor
            | NativeFunctionKind::SymbolKeyFor
            | NativeFunctionKind::GlobalNumeric(_)
            | NativeFunctionKind::GlobalUri(_)
            | NativeFunctionKind::Reflect(_)
            | NativeFunctionKind::JsonIsRawJson
            | NativeFunctionKind::JsonParse
            | NativeFunctionKind::JsonRawJson
            | NativeFunctionKind::JsonStringify
            | NativeFunctionKind::Math(_)
            | NativeFunctionKind::PromiseConstructor
            | NativeFunctionKind::PromiseResolve
            | NativeFunctionKind::PromiseReject
            | NativeFunctionKind::PromiseStatic(_)
            | NativeFunctionKind::PromiseSpeciesGetter
            | NativeFunctionKind::PromisePrototypeThen
            | NativeFunctionKind::PromisePrototypeCatch
            | NativeFunctionKind::PromisePrototypeFinally
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeclarativeBatch {
    Kernel,
    GlobalValues,
    KernelGlobals,
    Errors,
    ErrorGlobals,
    Globals,
    Primitives,
    PrimitiveGlobals,
    Arrays,
    ArrayGlobals,
    ArrayExoticInitialization,
    Iterators,
    Symbols,
    SymbolGlobals,
    Promises,
    PromiseGlobals,
    NamespaceObjects,
}

pub(super) fn property_batch(property: IntrinsicPropertySpec) -> DeclarativeBatch {
    if is_global_namespace_property(property) {
        return DeclarativeBatch::NamespaceObjects;
    }
    let referenced_function = match property.descriptor {
        IntrinsicDescriptorSpec::Data {
            value: IntrinsicValueSpec::Function(id),
            ..
        } => Some(id),
        _ => None,
    };
    if let Some(batch) = special_reference_batch(property.holder, referenced_function) {
        return batch;
    }
    if is_global_value_property(property) {
        return DeclarativeBatch::GlobalValues;
    }
    if is_error_identity(property.holder)
        || matches!(
            referenced_function,
            Some(IntrinsicFunctionId(
                NativeFunctionKind::ErrorConstructor(_)
                    | NativeFunctionKind::ErrorPrototypeToString
                    | NativeFunctionKind::ErrorIsError
            ))
        )
    {
        return DeclarativeBatch::Errors;
    }
    let references_primitive = match referenced_function {
        Some(id) => is_primitive_function(id),
        None => false,
    };
    if is_primitive_identity(property.holder) || references_primitive {
        return DeclarativeBatch::Primitives;
    }
    if is_iterator_identity(property.holder)
        || referenced_function.is_some_and(is_iterator_function)
    {
        return DeclarativeBatch::Iterators;
    }
    if property.holder == IntrinsicIdentity::Object(IntrinsicObjectId::ArrayPrototype)
        && property.key == IntrinsicKeySpec::PredefinedString(super::PredefinedAtom::Length)
    {
        return DeclarativeBatch::ArrayExoticInitialization;
    }
    if is_array_identity(property.holder) || referenced_function.is_some_and(is_array_function) {
        return DeclarativeBatch::Arrays;
    }
    if is_symbol_identity(property.holder)
        || matches!(
            referenced_function,
            Some(IntrinsicFunctionId(
                NativeFunctionKind::SymbolConstructor
                    | NativeFunctionKind::SymbolPrototypeToString
                    | NativeFunctionKind::SymbolPrototypeValueOf
                    | NativeFunctionKind::SymbolPrototypeToPrimitive
                    | NativeFunctionKind::SymbolPrototypeDescription
                    | NativeFunctionKind::SymbolFor
                    | NativeFunctionKind::SymbolKeyFor
            ))
        )
    {
        return DeclarativeBatch::Symbols;
    }
    if is_promise_identity(property.holder) || referenced_function.is_some_and(is_promise_function)
    {
        return DeclarativeBatch::Promises;
    }
    if is_kernel_identity(property.holder) || referenced_function.is_some_and(is_kernel_function) {
        return DeclarativeBatch::Kernel;
    }
    DeclarativeBatch::NamespaceObjects
}

fn is_global_namespace_property(property: IntrinsicPropertySpec) -> bool {
    property.holder == IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject)
        && matches!(
            property.descriptor,
            IntrinsicDescriptorSpec::Data {
                value: IntrinsicValueSpec::Object(
                    IntrinsicObjectId::Reflect | IntrinsicObjectId::Json | IntrinsicObjectId::Math
                ),
                ..
            }
        )
}

fn is_global_value_property(property: IntrinsicPropertySpec) -> bool {
    property.holder == IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject)
        && matches!(
            property.key,
            IntrinsicKeySpec::PredefinedString(
                super::PredefinedAtom::Undefined
                    | super::PredefinedAtom::Nan
                    | super::PredefinedAtom::Infinity
                    | super::PredefinedAtom::GlobalThis
            )
        )
}

fn special_reference_batch(
    holder: IntrinsicIdentity,
    referenced_function: Option<IntrinsicFunctionId>,
) -> Option<DeclarativeBatch> {
    let IntrinsicFunctionId(kind) = referenced_function?;
    if matches!(
        kind,
        NativeFunctionKind::GlobalNumeric(_) | NativeFunctionKind::GlobalUri(_)
    ) {
        return Some(DeclarativeBatch::Globals);
    }
    if holder != IntrinsicIdentity::Object(IntrinsicObjectId::GlobalObject) {
        return None;
    }
    match kind {
        NativeFunctionKind::ErrorConstructor(_) => Some(DeclarativeBatch::ErrorGlobals),
        NativeFunctionKind::BooleanConstructor
        | NativeFunctionKind::NumberConstructor
        | NativeFunctionKind::BigIntConstructor
        | NativeFunctionKind::StringConstructor => Some(DeclarativeBatch::PrimitiveGlobals),
        NativeFunctionKind::ArrayConstructor => Some(DeclarativeBatch::ArrayGlobals),
        NativeFunctionKind::SymbolConstructor => Some(DeclarativeBatch::SymbolGlobals),
        NativeFunctionKind::PromiseConstructor => Some(DeclarativeBatch::PromiseGlobals),
        NativeFunctionKind::OrdinaryFunctionConstructor | NativeFunctionKind::ObjectConstructor => {
            Some(DeclarativeBatch::KernelGlobals)
        }
        _ => None,
    }
}

const fn is_error_identity(id: IntrinsicIdentity) -> bool {
    matches!(
        id,
        IntrinsicIdentity::Object(IntrinsicObjectId::ErrorPrototype(_))
            | IntrinsicIdentity::Function(IntrinsicFunctionId(
                NativeFunctionKind::ErrorConstructor(_)
                    | NativeFunctionKind::ErrorPrototypeToString
                    | NativeFunctionKind::ErrorIsError,
            ))
    )
}

const fn is_kernel_identity(id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(
            IntrinsicObjectId::ObjectPrototype | IntrinsicObjectId::GlobalObject,
        ) => true,
        IntrinsicIdentity::Function(id) => is_kernel_function(id),
        IntrinsicIdentity::Object(_) => false,
    }
}

const fn is_symbol_identity(id: IntrinsicIdentity) -> bool {
    matches!(
        id,
        IntrinsicIdentity::Object(IntrinsicObjectId::SymbolPrototype)
            | IntrinsicIdentity::Function(IntrinsicFunctionId(
                NativeFunctionKind::SymbolConstructor
                    | NativeFunctionKind::SymbolPrototypeToString
                    | NativeFunctionKind::SymbolPrototypeValueOf
                    | NativeFunctionKind::SymbolPrototypeToPrimitive
                    | NativeFunctionKind::SymbolPrototypeDescription
                    | NativeFunctionKind::SymbolFor
                    | NativeFunctionKind::SymbolKeyFor,
            ))
    )
}

const fn is_array_identity(id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(IntrinsicObjectId::ArrayPrototype) => true,
        IntrinsicIdentity::Function(id) => is_array_function(id),
        IntrinsicIdentity::Object(_) => false,
    }
}

const fn is_promise_identity(id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(IntrinsicObjectId::PromisePrototype) => true,
        IntrinsicIdentity::Function(id) => is_promise_function(id),
        IntrinsicIdentity::Object(_) => false,
    }
}

const fn is_promise_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::PromiseConstructor
            | NativeFunctionKind::PromiseResolve
            | NativeFunctionKind::PromiseReject
            | NativeFunctionKind::PromiseStatic(_)
            | NativeFunctionKind::PromiseSpeciesGetter
            | NativeFunctionKind::PromisePrototypeThen
            | NativeFunctionKind::PromisePrototypeCatch
            | NativeFunctionKind::PromisePrototypeFinally
    )
}

const fn is_iterator_identity(id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(
            IntrinsicObjectId::IteratorPrototype
            | IntrinsicObjectId::ArrayIteratorPrototype
            | IntrinsicObjectId::StringIteratorPrototype
            | IntrinsicObjectId::GeneratorFunctionPrototype
            | IntrinsicObjectId::GeneratorPrototype,
        ) => true,
        IntrinsicIdentity::Function(id) => is_iterator_function(id),
        IntrinsicIdentity::Object(_) => false,
    }
}

const fn is_primitive_identity(id: IntrinsicIdentity) -> bool {
    match id {
        IntrinsicIdentity::Object(id) => matches!(
            id,
            IntrinsicObjectId::BooleanPrototype
                | IntrinsicObjectId::NumberPrototype
                | IntrinsicObjectId::BigIntPrototype
                | IntrinsicObjectId::StringPrototype
        ),
        IntrinsicIdentity::Function(id) => is_primitive_function(id),
    }
}

const fn is_primitive_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::BooleanConstructor
            | NativeFunctionKind::BooleanPrototypeToString
            | NativeFunctionKind::BooleanPrototypeValueOf
            | NativeFunctionKind::NumberConstructor
            | NativeFunctionKind::NumberPrototypeToString
            | NativeFunctionKind::NumberPrototypeValueOf
            | NativeFunctionKind::NumberPredicateStatic(_)
            | NativeFunctionKind::NumberPrototypeFormat(_)
            | NativeFunctionKind::BigIntConstructor
            | NativeFunctionKind::BigIntPrototypeToString
            | NativeFunctionKind::BigIntPrototypeValueOf
            | NativeFunctionKind::BigIntAsIntN
            | NativeFunctionKind::BigIntAsUintN
            | NativeFunctionKind::StringConstructor
            | NativeFunctionKind::StringPrototypeToString
            | NativeFunctionKind::StringPrototypeValueOf
            | NativeFunctionKind::StringPrototypeMethod(_)
            | NativeFunctionKind::StringRaw
            | NativeFunctionKind::LocaleString(
                super::LocaleStringMethod::Number
                    | super::LocaleStringMethod::BigInt
                    | super::LocaleStringMethod::Array
            )
    )
}

pub(super) const fn is_kernel_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::FunctionPrototype
            | NativeFunctionKind::ThrowTypeError
            | NativeFunctionKind::OrdinaryFunctionConstructor
            | NativeFunctionKind::ObjectConstructor
            | NativeFunctionKind::ObjectPrototypeToString
            | NativeFunctionKind::ObjectPrototypeValueOf
            | NativeFunctionKind::ObjectPrototypeHasOwnProperty
            | NativeFunctionKind::ObjectPrototypeIsPrototypeOf
            | NativeFunctionKind::ObjectPrototypePropertyIsEnumerable
            | NativeFunctionKind::ObjectPrototypeProtoGetter
            | NativeFunctionKind::ObjectPrototypeProtoSetter
            | NativeFunctionKind::ObjectPrototypeDefineGetter
            | NativeFunctionKind::ObjectPrototypeDefineSetter
            | NativeFunctionKind::ObjectPrototypeLookupGetter
            | NativeFunctionKind::ObjectPrototypeLookupSetter
            | NativeFunctionKind::LocaleString(super::LocaleStringMethod::Object)
            | NativeFunctionKind::FunctionPrototypeToString
            | NativeFunctionKind::FunctionPrototypeCall
            | NativeFunctionKind::FunctionPrototypeApply
            | NativeFunctionKind::FunctionPrototypeBind
            | NativeFunctionKind::FunctionPrototypeHasInstance
            | NativeFunctionKind::ObjectCreate
            | NativeFunctionKind::ObjectGetPrototypeOf
            | NativeFunctionKind::ObjectSetPrototypeOf
            | NativeFunctionKind::ObjectDefineProperty
            | NativeFunctionKind::ObjectDefineProperties
            | NativeFunctionKind::ObjectGetOwnPropertyNames
            | NativeFunctionKind::ObjectGetOwnPropertySymbols
            | NativeFunctionKind::ObjectGroupBy
            | NativeFunctionKind::ObjectKeys
            | NativeFunctionKind::ObjectValues
            | NativeFunctionKind::ObjectEntries
            | NativeFunctionKind::ObjectIsExtensible
            | NativeFunctionKind::ObjectPreventExtensions
            | NativeFunctionKind::ObjectGetOwnPropertyDescriptor
            | NativeFunctionKind::ObjectGetOwnPropertyDescriptors
            | NativeFunctionKind::ObjectIs
            | NativeFunctionKind::ObjectAssign
            | NativeFunctionKind::ObjectSeal
            | NativeFunctionKind::ObjectFreeze
            | NativeFunctionKind::ObjectIsSealed
            | NativeFunctionKind::ObjectIsFrozen
            | NativeFunctionKind::ObjectFromEntries
            | NativeFunctionKind::ObjectHasOwn
    )
}

const fn is_array_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::ArrayConstructor
            | NativeFunctionKind::ArraySpeciesGetter
            | NativeFunctionKind::ArrayPrototypeJoin
            | NativeFunctionKind::ArrayPrototypeToString
            | NativeFunctionKind::ArrayPrototypeSearch(_)
            | NativeFunctionKind::ArrayPrototypeMutator(_)
            | NativeFunctionKind::ArrayPrototypeCopier(_)
            | NativeFunctionKind::ArrayPrototypeSort(_)
            | NativeFunctionKind::ArrayPrototypeFlatten(_)
            | NativeFunctionKind::ArrayPrototypeCallback(_)
            | NativeFunctionKind::ArrayPrototypeReduction(_)
            | NativeFunctionKind::ArrayPrototypeSplice
            | NativeFunctionKind::ArrayIsArray
            | NativeFunctionKind::ArrayStatic(_)
    )
}

const fn is_iterator_function(id: IntrinsicFunctionId) -> bool {
    matches!(
        id.0,
        NativeFunctionKind::IteratorPrototypeIterator
            | NativeFunctionKind::ArrayIteratorNext
            | NativeFunctionKind::ArrayPrototypeValues
            | NativeFunctionKind::ArrayPrototypeKeys
            | NativeFunctionKind::ArrayPrototypeEntries
            | NativeFunctionKind::StringIteratorNext
            | NativeFunctionKind::StringPrototypeIterator
            | NativeFunctionKind::GeneratorFunctionConstructor
            | NativeFunctionKind::GeneratorPrototypeNext
            | NativeFunctionKind::GeneratorPrototypeReturn
            | NativeFunctionKind::GeneratorPrototypeThrow
    )
}

pub(super) const fn function_batch(id: IntrinsicFunctionId) -> DeclarativeBatch {
    if is_kernel_function(id) {
        DeclarativeBatch::Kernel
    } else if matches!(
        id.0,
        NativeFunctionKind::ErrorConstructor(_)
            | NativeFunctionKind::ErrorPrototypeToString
            | NativeFunctionKind::ErrorIsError
    ) {
        DeclarativeBatch::Errors
    } else if matches!(
        id.0,
        NativeFunctionKind::GlobalNumeric(_) | NativeFunctionKind::GlobalUri(_)
    ) {
        DeclarativeBatch::Globals
    } else if is_primitive_function(id) {
        DeclarativeBatch::Primitives
    } else if is_array_function(id) {
        DeclarativeBatch::Arrays
    } else if is_iterator_function(id) {
        DeclarativeBatch::Iterators
    } else if is_promise_function(id) {
        DeclarativeBatch::Promises
    } else if matches!(
        id.0,
        NativeFunctionKind::SymbolConstructor
            | NativeFunctionKind::SymbolPrototypeToString
            | NativeFunctionKind::SymbolPrototypeValueOf
            | NativeFunctionKind::SymbolPrototypeToPrimitive
            | NativeFunctionKind::SymbolPrototypeDescription
            | NativeFunctionKind::SymbolFor
            | NativeFunctionKind::SymbolKeyFor
    ) {
        DeclarativeBatch::Symbols
    } else {
        DeclarativeBatch::NamespaceObjects
    }
}

fn count_specs<T>(
    visit: fn(&mut dyn FnMut(T)),
    resource: RuntimeResource,
) -> Result<usize, RuntimeError> {
    let mut count = Some(0_usize);
    visit(&mut |_| {
        count = count.and_then(|value| value.checked_add(1));
    });
    count.ok_or_else(|| allocation_failed(resource, usize::MAX))
}

fn visit_object_specs(visit: ObjectSink<'_>) {
    kernel::visit_objects(visit);
    error::visit_objects(visit);
    primitives::visit_objects(visit);
    array::visit_objects(visit);
    iterator::visit_objects(visit);
    generator::visit_objects(visit);
    async_function::visit_objects(visit);
    symbol::visit_objects(visit);
    promise::visit_objects(visit);
    reflect::visit_objects(visit);
    json::visit_objects(visit);
    math::visit_objects(visit);
}

fn visit_function_specs(visit: FunctionSink<'_>) {
    kernel::visit_functions(visit);
    error::visit_functions(visit);
    primitives::visit_functions(visit);
    string::visit_functions(visit);
    array::visit_kernel_functions(visit);
    iterator::visit_functions(visit);
    generator::visit_functions(visit);
    async_function::visit_functions(visit);
    symbol::visit_functions(visit);
    promise::visit_functions(visit);
    reflect::visit_functions(visit);
    json::visit_functions(visit);
    math::visit_functions(visit);
    globals::visit_functions(visit);
    array::visit_method_functions(visit);
}

fn visit_property_specs(visit: PropertySink<'_>) {
    kernel::visit_properties(visit);
    error::visit_properties(visit);
    primitives::visit_properties(visit);
    string::visit_properties(visit);
    array::visit_properties(visit);
    iterator::visit_properties(visit);
    generator::visit_properties(visit);
    async_function::visit_properties(visit);
    symbol::visit_properties(visit);
    promise::visit_properties(visit);
    globals::visit_properties(visit);
    reflect::visit_properties(visit);
    json::visit_properties(visit);
    math::visit_properties(visit);
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

const fn object_prototype() -> PrototypeSpec {
    PrototypeSpec::Intrinsic(IntrinsicIdentity::Object(
        IntrinsicObjectId::ObjectPrototype,
    ))
}

const fn function_prototype() -> IntrinsicFunctionId {
    IntrinsicFunctionId(NativeFunctionKind::FunctionPrototype)
}

const fn ordinary(
    implementation: NativeFunctionKind,
    name: IntrinsicNameSpec,
    length: i32,
) -> IntrinsicFunctionSpec {
    function(
        implementation,
        PrototypeSpec::Intrinsic(IntrinsicIdentity::Function(function_prototype())),
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
        identity_publication: IntrinsicIdentityPublication::Automatic,
    }
}

const fn data(
    holder: IntrinsicIdentity,
    key: IntrinsicKeySpec,
    layout: super::PropertyLayout,
    value: IntrinsicValueSpec,
) -> IntrinsicPropertySpec {
    IntrinsicPropertySpec {
        holder,
        key,
        descriptor: IntrinsicDescriptorSpec::Data { layout, value },
    }
}

const fn method(
    holder: IntrinsicIdentity,
    key: IntrinsicKeySpec,
    function: NativeFunctionKind,
) -> IntrinsicPropertySpec {
    data(
        holder,
        key,
        super::METHOD_PROPERTY,
        IntrinsicValueSpec::Function(IntrinsicFunctionId(function)),
    )
}

const fn accessor(
    holder: IntrinsicIdentity,
    key: IntrinsicKeySpec,
    layout: super::PropertyLayout,
    getter: Option<IntrinsicFunctionId>,
    setter: Option<IntrinsicFunctionId>,
) -> IntrinsicPropertySpec {
    IntrinsicPropertySpec {
        holder,
        key,
        descriptor: IntrinsicDescriptorSpec::Accessor {
            layout,
            getter,
            setter,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::realm::{atoms::RealmAtomPlan, reservation::RealmReservationPlan};

    #[test]
    fn complete_function_schema_has_characterized_cardinality_and_unique_ids() {
        let schema = RealmIntrinsicSchema::try_new().expect("function schema");
        assert_eq!(schema.specs().len(), 256);
        assert_eq!(schema.constructor_prototypes.len(), 20);
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
        let schema = RealmIntrinsicSchema::try_new().expect("function schema");
        assert_eq!(schema.objects.len(), IntrinsicObjectId::ALL.len());
        for id in IntrinsicObjectId::ALL {
            assert_eq!(
                schema.objects.iter().filter(|spec| spec.id == id).count(),
                1
            );
        }
    }

    #[test]
    fn ordinary_method_maintenance_is_derived_from_one_schema_declaration() {
        let mut schema = RealmIntrinsicSchema::try_new().expect("Realm schema");
        let baseline_atoms = RealmAtomPlan::try_new(&schema).expect("baseline atom plan");
        let baseline_reservation =
            RealmReservationPlan::try_new(&baseline_atoms, &schema).expect("baseline reservation");
        let method_id = IntrinsicFunctionId(NativeFunctionKind::ErrorIsError);

        let function_index = schema
            .specs
            .iter()
            .position(|function| function.id == method_id)
            .expect("Error.isError function declaration");
        let function = schema.specs.remove(function_index);
        let mandatory_index = schema
            .mandatory_functions
            .iter()
            .position(|candidate| *candidate == method_id)
            .expect("Error.isError mandatory identity");
        let mandatory = schema.mandatory_functions.remove(mandatory_index);
        let property_index = schema
            .properties
            .iter()
            .position(|property| {
                matches!(
                    property.descriptor,
                    IntrinsicDescriptorSpec::Data {
                        value: IntrinsicValueSpec::Function(candidate),
                        ..
                    } if candidate == method_id
                )
            })
            .expect("Error.isError publication declaration");
        assert_eq!(
            schema
                .properties
                .iter()
                .filter(|property| {
                    matches!(
                        property.descriptor,
                        IntrinsicDescriptorSpec::Data {
                            value: IntrinsicValueSpec::Function(candidate),
                            ..
                        } if candidate == method_id
                    )
                })
                .count(),
            1
        );
        let property = schema.properties.remove(property_index);

        let reduced_atoms = RealmAtomPlan::try_new(&schema).expect("derived reduced atom plan");
        let reduced_reservation = RealmReservationPlan::try_new(&reduced_atoms, &schema)
            .expect("derived reduced reservation");
        assert_eq!(reduced_atoms.len() + 1, baseline_atoms.len());
        assert_eq!(
            reduced_atoms.description_code_units() + "isError".encode_utf16().count(),
            baseline_atoms.description_code_units()
        );
        assert_eq!(
            reduced_reservation.functions() + 1,
            baseline_reservation.functions()
        );
        assert_eq!(
            reduced_reservation.object_properties() + 3,
            baseline_reservation.object_properties()
        );

        schema.specs.insert(function_index, function);
        schema
            .mandatory_functions
            .insert(mandatory_index, mandatory);
        schema.properties.insert(property_index, property);
        schema.validate().expect("restored complete Realm schema");
        let restored_atoms = RealmAtomPlan::try_new(&schema).expect("restored atom plan");
        let restored_reservation =
            RealmReservationPlan::try_new(&restored_atoms, &schema).expect("restored reservation");
        assert_eq!(restored_atoms.len(), baseline_atoms.len());
        assert_eq!(
            restored_atoms.description_code_units(),
            baseline_atoms.description_code_units()
        );
        assert_eq!(restored_reservation, baseline_reservation);
    }

    #[test]
    fn realm_facade_has_no_manual_bootstrap_mirrors() {
        let facade = include_str!("../../realm.rs");
        for obsolete_mirror in ["_ATOM_START", "RealmRecords", "RealmGraph"] {
            assert!(
                !facade.contains(obsolete_mirror),
                "Realm facade reintroduced {obsolete_mirror}"
            );
        }
    }
}
