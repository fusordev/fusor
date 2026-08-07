//! Typed, allocation-order-independent declarations for Realm intrinsics.
//!
//! The declarations in this module deliberately contain no arena indices and
//! have no access to mutable [`Runtime`](super::Runtime) state.  They are the
//! vocabulary consumed by Realm schema validation, atom planning, identity
//! allocation, and descriptor publication.

use super::{
    ArrayCallback, ArrayCopier, ArrayFlatten, ArrayMutator, ArrayReduction, ArraySearch, ArraySort,
    ArrayStatic, ErrorIntrinsicKind, GlobalNumericFunction, LocaleStringMethod, MapMethod,
    MathMethod, NativeFunctionKind, NumberFormat, NumberPredicate, PredefinedAtom, PromiseStatic,
    PropertyLayout, ReflectMethod, SetMethod, StringMethod, UriFunction,
};
use crate::object::TypedArrayElementType;
use crate::runtime::{
    ArrayBufferPrototypeMethod, AtomicsMethod, DataViewPrototypeMethod, DatePrototypeMethod,
    DateStaticMethod, SharedArrayBufferPrototypeMethod, TemporalDurationPrototypeMethod,
    TemporalDurationStaticMethod, TemporalInstantPrototypeMethod, TemporalInstantStaticMethod,
    TemporalPlainDatePrototypeMethod, TemporalPlainDateStaticMethod,
    TemporalPlainDateTimePrototypeMethod, TemporalPlainDateTimeStaticMethod,
    TemporalPlainMonthDayPrototypeMethod, TemporalPlainMonthDayStaticMethod,
    TemporalPlainTimePrototypeMethod, TemporalPlainTimeStaticMethod, TypedArrayPrototypeMethod,
};

/// Stable identity of an object allocated by Realm bootstrap.
///
/// These values describe ECMA-262 intrinsic identities.  They never contain a
/// generational arena index and therefore remain valid before allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicObjectId {
    ObjectPrototype,
    GlobalObject,
    ErrorPrototype(ErrorIntrinsicKind),
    BooleanPrototype,
    NumberPrototype,
    BigIntPrototype,
    StringPrototype,
    ArrayPrototype,
    ArrayBufferPrototype,
    SharedArrayBufferPrototype,
    DataViewPrototype,
    TypedArrayPrototype,
    TypedArrayInstancePrototype(TypedArrayElementType),
    DatePrototype,
    Temporal,
    TemporalDurationPrototype,
    TemporalInstantPrototype,
    TemporalPlainDatePrototype,
    TemporalPlainDateTimePrototype,
    TemporalPlainTimePrototype,
    TemporalPlainMonthDayPrototype,
    RegExpPrototype,
    IteratorPrototype,
    AsyncIteratorPrototype,
    AsyncFromSyncIteratorPrototype,
    ArrayIteratorPrototype,
    StringIteratorPrototype,
    RegExpStringIteratorPrototype,
    GeneratorFunctionPrototype,
    GeneratorPrototype,
    AsyncGeneratorFunctionPrototype,
    AsyncGeneratorPrototype,
    AsyncFunctionPrototype,
    SymbolPrototype,
    PromisePrototype,
    MapPrototype,
    MapIteratorPrototype,
    SetPrototype,
    SetIteratorPrototype,
    WeakMapPrototype,
    WeakSetPrototype,
    WeakRefPrototype,
    FinalizationRegistryPrototype,
    Reflect,
    Json,
    Math,
    Atomics,
}

impl IntrinsicObjectId {
    pub(in crate::runtime) const ALL: [Self; 66] = [
        Self::ObjectPrototype,
        Self::GlobalObject,
        Self::ErrorPrototype(ErrorIntrinsicKind::Error),
        Self::ErrorPrototype(ErrorIntrinsicKind::EvalError),
        Self::ErrorPrototype(ErrorIntrinsicKind::RangeError),
        Self::ErrorPrototype(ErrorIntrinsicKind::ReferenceError),
        Self::ErrorPrototype(ErrorIntrinsicKind::SyntaxError),
        Self::ErrorPrototype(ErrorIntrinsicKind::TypeError),
        Self::ErrorPrototype(ErrorIntrinsicKind::UriError),
        Self::ErrorPrototype(ErrorIntrinsicKind::InternalError),
        Self::ErrorPrototype(ErrorIntrinsicKind::AggregateError),
        Self::BooleanPrototype,
        Self::NumberPrototype,
        Self::BigIntPrototype,
        Self::StringPrototype,
        Self::ArrayPrototype,
        Self::ArrayBufferPrototype,
        Self::SharedArrayBufferPrototype,
        Self::DataViewPrototype,
        Self::TypedArrayPrototype,
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Int8),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Uint8),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Uint8Clamped),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Int16),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Uint16),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Int32),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Uint32),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::BigInt64),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::BigUint64),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Float16),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Float32),
        Self::TypedArrayInstancePrototype(TypedArrayElementType::Float64),
        Self::DatePrototype,
        Self::Temporal,
        Self::TemporalDurationPrototype,
        Self::TemporalInstantPrototype,
        Self::TemporalPlainDatePrototype,
        Self::TemporalPlainDateTimePrototype,
        Self::TemporalPlainTimePrototype,
        Self::TemporalPlainMonthDayPrototype,
        Self::RegExpPrototype,
        Self::IteratorPrototype,
        Self::AsyncIteratorPrototype,
        Self::AsyncFromSyncIteratorPrototype,
        Self::ArrayIteratorPrototype,
        Self::StringIteratorPrototype,
        Self::RegExpStringIteratorPrototype,
        Self::GeneratorFunctionPrototype,
        Self::GeneratorPrototype,
        Self::AsyncGeneratorFunctionPrototype,
        Self::AsyncGeneratorPrototype,
        Self::AsyncFunctionPrototype,
        Self::SymbolPrototype,
        Self::PromisePrototype,
        Self::MapPrototype,
        Self::MapIteratorPrototype,
        Self::SetPrototype,
        Self::SetIteratorPrototype,
        Self::WeakMapPrototype,
        Self::WeakSetPrototype,
        Self::WeakRefPrototype,
        Self::FinalizationRegistryPrototype,
        Self::Reflect,
        Self::Json,
        Self::Math,
        Self::Atomics,
    ];
}

/// Stable identity of a native function allocated by Realm bootstrap.
///
/// [`NativeFunctionKind`] already gives repeated families semantic identities
/// such as [`MathMethod`], [`ArrayCallback`], and [`ErrorIntrinsicKind`].  The
/// wrapper keeps schema identity distinct from implementation dispatch while
/// retaining those family types instead of introducing anonymous slot numbers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) struct IntrinsicFunctionId(pub(in crate::runtime) NativeFunctionKind);

/// Stable identity of a Realm-created JavaScript name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum RealmNameId {
    Call,
    Entries,
    KeyFor,
    Description,
    IsError,
    Bind,
    Reflect,
    JsonIsRawJson,
    JsonParse,
    JsonStringify,
    ParseFloat,
    ParseInt,
    SymbolStatic(PredefinedAtom),
    Uri(UriFunction),
    ObjectStatic(NativeFunctionKind),
    BigIntStatic(NativeFunctionKind),
    StringMethod(StringMethod),
    NumberValue(&'static str),
    NumberPredicate(NumberPredicate),
    StringStatic(StringMethod),
    ArraySearch(ArraySearch),
    ObjectPrototypeMethod(NativeFunctionKind),
    ArrayMutator(ArrayMutator),
    ArrayCopier(ArrayCopier),
    NumberFormat(NumberFormat),
    ArrayCallback(ArrayCallback),
    ArrayReduction(ArrayReduction),
    ArraySplice,
    ArrayIsArray,
    ArrayFromAsync,
    ArrayBufferIsView,
    ArrayBufferPrototype(ArrayBufferPrototypeMethod),
    SharedArrayBufferPrototype(SharedArrayBufferPrototypeMethod),
    DataViewPrototype(DataViewPrototypeMethod),
    TypedArrayPrototype(TypedArrayPrototypeMethod),
    TypedArrayBytesPerElement,
    DateStatic(DateStaticMethod),
    DatePrototype(DatePrototypeMethod),
    Temporal,
    Duration,
    Instant,
    PlainDate,
    PlainDateTime,
    PlainTime,
    PlainMonthDay,
    TemporalDurationStatic(TemporalDurationStaticMethod),
    TemporalDurationPrototype(TemporalDurationPrototypeMethod),
    TemporalInstantStatic(TemporalInstantStaticMethod),
    TemporalInstantPrototype(TemporalInstantPrototypeMethod),
    TemporalPlainDateStatic(TemporalPlainDateStaticMethod),
    TemporalPlainDatePrototype(TemporalPlainDatePrototypeMethod),
    TemporalPlainDateTimeStatic(TemporalPlainDateTimeStaticMethod),
    TemporalPlainDateTimePrototype(TemporalPlainDateTimePrototypeMethod),
    TemporalPlainTimeStatic(TemporalPlainTimeStaticMethod),
    TemporalPlainTimePrototype(TemporalPlainTimePrototypeMethod),
    TemporalPlainMonthDayStatic(TemporalPlainMonthDayStaticMethod),
    TemporalPlainMonthDayPrototype(TemporalPlainMonthDayPrototypeMethod),
    RegExpEscape,
    RegExpCompile,
    RegExpTest,
    PromiseStatic(PromiseStatic),
    ProxyRevocable,
    MapMethod(MapMethod),
    SetMethod(SetMethod),
    Deref,
    Register,
    Unregister,
    ArraySort(ArraySort),
    ArrayFlatten(ArrayFlatten),
    MathMethod(MathMethod),
    MathConstant(&'static str),
    Atomics,
    AtomicsMethod(AtomicsMethod),
}

/// A reference to an intrinsic identity before arena allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicIdentity {
    Object(IntrinsicObjectId),
    Function(IntrinsicFunctionId),
}

/// An intrinsic property's key without a runtime-local [`super::Atom`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicKeySpec {
    /// A predefined atom whose namespace is String.
    PredefinedString(PredefinedAtom),
    /// A string name interned by this Realm transaction.
    InternedString(RealmNameId),
    /// A name whose [`super::JsString`] is built as part of Realm setup.
    RealmCreatedName(RealmNameId),
    /// A predefined atom whose namespace is Symbol.
    WellKnownSymbol(PredefinedAtom),
    /// An integer-indexed own property.
    ArrayIndex(u32),
}

/// One intrinsic identity's `[[Prototype]]` declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum PrototypeSpec {
    Null,
    Intrinsic(IntrinsicIdentity),
}

/// Special object storage required by a small set of intrinsic prototypes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicObjectKind {
    Ordinary,
    BooleanPrototype,
    NumberPrototype,
    StringPrototype,
    ArrayPrototype,
    DatePrototype,
}

/// One object identity and its allocation-time internal slots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) struct IntrinsicObjectSpec {
    pub(in crate::runtime) id: IntrinsicObjectId,
    pub(in crate::runtime) prototype: PrototypeSpec,
    pub(in crate::runtime) kind: IntrinsicObjectKind,
}

/// How a function's observable `name` value is obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicNameSpec {
    Predefined(PredefinedAtom),
    RealmName(RealmNameId),
    Literal(&'static str),
}

/// Where derived ordinary `length` and `name` descriptors participate in the
/// function's observable own-key order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicIdentityPublication {
    /// Publish `length` and `name` before every declared family property.
    Automatic,
    /// Publish a constructor's declared `prototype`, then `length` and `name`.
    AutomaticAfterPrototype,
    /// Both descriptors appear explicitly in the property declaration.
    Declared,
}

impl IntrinsicIdentityPublication {
    pub(in crate::runtime) const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic | Self::AutomaticAfterPrototype)
    }
}

/// One Realm-owned native function identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) struct IntrinsicFunctionSpec {
    pub(in crate::runtime) id: IntrinsicFunctionId,
    pub(in crate::runtime) implementation: NativeFunctionKind,
    pub(in crate::runtime) prototype: PrototypeSpec,
    pub(in crate::runtime) name: IntrinsicNameSpec,
    pub(in crate::runtime) length: i32,
    pub(in crate::runtime) constructable: bool,
    pub(in crate::runtime) identity_publication: IntrinsicIdentityPublication,
}

/// A string value used by an intrinsic data descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicStringSpec {
    Predefined(PredefinedAtom),
    RealmName(RealmNameId),
    Literal(&'static str),
}

/// A descriptor value whose Realm-local references are not allocated yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicValueSpec {
    Undefined,
    Null,
    Boolean(bool),
    NumberBits(u64),
    String(IntrinsicStringSpec),
    Object(IntrinsicObjectId),
    Function(IntrinsicFunctionId),
    WellKnownSymbol(PredefinedAtom),
}

/// Complete data or accessor descriptor for one intrinsic property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) enum IntrinsicDescriptorSpec {
    Data {
        layout: PropertyLayout,
        value: IntrinsicValueSpec,
    },
    Accessor {
        layout: PropertyLayout,
        getter: Option<IntrinsicFunctionId>,
        setter: Option<IntrinsicFunctionId>,
    },
}

/// One property in specification declaration order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) struct IntrinsicPropertySpec {
    pub(in crate::runtime) holder: IntrinsicIdentity,
    pub(in crate::runtime) key: IntrinsicKeySpec,
    pub(in crate::runtime) descriptor: IntrinsicDescriptorSpec,
}

/// A required constructor/prototype pair used by graph validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) struct ConstructorPrototypeSpec {
    pub(in crate::runtime) constructor: IntrinsicFunctionId,
    pub(in crate::runtime) prototype: IntrinsicIdentity,
}

/// Expected size of one semantic family before it is materialized as an array.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime) struct FamilyCardinality {
    pub(in crate::runtime) family: &'static str,
    pub(in crate::runtime) actual: usize,
    pub(in crate::runtime) expected: usize,
}

/// Ordered intrinsic declaration graph supplied to Realm construction.
#[derive(Clone, Copy)]
pub(in crate::runtime) struct IntrinsicSchema<'a> {
    pub(in crate::runtime) objects: &'a [IntrinsicObjectSpec],
    pub(in crate::runtime) functions: &'a [IntrinsicFunctionSpec],
    pub(in crate::runtime) properties: &'a [IntrinsicPropertySpec],
    pub(in crate::runtime) mandatory_objects: &'a [IntrinsicObjectId],
    pub(in crate::runtime) mandatory_functions: &'a [IntrinsicFunctionId],
    pub(in crate::runtime) constructor_prototypes: &'a [ConstructorPrototypeSpec],
    pub(in crate::runtime) family_cardinalities: &'a [FamilyCardinality],
}

// Keep the semantic family types visible in rustdoc for the schema contract.
const _: Option<(
    ArrayStatic,
    GlobalNumericFunction,
    LocaleStringMethod,
    ReflectMethod,
)> = None;

// Keep the complete schema vocabulary type-checked even when the current
// profile has no Realm-created-name, indexed, null, or Boolean descriptor.
const _: Option<(
    IntrinsicKeySpec,
    IntrinsicKeySpec,
    IntrinsicValueSpec,
    IntrinsicValueSpec,
)> = Some((
    IntrinsicKeySpec::RealmCreatedName(RealmNameId::Call),
    IntrinsicKeySpec::ArrayIndex(0),
    IntrinsicValueSpec::Null,
    IntrinsicValueSpec::Boolean(false),
));
