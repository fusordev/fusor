/*
 * JavaScript runtime and closure ownership derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Runtime construction and failure-atomic realm intrinsic graph publication.

mod atoms;
mod reservation;
mod schema;
mod transaction;
mod validation;

use std::collections::TryReserveError;

use super::{
    Arc, Arena, ArrayCallback, ArrayCopier, ArrayFlatten, ArrayIntrinsics, ArrayMutator,
    ArrayReduction, ArraySearch, ArraySort, ArrayState, ArrayStatic, AtomError, AtomTable,
    BigIntIntrinsics, BooleanIntrinsics, BoxedPrimitive, Context, ErrorIntrinsic,
    ErrorIntrinsicKind, ErrorIntrinsics, FunctionId, FunctionImplementation, GlobalNumericFunction,
    HandleError, HandleKind, HashMap, HeapFunction, HeapObject, HeapReference, InterruptState,
    IteratorIntrinsics, JsNumber, JsString, LocaleStringMethod, MathMethod, NativeFunction,
    NativeFunctionKind, NumberFormat, NumberIntrinsics, NumberPredicate, ObjectId, ObjectRecord,
    PredefinedAtom, PropertyKey, PropertyLayout, Realm, RealmHandle, RealmId, RealmIntrinsics,
    RealmState, ReflectMethod, ReleaseMailbox, Runtime, RuntimeError, RuntimeIdentity,
    RuntimeLimits, RuntimeResource, StoredValue, StringIntrinsics, StringMethod, SymbolIntrinsics,
    UriFunction, check_limit, predefined_string, usize_to_u64,
};

use atoms::{RealmAtomBindings, RealmAtomPlan};
use reservation::RealmReservationPlan;
use schema::RealmNameId;
use transaction::RealmBuildTransaction;

/// The `BigInt` static names that have no predefined atom.
const BIGINT_INTERNED_STATICS: [&str; 2] = ["asIntN", "asUintN"];

/// The `String.prototype` methods this profile installs.
///
/// The set is deliberately narrower than the pinned oracle's: `replace`
/// implements its `@@replace` protocol and exact plain-string path, while the
/// remaining RegExp-coupled methods (`match`, `matchAll`, `replaceAll`,
/// `search`, and `split`) and the Annex B HTML wrappers remain absent and
/// therefore fail closed rather than behaving incorrectly.
///
/// Each `length` matches the pinned oracle, which reports `1` for most methods
/// and `2` for `slice`, `substr`, and `substring`.
const STRING_PROTOTYPE_METHODS: [StringPrototypeMethod; 28] = [
    StringPrototypeMethod::interned("at", StringMethod::At, 1),
    StringPrototypeMethod::interned("charAt", StringMethod::CharAt, 1),
    StringPrototypeMethod::interned("charCodeAt", StringMethod::CharCodeAt, 1),
    StringPrototypeMethod::interned("codePointAt", StringMethod::CodePointAt, 1),
    StringPrototypeMethod::predefined(PredefinedAtom::Concat, StringMethod::Concat, 1),
    StringPrototypeMethod::interned("endsWith", StringMethod::EndsWith, 1),
    StringPrototypeMethod::interned("includes", StringMethod::Includes, 1),
    StringPrototypeMethod::interned("indexOf", StringMethod::IndexOf, 1),
    StringPrototypeMethod::interned("lastIndexOf", StringMethod::LastIndexOf, 1),
    StringPrototypeMethod::interned("padEnd", StringMethod::PadEnd, 1),
    StringPrototypeMethod::interned("padStart", StringMethod::PadStart, 1),
    StringPrototypeMethod::interned("repeat", StringMethod::Repeat, 1),
    StringPrototypeMethod::interned("replace", StringMethod::Replace, 2),
    StringPrototypeMethod::interned("slice", StringMethod::Slice, 2),
    StringPrototypeMethod::interned("startsWith", StringMethod::StartsWith, 1),
    StringPrototypeMethod::interned("substr", StringMethod::Substr, 2),
    StringPrototypeMethod::interned("substring", StringMethod::Substring, 2),
    StringPrototypeMethod::interned("trim", StringMethod::Trim, 0),
    StringPrototypeMethod::interned("trimEnd", StringMethod::TrimEnd, 0),
    StringPrototypeMethod::interned("trimStart", StringMethod::TrimStart, 0),
    StringPrototypeMethod::interned("isWellFormed", StringMethod::IsWellFormed, 0),
    StringPrototypeMethod::interned("toWellFormed", StringMethod::ToWellFormed, 0),
    StringPrototypeMethod::interned("toLowerCase", StringMethod::ToLowerCase, 0),
    StringPrototypeMethod::interned("toUpperCase", StringMethod::ToUpperCase, 0),
    StringPrototypeMethod::interned("toLocaleLowerCase", StringMethod::ToLocaleLowerCase, 0),
    StringPrototypeMethod::interned("toLocaleUpperCase", StringMethod::ToLocaleUpperCase, 0),
    StringPrototypeMethod::interned("normalize", StringMethod::Normalize, 0),
    StringPrototypeMethod::interned("localeCompare", StringMethod::LocaleCompare, 1),
];

/// One `String.prototype` method's name, implementation, and reported `length`.
#[derive(Clone, Copy)]
struct StringPrototypeMethod {
    /// The predefined atom for this name, when one exists.
    predefined_name: Option<PredefinedAtom>,
    /// The literal name to intern when no predefined atom exists.
    interned_name: Option<&'static str>,
    method: StringMethod,
    length: i32,
}

impl StringPrototypeMethod {
    const fn predefined(name: PredefinedAtom, method: StringMethod, length: i32) -> Self {
        Self {
            predefined_name: Some(name),
            interned_name: None,
            method,
            length,
        }
    }

    const fn interned(name: &'static str, method: StringMethod, length: i32) -> Self {
        Self {
            predefined_name: None,
            interned_name: Some(name),
            method,
            length,
        }
    }
}

/// The `Number` constructor's numeric value properties.
///
/// Each is non-writable, non-enumerable, and non-configurable, which the pinned
/// oracle confirms for `Number.MAX_VALUE`. The values are given as exact binary64
/// bit patterns so no decimal literal has to round-trip: the oracle reports
/// `MAX_VALUE` as `0x7fefffffffffffff`, `MIN_VALUE` as `0x1` (the smallest
/// subnormal), and `EPSILON` as `0x3cb0000000000000`.
/// `Number.NaN` is excluded because `NaN` already has a predefined atom, which
/// [`NUMBER_PREDEFINED_VALUE_STATICS`] reuses instead of interning a duplicate.
const NUMBER_VALUE_STATICS: [(&str, u64); 7] = [
    ("MAX_VALUE", 0x7fef_ffff_ffff_ffff),
    ("MIN_VALUE", 0x1),
    ("EPSILON", 0x3cb0_0000_0000_0000),
    ("MAX_SAFE_INTEGER", 0x433f_ffff_ffff_ffff),
    ("MIN_SAFE_INTEGER", 0xc33f_ffff_ffff_ffff),
    ("POSITIVE_INFINITY", 0x7ff0_0000_0000_0000),
    ("NEGATIVE_INFINITY", 0xfff0_0000_0000_0000),
];

/// The `Number` value statics whose names already have a predefined atom.
const NUMBER_PREDEFINED_VALUE_STATICS: [(PredefinedAtom, u64); 1] =
    [(PredefinedAtom::Nan, 0x7ff8_0000_0000_0000)];

/// The `Number` constructor's predicate statics.
///
/// Each takes exactly one argument and answers `false` for a non-Number without
/// converting it, which is what separates `Number.isNaN` from the global `isNaN`:
/// the oracle reports `Number.isFinite('1')` as `false`.
const NUMBER_PREDICATE_STATICS: [(&str, NumberPredicate); 4] = [
    ("isInteger", NumberPredicate::IsInteger),
    ("isSafeInteger", NumberPredicate::IsSafeInteger),
    ("isFinite", NumberPredicate::IsFinite),
    ("isNaN", NumberPredicate::IsNaN),
];

/// The coercing numeric functions installed directly on the global object.
///
/// The two parsers are also exposed through `Number` as aliases of these same
/// function identities, as required by ECMA-262.
const GLOBAL_NUMERIC_FUNCTIONS: [(GlobalNumericFunction, i32); 4] = [
    (GlobalNumericFunction::IsFinite, 1),
    (GlobalNumericFunction::IsNaN, 1),
    (GlobalNumericFunction::ParseFloat, 1),
    (GlobalNumericFunction::ParseInt, 2),
];

/// The four ECMA-262 URI handling functions, in global property order.
const URI_FUNCTIONS: [(&str, UriFunction); 4] = [
    ("decodeURI", UriFunction::DecodeUri),
    ("decodeURIComponent", UriFunction::DecodeUriComponent),
    ("encodeURI", UriFunction::EncodeUri),
    ("encodeURIComponent", UriFunction::EncodeUriComponent),
];

/// The `String` constructor's code-unit factories.
///
/// Both report arity 1 even though both are variadic, which the pinned oracle
/// confirms.
const STRING_FROM_STATICS: [(&str, StringMethod); 2] = [
    ("fromCharCode", StringMethod::FromCharCode),
    ("fromCodePoint", StringMethod::FromCodePoint),
];

/// The `Number.prototype` decimal renderings this profile installs.
///
/// Each reports arity 1, which the pinned oracle confirms.
const NUMBER_FORMAT_METHODS: [NumberFormat; 3] = [
    NumberFormat::Fixed,
    NumberFormat::Exponential,
    NumberFormat::Precision,
];

/// The stable sorting methods sharing `SortIndexedProperties`.
const ARRAY_SORT_METHODS: [ArraySort; 2] = [ArraySort::Sort, ArraySort::ToSorted];

/// The methods sharing the resumable `FlattenIntoArray` implementation.
const ARRAY_FLATTEN_METHODS: [ArrayFlatten; 2] = [ArrayFlatten::Flat, ArrayFlatten::FlatMap];

/// The `%Math%` numeric constants in specification and pinned-oracle order.
///
/// Exact binary64 payloads avoid depending on host decimal parsing or libm
/// definitions during realm construction.
const MATH_CONSTANTS: [(&str, u64); 8] = [
    ("E", 0x4005_bf0a_8b14_5769),
    ("LN10", 0x4002_6bb1_bbb5_5516),
    ("LN2", 0x3fe6_2e42_fefa_39ef),
    ("LOG2E", 0x3ff7_1547_652b_82fe),
    ("LOG10E", 0x3fdb_cb7b_1526_e50e),
    ("PI", 0x4009_21fb_5444_2d18),
    ("SQRT1_2", 0x3fe6_a09e_667f_3bcd),
    ("SQRT2", 0x3ff6_a09e_667f_3bcd),
];

/// Locale-string methods installed for the deterministic no-`Intl` profile.
const LOCALE_STRING_METHODS: [LocaleStringMethod; 4] = [
    LocaleStringMethod::Object,
    LocaleStringMethod::Number,
    LocaleStringMethod::BigInt,
    LocaleStringMethod::Array,
];

/// The `Array.prototype` reductions this profile installs.
const ARRAY_REDUCTION_METHODS: [ArrayReduction; 2] =
    [ArrayReduction::Reduce, ArrayReduction::ReduceRight];

/// The `Array.prototype` callback methods this profile installs.
///
/// Every one reports arity 1, which the pinned oracle confirms.
const ARRAY_CALLBACK_METHODS: [ArrayCallback; 9] = [
    ArrayCallback::ForEach,
    ArrayCallback::Map,
    ArrayCallback::Filter,
    ArrayCallback::Every,
    ArrayCallback::Some,
    ArrayCallback::Find,
    ArrayCallback::FindIndex,
    ArrayCallback::FindLast,
    ArrayCallback::FindLastIndex,
];

/// The `Array.prototype` copying methods whose names must be interned.
///
/// `concat` and `with` are excluded because they already have predefined atoms.
/// Interning a duplicate breaks the atom table's rollback invariant, which is
/// exactly how this was caught.
const ARRAY_COPIER_METHODS: [ArrayCopier; 4] = [
    ArrayCopier::Slice,
    ArrayCopier::At,
    ArrayCopier::ToReversed,
    ArrayCopier::ToSpliced,
];

/// The `Array.prototype` copying methods that reuse predefined atoms.
const ARRAY_PREDEFINED_COPIERS: [(PredefinedAtom, ArrayCopier); 2] = [
    (PredefinedAtom::Concat, ArrayCopier::Concat),
    (PredefinedAtom::With, ArrayCopier::With),
];

/// The total number of installed copying methods.
const ARRAY_COPIER_TOTAL: usize = ARRAY_COPIER_METHODS.len() + ARRAY_PREDEFINED_COPIERS.len();

/// The `Array.prototype` mutators this profile installs.
///
/// Each name and arity comes from the pinned oracle, which reports `2` for
/// `copyWithin`, `1` for `push`, `unshift`, and `fill`, and `0` for `pop`,
/// `shift`, and `reverse`.
const ARRAY_MUTATOR_METHODS: [ArrayMutator; 7] = [
    ArrayMutator::Push,
    ArrayMutator::Pop,
    ArrayMutator::Shift,
    ArrayMutator::Unshift,
    ArrayMutator::Reverse,
    ArrayMutator::Fill,
    ArrayMutator::CopyWithin,
];

/// The `Object.prototype` reflection methods.
///
/// Each entry pairs the interned name with its implementation and reported
/// `length`, all of which the pinned oracle reports as 1. These are the methods
/// the Error corpus reaches through `Object.prototype.hasOwnProperty.call`.
const OBJECT_PROTOTYPE_REFLECTION: [(&str, NativeFunctionKind, i32); 3] = [
    (
        "hasOwnProperty",
        NativeFunctionKind::ObjectPrototypeHasOwnProperty,
        1,
    ),
    (
        "isPrototypeOf",
        NativeFunctionKind::ObjectPrototypeIsPrototypeOf,
        1,
    ),
    (
        "propertyIsEnumerable",
        NativeFunctionKind::ObjectPrototypePropertyIsEnumerable,
        1,
    ),
];

/// The `Array.prototype` searches this profile installs.
///
/// Each reports arity 1, which the pinned oracle confirms.
const ARRAY_SEARCH_METHODS: [(&str, ArraySearch); 3] = [
    ("indexOf", ArraySearch::IndexOf),
    ("lastIndexOf", ArraySearch::LastIndexOf),
    ("includes", ArraySearch::Includes),
];

/// The admitted `Object` constructor static methods in `QuickJS` property-table
/// order.
///
/// Each entry pairs the property name with the native implementation and its
/// reported `length`. A name that already has a predefined atom reuses it; the
/// rest are interned during realm construction like the `Symbol` statics. The
/// set is deliberately narrower than the pinned oracle's: only reflection
/// operations the current profile can honor completely are installed, so an
/// absent method fails closed as a missing property rather than behaving
/// incorrectly.
const OBJECT_STATIC_METHODS: [ObjectStaticMethod; 23] = [
    ObjectStaticMethod::interned("create", NativeFunctionKind::ObjectCreate, 2),
    ObjectStaticMethod::predefined(
        PredefinedAtom::GetPrototypeOf,
        NativeFunctionKind::ObjectGetPrototypeOf,
        1,
    ),
    ObjectStaticMethod::predefined(
        PredefinedAtom::SetPrototypeOf,
        NativeFunctionKind::ObjectSetPrototypeOf,
        2,
    ),
    ObjectStaticMethod::predefined(
        PredefinedAtom::DefineProperty,
        NativeFunctionKind::ObjectDefineProperty,
        3,
    ),
    ObjectStaticMethod::predefined(
        PredefinedAtom::DefineProperties,
        NativeFunctionKind::ObjectDefineProperties,
        2,
    ),
    ObjectStaticMethod::interned(
        "getOwnPropertyNames",
        NativeFunctionKind::ObjectGetOwnPropertyNames,
        1,
    ),
    ObjectStaticMethod::interned(
        "getOwnPropertySymbols",
        NativeFunctionKind::ObjectGetOwnPropertySymbols,
        1,
    ),
    ObjectStaticMethod::interned("groupBy", NativeFunctionKind::ObjectGroupBy, 2),
    ObjectStaticMethod::predefined(PredefinedAtom::Keys, NativeFunctionKind::ObjectKeys, 1),
    ObjectStaticMethod::predefined(PredefinedAtom::Values, NativeFunctionKind::ObjectValues, 1),
    ObjectStaticMethod::realm_name(RealmNameId::Entries, NativeFunctionKind::ObjectEntries, 1),
    ObjectStaticMethod::predefined(
        PredefinedAtom::IsExtensible,
        NativeFunctionKind::ObjectIsExtensible,
        1,
    ),
    ObjectStaticMethod::predefined(
        PredefinedAtom::PreventExtensions,
        NativeFunctionKind::ObjectPreventExtensions,
        1,
    ),
    ObjectStaticMethod::predefined(
        PredefinedAtom::GetOwnPropertyDescriptor,
        NativeFunctionKind::ObjectGetOwnPropertyDescriptor,
        2,
    ),
    ObjectStaticMethod::interned(
        "getOwnPropertyDescriptors",
        NativeFunctionKind::ObjectGetOwnPropertyDescriptors,
        1,
    ),
    ObjectStaticMethod::interned("is", NativeFunctionKind::ObjectIs, 2),
    ObjectStaticMethod::interned("assign", NativeFunctionKind::ObjectAssign, 2),
    ObjectStaticMethod::interned("seal", NativeFunctionKind::ObjectSeal, 1),
    ObjectStaticMethod::interned("freeze", NativeFunctionKind::ObjectFreeze, 1),
    ObjectStaticMethod::interned("isSealed", NativeFunctionKind::ObjectIsSealed, 1),
    ObjectStaticMethod::interned("isFrozen", NativeFunctionKind::ObjectIsFrozen, 1),
    ObjectStaticMethod::interned("fromEntries", NativeFunctionKind::ObjectFromEntries, 1),
    ObjectStaticMethod::interned("hasOwn", NativeFunctionKind::ObjectHasOwn, 2),
];

/// One `Object` static method's name, implementation, and reported `length`.
#[derive(Clone, Copy)]
struct ObjectStaticMethod {
    /// The predefined atom for this name, when one exists.
    predefined_name: Option<PredefinedAtom>,
    /// A Realm-local atom already declared by another intrinsic name.
    realm_name: Option<RealmNameId>,
    /// The literal name to intern when no predefined atom exists.
    interned_name: Option<&'static str>,
    kind: NativeFunctionKind,
    length: i32,
}

impl ObjectStaticMethod {
    const fn predefined(name: PredefinedAtom, kind: NativeFunctionKind, length: i32) -> Self {
        Self {
            predefined_name: Some(name),
            realm_name: None,
            interned_name: None,
            kind,
            length,
        }
    }

    const fn interned(name: &'static str, kind: NativeFunctionKind, length: i32) -> Self {
        Self {
            predefined_name: None,
            realm_name: None,
            interned_name: Some(name),
            kind,
            length,
        }
    }

    const fn realm_name(name: RealmNameId, kind: NativeFunctionKind, length: i32) -> Self {
        Self {
            predefined_name: None,
            realm_name: Some(name),
            interned_name: None,
            kind,
            length,
        }
    }
}

const DYNAMIC_SYMBOL_STATIC_PROPERTIES: [(&str, PredefinedAtom); 12] = [
    ("toPrimitive", PredefinedAtom::SymbolToPrimitive),
    ("iterator", PredefinedAtom::SymbolIterator),
    ("match", PredefinedAtom::SymbolMatch),
    ("matchAll", PredefinedAtom::SymbolMatchAll),
    ("replace", PredefinedAtom::SymbolReplace),
    ("search", PredefinedAtom::SymbolSearch),
    ("toStringTag", PredefinedAtom::SymbolToStringTag),
    (
        "isConcatSpreadable",
        PredefinedAtom::SymbolIsConcatSpreadable,
    ),
    ("hasInstance", PredefinedAtom::SymbolHasInstance),
    ("species", PredefinedAtom::SymbolSpecies),
    ("unscopables", PredefinedAtom::SymbolUnscopables),
    ("asyncIterator", PredefinedAtom::SymbolAsyncIterator),
];

const METHOD_PROPERTY: PropertyLayout = PropertyLayout::data(true, false, true);
const IDENTITY_PROPERTY: PropertyLayout = PropertyLayout::data(false, false, true);
const FROZEN_PROPERTY: PropertyLayout = PropertyLayout::data(false, false, false);
const CONSTRUCTOR_PROTOTYPE_PROPERTY: PropertyLayout = PropertyLayout::data(false, false, false);
const ARRAY_LENGTH_PROPERTY: PropertyLayout = PropertyLayout::data(true, false, false);

struct RealmKeys {
    errors: [PropertyKey; ErrorIntrinsicKind::ALL.len()],
    function: PropertyKey,
    object: PropertyKey,
    bigint: PropertyKey,
    join: PropertyKey,
    boolean: PropertyKey,
    number: PropertyKey,
    string: PropertyKey,
    array: PropertyKey,
    symbol: PropertyKey,
    prototype: PropertyKey,
    constructor: PropertyKey,
    length: PropertyKey,
    name: PropertyKey,
    message: PropertyKey,
    to_string: PropertyKey,
    value_of: PropertyKey,
    apply: PropertyKey,
    caller: PropertyKey,
    arguments: PropertyKey,
    values: PropertyKey,
    keys: PropertyKey,
    next: PropertyKey,
    symbol_iterator: PropertyKey,
    symbol_to_primitive: PropertyKey,
    symbol_to_string_tag: PropertyKey,
    symbol_has_instance: PropertyKey,
    symbol_species: PropertyKey,
    for_key: PropertyKey,
    split: PropertyKey,
}

impl RealmKeys {
    fn new(atoms: &AtomTable) -> Self {
        let key = |atom| PropertyKey::from_validated_atom(atoms.predefined(atom));
        Self {
            errors: ErrorIntrinsicKind::ALL.map(|kind| key(kind.predefined_atom())),
            function: key(PredefinedAtom::Function),
            object: key(PredefinedAtom::Object),
            bigint: key(PredefinedAtom::BigInt),
            join: key(PredefinedAtom::Join),
            boolean: key(PredefinedAtom::Boolean),
            number: key(PredefinedAtom::Number),
            string: key(PredefinedAtom::String),
            array: key(PredefinedAtom::Array),
            symbol: key(PredefinedAtom::Symbol),
            prototype: key(PredefinedAtom::Prototype),
            constructor: key(PredefinedAtom::Constructor),
            length: key(PredefinedAtom::Length),
            name: key(PredefinedAtom::Name),
            message: key(PredefinedAtom::Message),
            to_string: key(PredefinedAtom::ToString),
            value_of: key(PredefinedAtom::ValueOf),
            apply: key(PredefinedAtom::Apply),
            caller: key(PredefinedAtom::Caller),
            arguments: key(PredefinedAtom::ArgumentsIdentifier),
            values: key(PredefinedAtom::Values),
            keys: key(PredefinedAtom::Keys),
            next: key(PredefinedAtom::Next),
            symbol_iterator: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolIterator),
            ),
            symbol_to_primitive: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolToPrimitive),
            ),
            symbol_to_string_tag: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolToStringTag),
            ),
            symbol_has_instance: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolHasInstance),
            ),
            symbol_species: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolSpecies),
            ),
            for_key: key(PredefinedAtom::For),
            split: key(PredefinedAtom::Split),
        }
    }
}

struct RealmNames {
    errors: [JsString; ErrorIntrinsicKind::ALL.len()],
    function: JsString,
    object: JsString,
    bigint: JsString,
    join: JsString,
    as_int_n: JsString,
    as_uint_n: JsString,
    boolean: JsString,
    number: JsString,
    string: JsString,
    array: JsString,
    symbol: JsString,
    empty: JsString,
    to_string: JsString,
    value_of: JsString,
    apply: JsString,
    call: JsString,
    bind: JsString,
    has_instance: JsString,
    values: JsString,
    keys: JsString,
    entries: JsString,
    next: JsString,
    key_for: JsString,
    symbol_for: JsString,
    description: JsString,
    is_error: JsString,
    array_iterator: JsString,
    string_iterator: JsString,
    symbol_iterator_name: JsString,
    symbol_to_primitive_name: JsString,
    get_description: JsString,
    get_species: JsString,
    reflect: JsString,
    json: JsString,
    is_raw_json: JsString,
    parse: JsString,
    raw_json: JsString,
    stringify: JsString,
    math: JsString,
}

impl RealmNames {
    fn try_new(atoms: &AtomTable) -> Result<Self, RuntimeError> {
        Ok(Self {
            errors: ErrorIntrinsicKind::ALL
                .map(|kind| predefined_string(atoms, kind.predefined_atom())),
            function: predefined_string(atoms, PredefinedAtom::Function),
            object: predefined_string(atoms, PredefinedAtom::Object),
            bigint: predefined_string(atoms, PredefinedAtom::BigInt),
            join: predefined_string(atoms, PredefinedAtom::Join),
            as_int_n: JsString::from_utf8("asIntN").map_err(AtomError::from)?,
            as_uint_n: JsString::from_utf8("asUintN").map_err(AtomError::from)?,
            boolean: predefined_string(atoms, PredefinedAtom::Boolean),
            number: predefined_string(atoms, PredefinedAtom::Number),
            string: predefined_string(atoms, PredefinedAtom::String),
            array: predefined_string(atoms, PredefinedAtom::Array),
            symbol: predefined_string(atoms, PredefinedAtom::Symbol),
            empty: predefined_string(atoms, PredefinedAtom::EmptyString),
            to_string: predefined_string(atoms, PredefinedAtom::ToString),
            value_of: predefined_string(atoms, PredefinedAtom::ValueOf),
            apply: predefined_string(atoms, PredefinedAtom::Apply),
            call: JsString::from_utf8("call").map_err(AtomError::from)?,
            bind: JsString::from_utf8("bind").map_err(AtomError::from)?,
            has_instance: JsString::from_utf8("[Symbol.hasInstance]").map_err(AtomError::from)?,
            values: predefined_string(atoms, PredefinedAtom::Values),
            keys: predefined_string(atoms, PredefinedAtom::Keys),
            entries: JsString::from_utf8("entries").map_err(AtomError::from)?,
            next: predefined_string(atoms, PredefinedAtom::Next),
            key_for: JsString::from_utf8("keyFor").map_err(AtomError::from)?,
            symbol_for: predefined_string(atoms, PredefinedAtom::For),
            description: JsString::from_utf8("description").map_err(AtomError::from)?,
            is_error: JsString::from_utf8("isError").map_err(AtomError::from)?,
            array_iterator: predefined_string(atoms, PredefinedAtom::ArrayIterator),
            string_iterator: predefined_string(atoms, PredefinedAtom::StringIterator),
            symbol_iterator_name: JsString::from_utf8("[Symbol.iterator]")
                .map_err(AtomError::from)?,
            symbol_to_primitive_name: JsString::from_utf8("[Symbol.toPrimitive]")
                .map_err(AtomError::from)?,
            get_description: JsString::from_utf8("get description").map_err(AtomError::from)?,
            get_species: JsString::from_utf8("get [Symbol.species]").map_err(AtomError::from)?,
            reflect: JsString::from_utf8("Reflect").map_err(AtomError::from)?,
            json: predefined_string(atoms, PredefinedAtom::Json),
            is_raw_json: JsString::from_utf8("isRawJSON").map_err(AtomError::from)?,
            parse: JsString::from_utf8("parse").map_err(AtomError::from)?,
            raw_json: predefined_string(atoms, PredefinedAtom::RawJson),
            stringify: JsString::from_utf8("stringify").map_err(AtomError::from)?,
            math: predefined_string(atoms, PredefinedAtom::Math),
        })
    }
}

/// Reserved records for the ordinary `%Reflect%` object and its method set.
struct ReflectRecords {
    object: ObjectRecord,
    methods: [ObjectRecord; ReflectMethod::ALL.len()],
}

/// Reserved records for the currently installed `%JSON%` surface.
struct JsonRecords {
    object: ObjectRecord,
    is_raw_json: ObjectRecord,
    parse: ObjectRecord,
    raw_json: ObjectRecord,
    stringify: ObjectRecord,
}

/// Reserved records for the ordinary `%Math%` object and installed prefix.
struct MathRecords {
    object: ObjectRecord,
    methods: [ObjectRecord; MathMethod::ALL.len()],
}

struct RealmBaseRecords {
    global: ObjectRecord,
    object_prototype: ObjectRecord,
    function_prototype: ObjectRecord,
    throw_type_error: ObjectRecord,
    function_constructor: ObjectRecord,
    object_constructor: ObjectRecord,
    object_statics: [ObjectRecord; OBJECT_STATIC_METHODS.len()],
    object_to_string: ObjectRecord,
    object_value_of: ObjectRecord,
    object_reflection: [ObjectRecord; OBJECT_PROTOTYPE_REFLECTION.len()],
    function_to_string: ObjectRecord,
    function_call: ObjectRecord,
    function_apply: ObjectRecord,
    function_bind: ObjectRecord,
    function_has_instance: ObjectRecord,
}

struct ErrorIntrinsicRecords {
    prototype: ObjectRecord,
    constructor: ObjectRecord,
}

struct ErrorRecords {
    entries: [ErrorIntrinsicRecords; ErrorIntrinsicKind::ALL.len()],
    to_string: ObjectRecord,
    is_error: ObjectRecord,
}

struct PrimitiveIntrinsicRecords {
    prototype: ObjectRecord,
    constructor: ObjectRecord,
    to_string: ObjectRecord,
    value_of: ObjectRecord,
}

/// Reserved records for the `BigInt` constructor, prototype, and methods.
struct BigIntIntrinsicRecords {
    prototype: ObjectRecord,
    constructor: ObjectRecord,
    to_string: ObjectRecord,
    value_of: ObjectRecord,
    as_int_n: ObjectRecord,
    as_uint_n: ObjectRecord,
}

impl BigIntIntrinsicRecords {
    /// Reserves the `BigInt` records in the realm transaction's order.
    ///
    /// `BigInt.prototype` holds `constructor`, `toString`, `toLocaleString`,
    /// `valueOf`, and `[Symbol.toStringTag]`; the constructor holds `prototype`,
    /// `length`, `name`, `asIntN`, and `asUintN`.
    fn try_new() -> Result<Self, RuntimeError> {
        Ok(Self {
            prototype: reserved_record(5)?,
            constructor: reserved_record(5)?,
            to_string: reserved_record(2)?,
            value_of: reserved_record(2)?,
            as_int_n: reserved_record(2)?,
            as_uint_n: reserved_record(2)?,
        })
    }
}

impl PrimitiveIntrinsicRecords {
    /// Reserves one primitive wrapper's records.
    ///
    /// `prototype_properties` differs per family because each prototype carries
    /// its own extra members beyond `constructor`, `toString`, and `valueOf`.
    fn try_new(prototype_properties: usize) -> Result<Self, RuntimeError> {
        Self::try_new_with_constructor(prototype_properties, 3)
    }

    /// Reserves records when the constructor also carries static members.
    fn try_new_with_constructor(
        prototype_properties: usize,
        constructor_properties: usize,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            prototype: reserved_record(prototype_properties)?,
            constructor: reserved_record(constructor_properties)?,
            to_string: reserved_record(2)?,
            value_of: reserved_record(2)?,
        })
    }
}

struct ArrayIntrinsicRecords {
    prototype: ObjectRecord,
    constructor: ObjectRecord,
    species: ObjectRecord,
    join: ObjectRecord,
    to_string: ObjectRecord,
}

struct IteratorIntrinsicRecords {
    iterator_prototype: ObjectRecord,
    iterator_method: ObjectRecord,
    array_iterator_prototype: ObjectRecord,
    array_iterator_next: ObjectRecord,
    array_values: ObjectRecord,
    array_keys: ObjectRecord,
    array_entries: ObjectRecord,
    string_iterator_prototype: ObjectRecord,
    string_iterator_next: ObjectRecord,
    string_iterator: ObjectRecord,
}

struct SymbolIntrinsicRecords {
    prototype: ObjectRecord,
    constructor: ObjectRecord,
    to_string: ObjectRecord,
    value_of: ObjectRecord,
    to_primitive: ObjectRecord,
    description: ObjectRecord,
    symbol_for: ObjectRecord,
    key_for: ObjectRecord,
}

struct RealmRecords {
    base: RealmBaseRecords,
    errors: ErrorRecords,
    boolean: PrimitiveIntrinsicRecords,
    number: PrimitiveIntrinsicRecords,
    bigint: BigIntIntrinsicRecords,
    string: PrimitiveIntrinsicRecords,
    string_methods: [ObjectRecord; STRING_PROTOTYPE_METHODS.len()],
    number_predicates: [ObjectRecord; NUMBER_PREDICATE_STATICS.len()],
    global_numeric_functions: [ObjectRecord; GLOBAL_NUMERIC_FUNCTIONS.len()],
    uri_functions: [ObjectRecord; URI_FUNCTIONS.len()],
    string_from_statics: [ObjectRecord; STRING_FROM_STATICS.len()],
    string_raw: ObjectRecord,
    array_searches: [ObjectRecord; ARRAY_SEARCH_METHODS.len()],
    array_mutators: [ObjectRecord; ARRAY_MUTATOR_METHODS.len()],
    array_copiers: [ObjectRecord; ARRAY_COPIER_TOTAL],
    array_sorts: [ObjectRecord; ARRAY_SORT_METHODS.len()],
    array_flattens: [ObjectRecord; ARRAY_FLATTEN_METHODS.len()],
    locale_strings: [ObjectRecord; LOCALE_STRING_METHODS.len()],
    number_formats: [ObjectRecord; NUMBER_FORMAT_METHODS.len()],
    array_callbacks: [ObjectRecord; ARRAY_CALLBACK_METHODS.len()],
    array_reductions: [ObjectRecord; ARRAY_REDUCTION_METHODS.len()],
    array_splice: ObjectRecord,
    array_is_array: ObjectRecord,
    array_statics: [ObjectRecord; ArrayStatic::ALL.len()],
    array: ArrayIntrinsicRecords,
    iterators: IteratorIntrinsicRecords,
    symbol: SymbolIntrinsicRecords,
    reflect: ReflectRecords,
    json: JsonRecords,
    math: MathRecords,
}

impl RealmRecords {
    #[expect(
        clippy::too_many_lines,
        reason = "one flat reservation site keeps every realm intrinsic's exact property budget auditable in a single place"
    )]
    fn try_new(length_key: &PropertyKey) -> Result<Self, RuntimeError> {
        // Keep these reservations in the original transaction order so a
        // recoverable allocation failure reports the same `additional` value.
        let base = RealmBaseRecords {
            global: reserved_record(31)?,
            object_prototype: reserved_record(4 + OBJECT_PROTOTYPE_REFLECTION.len())?,
            function_prototype: reserved_record(10)?,
            throw_type_error: reserved_record(2)?,
            function_constructor: reserved_record(3)?,
            object_constructor: reserved_record(3 + OBJECT_STATIC_METHODS.len())?,
            object_statics: object_static_records()?,
            object_to_string: reserved_record(2)?,
            object_value_of: reserved_record(2)?,
            object_reflection: object_reflection_records()?,
            function_to_string: reserved_record(2)?,
            function_call: reserved_record(2)?,
            function_apply: reserved_record(2)?,
            function_bind: reserved_record(2)?,
            function_has_instance: reserved_record(2)?,
        };
        let error_records = |prototype_properties, constructor_properties| {
            Ok::<_, RuntimeError>(ErrorIntrinsicRecords {
                prototype: reserved_record(prototype_properties)?,
                constructor: reserved_record(constructor_properties)?,
            })
        };
        let errors = ErrorRecords {
            entries: [
                error_records(4, 4)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
                error_records(3, 3)?,
            ],
            to_string: reserved_record(2)?,
            is_error: reserved_record(2)?,
        };
        let boolean = PrimitiveIntrinsicRecords::try_new(3)?;
        // The `Number` constructor additionally carries its value and predicate
        // statics plus the two parser aliases.
        let number = PrimitiveIntrinsicRecords::try_new_with_constructor(
            4 + NUMBER_FORMAT_METHODS.len(),
            3 + NUMBER_VALUE_STATICS.len()
                + NUMBER_PREDEFINED_VALUE_STATICS.len()
                + NUMBER_PREDICATE_STATICS.len()
                + 2,
        )?;
        let number_predicates = number_predicate_records()?;
        let global_numeric_functions = global_numeric_function_records()?;
        let uri_functions = uri_function_records()?;
        let bigint = BigIntIntrinsicRecords::try_new()?;
        // `String.prototype` additionally carries `length`, its iterator, and
        // every installed method.
        let string = PrimitiveIntrinsicRecords::try_new_with_constructor(
            5 + STRING_PROTOTYPE_METHODS.len(),
            3 + STRING_FROM_STATICS.len() + 1,
        )?;
        let string_methods = string_method_records()?;
        let string_from_statics = string_from_records()?;
        let mut array = ArrayIntrinsicRecords {
            prototype: reserved_record(
                8 + ARRAY_SEARCH_METHODS.len()
                    + ARRAY_MUTATOR_METHODS.len()
                    + ARRAY_COPIER_TOTAL
                    + ARRAY_SORT_METHODS.len()
                    + ARRAY_FLATTEN_METHODS.len()
                    + 1
                    + NUMBER_FORMAT_METHODS.len()
                    + ARRAY_CALLBACK_METHODS.len()
                    + ARRAY_REDUCTION_METHODS.len()
                    + 1,
            )?,
            // `length`, `name`, `isArray`, both generic factories,
            // `prototype`, and @@species.
            constructor: reserved_record(5 + ArrayStatic::ALL.len())?,
            species: reserved_record(2)?,
            join: reserved_record(2)?,
            to_string: reserved_record(2)?,
        };
        array
            .prototype
            .append_data(
                length_key.clone(),
                ARRAY_LENGTH_PROPERTY,
                StoredValue::Number(JsNumber::from_i32(0)),
            )
            .map_err(|_| property_allocation_failed(1))?;
        let iterators = IteratorIntrinsicRecords {
            iterator_prototype: reserved_record(1)?,
            iterator_method: reserved_record(2)?,
            array_iterator_prototype: reserved_record(2)?,
            array_iterator_next: reserved_record(2)?,
            array_values: reserved_record(2)?,
            array_keys: reserved_record(2)?,
            array_entries: reserved_record(2)?,
            string_iterator_prototype: reserved_record(2)?,
            string_iterator_next: reserved_record(2)?,
            string_iterator: reserved_record(2)?,
        };
        let symbol = SymbolIntrinsicRecords {
            prototype: reserved_record(6)?,
            constructor: reserved_record(18)?,
            to_string: reserved_record(2)?,
            value_of: reserved_record(2)?,
            to_primitive: reserved_record(2)?,
            description: reserved_record(2)?,
            symbol_for: reserved_record(2)?,
            key_for: reserved_record(2)?,
        };
        let reflect = ReflectRecords {
            // Thirteen methods plus @@toStringTag.
            object: reserved_record(ReflectMethod::ALL.len() + 1)?,
            methods: reflect_method_records()?,
        };
        let json = JsonRecords {
            // Four methods plus @@toStringTag.
            object: reserved_record(5)?,
            is_raw_json: reserved_record(2)?,
            parse: reserved_record(2)?,
            raw_json: reserved_record(2)?,
            stringify: reserved_record(2)?,
        };
        let math = MathRecords {
            // Every method and numeric constant plus @@toStringTag.
            object: reserved_record(MathMethod::ALL.len() + MATH_CONSTANTS.len() + 1)?,
            methods: math_method_records()?,
        };
        Ok(Self {
            base,
            errors,
            boolean,
            number,
            bigint,
            string,
            string_methods,
            number_predicates,
            global_numeric_functions,
            uri_functions,
            string_from_statics,
            string_raw: reserved_record(2)?,
            array_searches: array_search_records()?,
            array_mutators: array_mutator_records()?,
            array_copiers: array_copier_records()?,
            array_sorts: array_sort_records()?,
            array_flattens: array_flatten_records()?,
            locale_strings: locale_string_records()?,
            number_formats: number_format_records()?,
            array_callbacks: array_callback_records()?,
            array_reductions: array_reduction_records()?,
            array_splice: reserved_record(2)?,
            array_is_array: reserved_record(2)?,
            array_statics: array_static_records()?,
            array,
            iterators,
            symbol,
            reflect,
            json,
            math,
        })
    }
}

struct RealmBase {
    realm: RealmId,
    object_prototype: ObjectId,
    global_object: ObjectId,
    function_prototype: FunctionId,
    throw_type_error: FunctionId,
    function_constructor: FunctionId,
    object_constructor: FunctionId,
    object_statics: [FunctionId; OBJECT_STATIC_METHODS.len()],
    object_to_string: FunctionId,
    object_value_of: FunctionId,
    object_reflection: [FunctionId; OBJECT_PROTOTYPE_REFLECTION.len()],
    function_to_string: FunctionId,
    function_call: FunctionId,
    function_apply: FunctionId,
    function_bind: FunctionId,
    function_has_instance: FunctionId,
}

struct PrimitiveIntrinsicGraph {
    prototype: ObjectId,
    constructor: FunctionId,
    to_string: FunctionId,
    value_of: FunctionId,
}

#[derive(Clone, Copy)]
struct PrimitiveIntrinsicKinds {
    constructor: NativeFunctionKind,
    to_string: NativeFunctionKind,
    value_of: NativeFunctionKind,
}

#[derive(Clone, Copy)]
struct PrimitivePropertySpec<'a> {
    constructor_name: &'a JsString,
    to_string_length: i32,
    prototype_length: Option<i32>,
    defer_constructor_prototype: bool,
}

/// The inserted `BigInt` intrinsic identities.
struct BigIntIntrinsicGraph {
    prototype: ObjectId,
    constructor: FunctionId,
    to_string: FunctionId,
    value_of: FunctionId,
    as_int_n: FunctionId,
    as_uint_n: FunctionId,
}

struct ArrayIntrinsicGraph {
    prototype: ObjectId,
    constructor: FunctionId,
    species: FunctionId,
    join: FunctionId,
    to_string: FunctionId,
}

struct IteratorIntrinsicGraph {
    iterator_prototype: ObjectId,
    iterator_method: FunctionId,
    array_iterator_prototype: ObjectId,
    array_iterator_next: FunctionId,
    array_values: FunctionId,
    array_keys: FunctionId,
    array_entries: FunctionId,
    string_iterator_prototype: ObjectId,
    string_iterator_next: FunctionId,
    string_iterator: FunctionId,
}

struct SymbolIntrinsicGraph {
    prototype: ObjectId,
    constructor: FunctionId,
    to_string: FunctionId,
    value_of: FunctionId,
    to_primitive: FunctionId,
    description: FunctionId,
    symbol_for: FunctionId,
    key_for: FunctionId,
}

/// Inserted identities for the ordinary `%Reflect%` surface.
struct ReflectGraph {
    object: ObjectId,
    methods: [FunctionId; ReflectMethod::ALL.len()],
}

/// Inserted identities for the ordinary `%JSON%` object and its methods.
struct JsonGraph {
    object: ObjectId,
    is_raw_json: FunctionId,
    parse: FunctionId,
    raw_json: FunctionId,
    stringify: FunctionId,
}

/// Inserted identities for the ordinary `%Math%` object and method prefix.
struct MathGraph {
    object: ObjectId,
    methods: [FunctionId; MathMethod::ALL.len()],
}

struct RealmGraph {
    base: RealmBase,
    dynamic_atoms: RealmAtomBindings,
    errors: ErrorIntrinsics,
    boolean: PrimitiveIntrinsicGraph,
    number: PrimitiveIntrinsicGraph,
    bigint: BigIntIntrinsicGraph,
    string: PrimitiveIntrinsicGraph,
    string_methods: [FunctionId; STRING_PROTOTYPE_METHODS.len()],
    number_predicates: [FunctionId; NUMBER_PREDICATE_STATICS.len()],
    global_numeric_functions: [FunctionId; GLOBAL_NUMERIC_FUNCTIONS.len()],
    uri_functions: [FunctionId; URI_FUNCTIONS.len()],
    string_from_statics: [FunctionId; STRING_FROM_STATICS.len()],
    string_raw: FunctionId,
    array_searches: [FunctionId; ARRAY_SEARCH_METHODS.len()],
    array_mutators: [FunctionId; ARRAY_MUTATOR_METHODS.len()],
    array_copiers: [FunctionId; ARRAY_COPIER_TOTAL],
    array_sorts: [FunctionId; ARRAY_SORT_METHODS.len()],
    array_flattens: [FunctionId; ARRAY_FLATTEN_METHODS.len()],
    locale_strings: [FunctionId; LOCALE_STRING_METHODS.len()],
    number_formats: [FunctionId; NUMBER_FORMAT_METHODS.len()],
    array_callbacks: [FunctionId; ARRAY_CALLBACK_METHODS.len()],
    array_reductions: [FunctionId; ARRAY_REDUCTION_METHODS.len()],
    array_splice: FunctionId,
    array_is_array: FunctionId,
    array_statics: [FunctionId; ArrayStatic::ALL.len()],
    array: ArrayIntrinsicGraph,
    iterators: IteratorIntrinsicGraph,
    symbol: SymbolIntrinsicGraph,
    reflect: ReflectGraph,
    json: JsonGraph,
    math: MathGraph,
}

impl Runtime {
    /// Creates one bounded runtime and its predefined atom table.
    ///
    /// # Errors
    ///
    /// Returns a structured atom-table configuration or allocation error.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "Arc ownership is user-selected while Cell deliberately keeps this runtime local"
    )]
    pub fn try_new(limits: RuntimeLimits) -> Result<Self, RuntimeError> {
        let atoms = AtomTable::try_new(limits.atom_limits)?;
        let mailbox = Arc::new(ReleaseMailbox::new());
        let runtime_identity =
            RuntimeIdentity::from_address(Arc::as_ptr(&mailbox).cast::<()>() as usize);
        Ok(Self {
            mailbox,
            atoms,
            realms: Arena::new(runtime_identity),
            code: Arena::new(runtime_identity),
            functions: Arena::new(runtime_identity),
            objects: Arena::new(runtime_identity),
            cells: Arena::new(runtime_identity),
            global_bindings: Arena::new(runtime_identity),
            limits,
            installed_templates: 0,
            installed_atoms: 0,
            installed_constants: 0,
            object_properties: 0,
            for_in_entries: 0,
            public_roots: 0,
            collection_pending: false,
            interrupts: InterruptState::default(),
            next_math_random_seed: 1,
        })
    }

    /// Creates a realm owned by this runtime.
    ///
    /// # Errors
    ///
    /// Returns a limit or recoverable allocation failure.
    #[allow(
        clippy::arc_with_non_send_sync,
        clippy::missing_panics_doc,
        reason = "pre-reserved arena insertion failures are internal invariant violations"
    )]
    pub fn create_realm(&mut self) -> Result<Realm, RuntimeError> {
        self.drain_releases();
        let keys = RealmKeys::new(&self.atoms);
        let names = RealmNames::try_new(&self.atoms)?;
        let atom_plan = RealmAtomPlan::try_new(&names)?;
        let reservation = RealmReservationPlan::try_new(&atom_plan)?;
        reservation.preflight_and_reserve(self)?;
        let records = RealmRecords::try_new(&keys.length)?;
        let mut transaction = RealmBuildTransaction::try_new(self, reservation.journal_entries())?;
        let graph = transaction.build_realm_graph(records, &atom_plan)?;

        if transaction
            .publish_realm_properties(&graph, &keys, &names)
            .is_err()
        {
            return Err(property_allocation_failed(1));
        }

        let id = graph.base.realm;
        let state = transaction
            .realms
            .get_mut(id)
            .expect("new realm remains live");
        state.intrinsics = RealmIntrinsics::Ready {
            function_prototype: graph.base.function_prototype,
            throw_type_error: graph.base.throw_type_error,
            function_constructor: graph.base.function_constructor,
            errors: graph.errors,
            boolean: BooleanIntrinsics {
                prototype: graph.boolean.prototype,
                constructor: graph.boolean.constructor,
            },
            number: NumberIntrinsics {
                prototype: graph.number.prototype,
                constructor: graph.number.constructor,
            },
            bigint: BigIntIntrinsics {
                prototype: graph.bigint.prototype,
                constructor: graph.bigint.constructor,
            },
            string: StringIntrinsics {
                prototype: graph.string.prototype,
                constructor: graph.string.constructor,
            },
            array: ArrayIntrinsics {
                prototype: graph.array.prototype,
                constructor: graph.array.constructor,
            },
            symbol: SymbolIntrinsics {
                prototype: graph.symbol.prototype,
                constructor: graph.symbol.constructor,
            },
            iterators: IteratorIntrinsics {
                iterator_prototype: graph.iterators.iterator_prototype,
                array_iterator_prototype: graph.iterators.array_iterator_prototype,
                string_iterator_prototype: graph.iterators.string_iterator_prototype,
                array_values: graph.iterators.array_values,
            },
        };
        transaction.commit();
        let math_random_seed = transaction.next_math_random_seed;
        transaction.next_math_random_seed =
            transaction.next_math_random_seed.wrapping_add(1).max(1);
        transaction
            .realms
            .get_mut(id)
            .expect("committed Realm remains live")
            .math_random_state = math_random_seed;
        transaction.object_properties += reservation.object_properties();
        Ok(Realm(Arc::new(RealmHandle {
            owner: Arc::downgrade(&transaction.mailbox),
            id,
        })))
    }

    /// Advances the realm-local xorshift64* stream and maps its high 52 bits
    /// onto the uniformly spaced binary64 values in `[0, 1)`.
    pub(crate) fn math_random_number(
        &mut self,
        realm: RealmId,
    ) -> Result<JsNumber, crate::EngineFault> {
        let state = &mut self
            .realms
            .get_mut(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?
            .math_random_state;
        let mut value = *state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        *state = value;
        let output = value.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let bits = (0x3ff_u64 << 52) | (output >> 12);
        Ok(JsNumber::from_f64(f64::from_bits(bits) - 1.0))
    }
}

impl RealmBuildTransaction<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "one flat insertion transaction keeps the exact intrinsic order auditable"
    )]
    fn build_realm_graph(
        &mut self,
        records: RealmRecords,
        atom_plan: &RealmAtomPlan<'_>,
    ) -> Result<RealmGraph, RuntimeError> {
        let dynamic_atoms = self.intern_realm_atom_plan(atom_plan)?;
        self.record_atoms(&dynamic_atoms);
        let base = self.insert_realm_base(records.base);

        let errors = self.insert_error_intrinsics(&base, records.errors);
        let boolean = self.insert_primitive_intrinsics(
            &base,
            records.boolean,
            BoxedPrimitive::Boolean(false),
            PrimitiveIntrinsicKinds {
                constructor: NativeFunctionKind::BooleanConstructor,
                to_string: NativeFunctionKind::BooleanPrototypeToString,
                value_of: NativeFunctionKind::BooleanPrototypeValueOf,
            },
        );
        let number = self.insert_primitive_intrinsics(
            &base,
            records.number,
            BoxedPrimitive::Number(JsNumber::from_i32(0)),
            PrimitiveIntrinsicKinds {
                constructor: NativeFunctionKind::NumberConstructor,
                to_string: NativeFunctionKind::NumberPrototypeToString,
                value_of: NativeFunctionKind::NumberPrototypeValueOf,
            },
        );
        let string = self.insert_primitive_intrinsics(
            &base,
            records.string,
            BoxedPrimitive::String(JsString::empty()),
            PrimitiveIntrinsicKinds {
                constructor: NativeFunctionKind::StringConstructor,
                to_string: NativeFunctionKind::StringPrototypeToString,
                value_of: NativeFunctionKind::StringPrototypeValueOf,
            },
        );
        let bigint = self.insert_bigint_intrinsics(&base, records.bigint);
        let array = self.insert_array_intrinsics(&base, records.array);
        let iterators = self.insert_iterator_intrinsics(&base, records.iterators);
        let symbol = self.insert_symbol_intrinsics(&base, records.symbol);
        let reflect = self.insert_reflect_intrinsics(&base, records.reflect);
        let json = self.insert_json_intrinsics(&base, records.json);
        let math = self.insert_math_intrinsics(&base, records.math);
        let string_methods = self.insert_string_prototype_methods(&base, records.string_methods);
        let number_predicates = self.insert_number_predicates(&base, records.number_predicates);
        let global_numeric_functions =
            self.insert_global_numeric_functions(&base, records.global_numeric_functions);
        let uri_functions = self.insert_uri_functions(&base, records.uri_functions);
        let string_from_statics =
            self.insert_string_from_statics(&base, records.string_from_statics);
        let string_raw = self.insert_string_raw(&base, records.string_raw);
        let array_searches = self.insert_array_searches(&base, records.array_searches);
        let array_mutators = self.insert_array_mutators(&base, records.array_mutators);
        let array_copiers = self.insert_array_copiers(&base, records.array_copiers);
        let array_sorts = self.insert_array_sorts(&base, records.array_sorts);
        let array_flattens = self.insert_array_flattens(&base, records.array_flattens);
        let locale_strings = self.insert_locale_strings(&base, records.locale_strings);
        let number_formats = self.insert_number_formats(&base, records.number_formats);
        let array_callbacks = self.insert_array_callbacks(&base, records.array_callbacks);
        let array_reductions = self.insert_array_reductions(&base, records.array_reductions);
        let array_splice = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayPrototypeSplice,
            records.array_splice,
        );
        let array_is_array = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayIsArray,
            records.array_is_array,
        );
        let array_statics = self.insert_array_statics(&base, records.array_statics);

        Ok(RealmGraph {
            base,
            dynamic_atoms,
            errors,
            boolean,
            number,
            bigint,
            string,
            string_methods,
            number_predicates,
            global_numeric_functions,
            uri_functions,
            string_from_statics,
            string_raw,
            array_searches,
            array_mutators,
            array_copiers,
            array_sorts,
            array_flattens,
            locale_strings,
            number_formats,
            array_callbacks,
            array_reductions,
            array_splice,
            array_is_array,
            array_statics,
            array,
            iterators,
            symbol,
            reflect,
            json,
            math,
        })
    }

    /// Inserts one reserved native function per `Object` static method.
    ///
    /// The result keeps `OBJECT_STATIC_METHODS` order so the publication step
    /// can pair each function with its name and `length`.
    fn insert_object_statics(
        &mut self,
        realm: RealmId,
        function_prototype: FunctionId,
        records: [ObjectRecord; OBJECT_STATIC_METHODS.len()],
    ) -> [FunctionId; OBJECT_STATIC_METHODS.len()] {
        let mut inserted = [None; OBJECT_STATIC_METHODS.len()];
        for ((slot, method), record) in inserted.iter_mut().zip(OBJECT_STATIC_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                realm,
                HeapReference::Function(function_prototype),
                method.kind,
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Object static was inserted"))
    }

    /// Inserts `Array.from` and `Array.of` in specification property order.
    fn insert_array_statics(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ArrayStatic::ALL.len()],
    ) -> [FunctionId; ArrayStatic::ALL.len()] {
        let mut inserted = [None; ArrayStatic::ALL.len()];
        for ((slot, method), record) in inserted.iter_mut().zip(ArrayStatic::ALL).zip(records) {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayStatic(method),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array static was inserted"))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one flat insertion site keeps every base intrinsic's realm ownership and prototype edge auditable together"
    )]
    fn insert_realm_base(&mut self, mut records: RealmBaseRecords) -> RealmBase {
        let object_prototype =
            self.insert_reserved_object(HeapObject::ordinary(records.object_prototype));
        records
            .global
            .replace_prototype(Some(HeapReference::Object(object_prototype)));
        let global_object = self.insert_reserved_object(HeapObject::ordinary(records.global));
        let realm = self
            .realms
            .try_insert(RealmState {
                object_prototype,
                global_object,
                intrinsics: RealmIntrinsics::Initializing,
                global_bindings: HashMap::new(),
                math_random_state: 1,
            })
            .expect("the realm transaction reserved its realm slot");
        self.record_realm(realm);

        let function_prototype = self.insert_reserved_native(
            realm,
            HeapReference::Object(object_prototype),
            NativeFunctionKind::FunctionPrototype,
            records.function_prototype,
        );
        let throw_type_error = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::ThrowTypeError,
            records.throw_type_error,
        );
        let function_constructor = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::OrdinaryFunctionConstructor,
            records.function_constructor,
        );
        let object_constructor = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::ObjectConstructor,
            records.object_constructor,
        );
        let object_statics =
            self.insert_object_statics(realm, function_prototype, records.object_statics);
        let object_to_string = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::ObjectPrototypeToString,
            records.object_to_string,
        );
        let object_value_of = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::ObjectPrototypeValueOf,
            records.object_value_of,
        );
        let mut object_reflection = [None; OBJECT_PROTOTYPE_REFLECTION.len()];
        for ((slot, (_, kind, _)), record) in object_reflection
            .iter_mut()
            .zip(OBJECT_PROTOTYPE_REFLECTION)
            .zip(records.object_reflection)
        {
            *slot = Some(self.insert_reserved_native(
                realm,
                HeapReference::Function(function_prototype),
                kind,
                record,
            ));
        }
        let object_reflection = object_reflection
            .map(|slot| slot.expect("every Object reflection method was inserted"));
        let function_to_string = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::FunctionPrototypeToString,
            records.function_to_string,
        );
        let function_call = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::FunctionPrototypeCall,
            records.function_call,
        );
        let function_apply = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::FunctionPrototypeApply,
            records.function_apply,
        );
        let function_bind = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::FunctionPrototypeBind,
            records.function_bind,
        );
        let function_has_instance = self.insert_reserved_native(
            realm,
            HeapReference::Function(function_prototype),
            NativeFunctionKind::FunctionPrototypeHasInstance,
            records.function_has_instance,
        );
        RealmBase {
            realm,
            object_prototype,
            global_object,
            function_prototype,
            throw_type_error,
            function_constructor,
            object_constructor,
            object_statics,
            object_to_string,
            object_value_of,
            object_reflection,
            function_to_string,
            function_call,
            function_apply,
            function_bind,
            function_has_instance,
        }
    }

    fn insert_error_intrinsics(
        &mut self,
        base: &RealmBase,
        records: ErrorRecords,
    ) -> ErrorIntrinsics {
        let [
            error_records,
            eval_error_records,
            range_error_records,
            reference_error_records,
            syntax_error_records,
            type_error_records,
            uri_error_records,
            internal_error_records,
            aggregate_error_records,
        ] = records.entries;
        let error_prototype = self.insert_reserved_object_with_prototype(
            error_records.prototype,
            HeapReference::Object(base.object_prototype),
        );
        let error_constructor = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ErrorConstructor(ErrorIntrinsicKind::Error),
            error_records.constructor,
        );
        let to_string = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ErrorPrototypeToString,
            records.to_string,
        );
        let is_error = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ErrorIsError,
            records.is_error,
        );
        let insert_native =
            |runtime: &mut Self, kind: ErrorIntrinsicKind, records: ErrorIntrinsicRecords| {
                let prototype = runtime.insert_reserved_object_with_prototype(
                    records.prototype,
                    HeapReference::Object(error_prototype),
                );
                let constructor = runtime.insert_reserved_native(
                    base.realm,
                    HeapReference::Function(error_constructor),
                    NativeFunctionKind::ErrorConstructor(kind),
                    records.constructor,
                );
                ErrorIntrinsic {
                    prototype,
                    constructor,
                }
            };
        ErrorIntrinsics {
            entries: [
                ErrorIntrinsic {
                    prototype: error_prototype,
                    constructor: error_constructor,
                },
                insert_native(self, ErrorIntrinsicKind::EvalError, eval_error_records),
                insert_native(self, ErrorIntrinsicKind::RangeError, range_error_records),
                insert_native(
                    self,
                    ErrorIntrinsicKind::ReferenceError,
                    reference_error_records,
                ),
                insert_native(self, ErrorIntrinsicKind::SyntaxError, syntax_error_records),
                insert_native(self, ErrorIntrinsicKind::TypeError, type_error_records),
                insert_native(self, ErrorIntrinsicKind::UriError, uri_error_records),
                insert_native(
                    self,
                    ErrorIntrinsicKind::InternalError,
                    internal_error_records,
                ),
                insert_native(
                    self,
                    ErrorIntrinsicKind::AggregateError,
                    aggregate_error_records,
                ),
            ],
            to_string,
            is_error,
        }
    }

    fn insert_primitive_intrinsics(
        &mut self,
        base: &RealmBase,
        records: PrimitiveIntrinsicRecords,
        primitive: BoxedPrimitive,
        kinds: PrimitiveIntrinsicKinds,
    ) -> PrimitiveIntrinsicGraph {
        let prototype = self.insert_reserved_boxed_object(
            records.prototype,
            HeapReference::Object(base.object_prototype),
            primitive,
        );
        let constructor = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            kinds.constructor,
            records.constructor,
        );
        let to_string = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            kinds.to_string,
            records.to_string,
        );
        let value_of = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            kinds.value_of,
            records.value_of,
        );
        PrimitiveIntrinsicGraph {
            prototype,
            constructor,
            to_string,
            value_of,
        }
    }

    /// Inserts one native function per `Array.prototype` reduction.
    fn insert_array_reductions(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_REDUCTION_METHODS.len()],
    ) -> [FunctionId; ARRAY_REDUCTION_METHODS.len()] {
        let mut inserted = [None; ARRAY_REDUCTION_METHODS.len()];
        for ((slot, reduction), record) in inserted
            .iter_mut()
            .zip(ARRAY_REDUCTION_METHODS)
            .zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeReduction(reduction),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array reduction function was inserted"))
    }

    /// Inserts one native function per `Array.prototype` callback method.
    fn insert_array_callbacks(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_CALLBACK_METHODS.len()],
    ) -> [FunctionId; ARRAY_CALLBACK_METHODS.len()] {
        let mut inserted = [None; ARRAY_CALLBACK_METHODS.len()];
        for ((slot, method), record) in inserted.iter_mut().zip(ARRAY_CALLBACK_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeCallback(method),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array callback function was inserted"))
    }

    /// Inserts one native function per `Number.prototype` decimal rendering.
    fn insert_number_formats(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; NUMBER_FORMAT_METHODS.len()],
    ) -> [FunctionId; NUMBER_FORMAT_METHODS.len()] {
        let mut inserted = [None; NUMBER_FORMAT_METHODS.len()];
        for ((slot, format), record) in inserted.iter_mut().zip(NUMBER_FORMAT_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::NumberPrototypeFormat(format),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Number format function was inserted"))
    }

    /// Inserts one native function per `Array.prototype` copying method.
    fn insert_array_copiers(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_COPIER_TOTAL],
    ) -> [FunctionId; ARRAY_COPIER_TOTAL] {
        let mut inserted = [None; ARRAY_COPIER_TOTAL];
        // The interned names come first, then the predefined one, which is the
        // order the publication step walks.
        let copiers = ARRAY_COPIER_METHODS.into_iter().chain(
            ARRAY_PREDEFINED_COPIERS
                .into_iter()
                .map(|(_, copier)| copier),
        );
        for ((slot, copier), record) in inserted.iter_mut().zip(copiers).zip(records) {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeCopier(copier),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array copier function was inserted"))
    }

    /// Inserts one native function per `Array.prototype` mutator.
    fn insert_array_mutators(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_MUTATOR_METHODS.len()],
    ) -> [FunctionId; ARRAY_MUTATOR_METHODS.len()] {
        let mut inserted = [None; ARRAY_MUTATOR_METHODS.len()];
        for ((slot, mutator), record) in inserted.iter_mut().zip(ARRAY_MUTATOR_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeMutator(mutator),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array mutator function was inserted"))
    }

    /// Inserts one native function per `SortIndexedProperties` method.
    fn insert_array_sorts(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_SORT_METHODS.len()],
    ) -> [FunctionId; ARRAY_SORT_METHODS.len()] {
        let mut inserted = [None; ARRAY_SORT_METHODS.len()];
        for ((slot, method), record) in inserted.iter_mut().zip(ARRAY_SORT_METHODS).zip(records) {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeSort(method),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array sort function was inserted"))
    }

    /// Inserts one native function per `FlattenIntoArray` method.
    fn insert_array_flattens(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_FLATTEN_METHODS.len()],
    ) -> [FunctionId; ARRAY_FLATTEN_METHODS.len()] {
        let mut inserted = [None; ARRAY_FLATTEN_METHODS.len()];
        for ((slot, method), record) in inserted.iter_mut().zip(ARRAY_FLATTEN_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeFlatten(method),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array flatten function was inserted"))
    }

    /// Inserts the no-`Intl` locale-string methods.
    fn insert_locale_strings(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; LOCALE_STRING_METHODS.len()],
    ) -> [FunctionId; LOCALE_STRING_METHODS.len()] {
        let mut inserted = [None; LOCALE_STRING_METHODS.len()];
        for ((slot, method), record) in inserted.iter_mut().zip(LOCALE_STRING_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::LocaleString(method),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every locale-string function was inserted"))
    }

    /// Inserts one native function per `Array.prototype` search.
    fn insert_array_searches(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; ARRAY_SEARCH_METHODS.len()],
    ) -> [FunctionId; ARRAY_SEARCH_METHODS.len()] {
        let mut inserted = [None; ARRAY_SEARCH_METHODS.len()];
        for ((slot, (_, search)), record) in
            inserted.iter_mut().zip(ARRAY_SEARCH_METHODS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::ArrayPrototypeSearch(search),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Array search function was inserted"))
    }

    /// Inserts one native function per `String` code-unit factory.
    fn insert_string_from_statics(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; STRING_FROM_STATICS.len()],
    ) -> [FunctionId; STRING_FROM_STATICS.len()] {
        let mut inserted = [None; STRING_FROM_STATICS.len()];
        for ((slot, (_, kind)), record) in inserted.iter_mut().zip(STRING_FROM_STATICS).zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::StringPrototypeMethod(kind),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every String factory function was inserted"))
    }

    fn insert_string_raw(&mut self, base: &RealmBase, record: ObjectRecord) -> FunctionId {
        self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::StringRaw,
            record,
        )
    }

    /// Inserts one native function per `Number` predicate static.
    fn insert_number_predicates(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; NUMBER_PREDICATE_STATICS.len()],
    ) -> [FunctionId; NUMBER_PREDICATE_STATICS.len()] {
        let mut inserted = [None; NUMBER_PREDICATE_STATICS.len()];
        for ((slot, (_, predicate)), record) in inserted
            .iter_mut()
            .zip(NUMBER_PREDICATE_STATICS)
            .zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::NumberPredicateStatic(predicate),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every Number predicate function was inserted"))
    }

    /// Inserts the realm-owned coercing numeric globals.
    fn insert_global_numeric_functions(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; GLOBAL_NUMERIC_FUNCTIONS.len()],
    ) -> [FunctionId; GLOBAL_NUMERIC_FUNCTIONS.len()] {
        let mut inserted = [None; GLOBAL_NUMERIC_FUNCTIONS.len()];
        for ((slot, (kind, _)), record) in inserted
            .iter_mut()
            .zip(GLOBAL_NUMERIC_FUNCTIONS)
            .zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::GlobalNumeric(kind),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every global numeric function was inserted"))
    }

    /// Inserts the four realm-owned URI handling functions.
    fn insert_uri_functions(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; URI_FUNCTIONS.len()],
    ) -> [FunctionId; URI_FUNCTIONS.len()] {
        let mut inserted = [None; URI_FUNCTIONS.len()];
        for ((slot, (_, kind)), record) in inserted.iter_mut().zip(URI_FUNCTIONS).zip(records) {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::GlobalUri(kind),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every URI function was inserted"))
    }

    /// Inserts one native function per installed `String.prototype` method.
    ///
    /// The result keeps `STRING_PROTOTYPE_METHODS` order so the publication step
    /// can zip the two together.
    fn insert_string_prototype_methods(
        &mut self,
        base: &RealmBase,
        records: [ObjectRecord; STRING_PROTOTYPE_METHODS.len()],
    ) -> [FunctionId; STRING_PROTOTYPE_METHODS.len()] {
        let mut inserted = [None; STRING_PROTOTYPE_METHODS.len()];
        for ((slot, method), record) in inserted
            .iter_mut()
            .zip(STRING_PROTOTYPE_METHODS)
            .zip(records)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::StringPrototypeMethod(method.method),
                record,
            ));
        }
        inserted.map(|slot| slot.expect("every String method function was inserted"))
    }

    /// Inserts the `BigInt` constructor, prototype, and methods.
    ///
    /// `BigInt.prototype` is an ordinary object, not a wrapper: it carries no
    /// `[[BigIntData]]`, which is why `BigInt.prototype.valueOf()` throws
    /// instead of returning `0n` (`quickjs.c:56014-56027`).
    fn insert_bigint_intrinsics(
        &mut self,
        base: &RealmBase,
        mut records: BigIntIntrinsicRecords,
    ) -> BigIntIntrinsicGraph {
        records
            .prototype
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let prototype = self.insert_reserved_object(HeapObject::ordinary(records.prototype));
        let constructor = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::BigIntConstructor,
            records.constructor,
        );
        let to_string = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::BigIntPrototypeToString,
            records.to_string,
        );
        let value_of = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::BigIntPrototypeValueOf,
            records.value_of,
        );
        let signed_truncation = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::BigIntAsIntN,
            records.as_int_n,
        );
        let unsigned_truncation = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::BigIntAsUintN,
            records.as_uint_n,
        );
        BigIntIntrinsicGraph {
            prototype,
            constructor,
            to_string,
            value_of,
            as_int_n: signed_truncation,
            as_uint_n: unsigned_truncation,
        }
    }

    fn insert_array_intrinsics(
        &mut self,
        base: &RealmBase,
        mut records: ArrayIntrinsicRecords,
    ) -> ArrayIntrinsicGraph {
        records
            .prototype
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let prototype =
            self.insert_reserved_object(HeapObject::array(records.prototype, ArrayState::new(0)));
        let constructor = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayConstructor,
            records.constructor,
        );
        let species = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArraySpeciesGetter,
            records.species,
        );
        let join = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayPrototypeJoin,
            records.join,
        );
        let to_string = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayPrototypeToString,
            records.to_string,
        );
        ArrayIntrinsicGraph {
            prototype,
            constructor,
            species,
            join,
            to_string,
        }
    }

    fn insert_iterator_intrinsics(
        &mut self,
        base: &RealmBase,
        mut records: IteratorIntrinsicRecords,
    ) -> IteratorIntrinsicGraph {
        records
            .iterator_prototype
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let iterator_prototype =
            self.insert_reserved_object(HeapObject::ordinary(records.iterator_prototype));
        let iterator_method = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::IteratorPrototypeIterator,
            records.iterator_method,
        );

        records
            .array_iterator_prototype
            .replace_prototype(Some(HeapReference::Object(iterator_prototype)));
        let array_iterator_prototype =
            self.insert_reserved_object(HeapObject::ordinary(records.array_iterator_prototype));
        let array_iterator_next = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayIteratorNext,
            records.array_iterator_next,
        );
        let array_values = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayPrototypeValues,
            records.array_values,
        );
        let array_keys = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayPrototypeKeys,
            records.array_keys,
        );
        let array_entries = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::ArrayPrototypeEntries,
            records.array_entries,
        );

        records
            .string_iterator_prototype
            .replace_prototype(Some(HeapReference::Object(iterator_prototype)));
        let string_iterator_prototype =
            self.insert_reserved_object(HeapObject::ordinary(records.string_iterator_prototype));
        let string_iterator_next = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::StringIteratorNext,
            records.string_iterator_next,
        );
        let string_iterator = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::StringPrototypeIterator,
            records.string_iterator,
        );
        IteratorIntrinsicGraph {
            iterator_prototype,
            iterator_method,
            array_iterator_prototype,
            array_iterator_next,
            array_values,
            array_keys,
            array_entries,
            string_iterator_prototype,
            string_iterator_next,
            string_iterator,
        }
    }

    fn insert_symbol_intrinsics(
        &mut self,
        base: &RealmBase,
        mut records: SymbolIntrinsicRecords,
    ) -> SymbolIntrinsicGraph {
        records
            .prototype
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let prototype = self.insert_reserved_object(HeapObject::ordinary(records.prototype));
        let make = |runtime: &mut Self, kind, record| {
            runtime.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                kind,
                record,
            )
        };
        let constructor = make(
            self,
            NativeFunctionKind::SymbolConstructor,
            records.constructor,
        );
        let to_string = make(
            self,
            NativeFunctionKind::SymbolPrototypeToString,
            records.to_string,
        );
        let value_of = make(
            self,
            NativeFunctionKind::SymbolPrototypeValueOf,
            records.value_of,
        );
        let to_primitive = make(
            self,
            NativeFunctionKind::SymbolPrototypeToPrimitive,
            records.to_primitive,
        );
        let description = make(
            self,
            NativeFunctionKind::SymbolPrototypeDescription,
            records.description,
        );
        let symbol_for = make(self, NativeFunctionKind::SymbolFor, records.symbol_for);
        let key_for = make(self, NativeFunctionKind::SymbolKeyFor, records.key_for);
        SymbolIntrinsicGraph {
            prototype,
            constructor,
            to_string,
            value_of,
            to_primitive,
            description,
            symbol_for,
            key_for,
        }
    }

    /// Inserts the ordinary `%Reflect%` object and its thirteen methods.
    fn insert_reflect_intrinsics(
        &mut self,
        base: &RealmBase,
        mut records: ReflectRecords,
    ) -> ReflectGraph {
        records
            .object
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let object = self.insert_reserved_object(HeapObject::ordinary(records.object));
        let mut methods = [None; ReflectMethod::ALL.len()];
        for ((slot, method), record) in methods
            .iter_mut()
            .zip(ReflectMethod::ALL)
            .zip(records.methods)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::Reflect(method),
                record,
            ));
        }
        ReflectGraph {
            object,
            methods: methods.map(|slot| slot.expect("every Reflect method was inserted")),
        }
    }

    /// Inserts the ordinary `%JSON%` object and its currently complete methods.
    fn insert_json_intrinsics(&mut self, base: &RealmBase, mut records: JsonRecords) -> JsonGraph {
        records
            .object
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let object = self.insert_reserved_object(HeapObject::ordinary(records.object));
        let parse = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::JsonParse,
            records.parse,
        );
        let is_raw_json = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::JsonIsRawJson,
            records.is_raw_json,
        );
        let raw_json = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::JsonRawJson,
            records.raw_json,
        );
        let stringify = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::JsonStringify,
            records.stringify,
        );
        JsonGraph {
            object,
            is_raw_json,
            parse,
            raw_json,
            stringify,
        }
    }

    /// Inserts the ordinary `%Math%` object and its specification-order prefix.
    fn insert_math_intrinsics(&mut self, base: &RealmBase, mut records: MathRecords) -> MathGraph {
        records
            .object
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let object = self.insert_reserved_object(HeapObject::ordinary(records.object));
        let mut methods = [None; MathMethod::ALL.len()];
        for ((slot, method), record) in methods.iter_mut().zip(MathMethod::ALL).zip(records.methods)
        {
            *slot = Some(self.insert_reserved_native(
                base.realm,
                HeapReference::Function(base.function_prototype),
                NativeFunctionKind::Math(method),
                record,
            ));
        }
        MathGraph {
            object,
            methods: methods.map(|slot| slot.expect("every Math method was inserted")),
        }
    }

    fn insert_reserved_native(
        &mut self,
        realm: RealmId,
        prototype: HeapReference,
        kind: NativeFunctionKind,
        mut object: ObjectRecord,
    ) -> FunctionId {
        object.replace_prototype(Some(prototype));
        let function = self
            .functions
            .try_insert(HeapFunction {
                implementation: FunctionImplementation::Native(NativeFunction { realm, kind }),
                object,
                public_roots: 0,
            })
            .expect("the realm transaction reserved all intrinsic function slots");
        self.record_function(function);
        function
    }

    fn insert_reserved_object(&mut self, object: HeapObject) -> ObjectId {
        let object = self
            .objects
            .try_insert(object)
            .expect("the realm transaction reserved all intrinsic object slots");
        self.record_object(object);
        object
    }

    fn insert_reserved_object_with_prototype(
        &mut self,
        mut record: ObjectRecord,
        prototype: HeapReference,
    ) -> ObjectId {
        record.replace_prototype(Some(prototype));
        self.insert_reserved_object(HeapObject::ordinary(record))
    }

    fn insert_reserved_boxed_object(
        &mut self,
        mut record: ObjectRecord,
        prototype: HeapReference,
        primitive: BoxedPrimitive,
    ) -> ObjectId {
        record.replace_prototype(Some(prototype));
        self.insert_reserved_object(HeapObject::with_boxed_primitive(record, primitive))
    }
}

impl Runtime {
    fn publish_realm_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        self.append_object_methods(
            graph.base.object_prototype,
            [
                (&keys.to_string, graph.base.object_to_string),
                (&keys.value_of, graph.base.object_value_of),
            ],
        )?;
        self.publish_object_reflection_methods(graph, keys)?;
        self.publish_error_intrinsic_properties(graph, keys, names)?;
        self.publish_function_intrinsic_properties(graph, keys, names)?;
        self.publish_primitive_intrinsic_properties(
            &graph.boolean,
            PrimitivePropertySpec {
                constructor_name: &names.boolean,
                to_string_length: 0,
                prototype_length: None,
                defer_constructor_prototype: false,
            },
            keys,
            names,
        )?;
        self.publish_primitive_intrinsic_properties(
            &graph.number,
            PrimitivePropertySpec {
                constructor_name: &names.number,
                to_string_length: 1,
                prototype_length: None,
                defer_constructor_prototype: false,
            },
            keys,
            names,
        )?;
        self.publish_primitive_intrinsic_properties(
            &graph.string,
            PrimitivePropertySpec {
                constructor_name: &names.string,
                to_string_length: 0,
                prototype_length: Some(0),
                defer_constructor_prototype: true,
            },
            keys,
            names,
        )?;
        self.publish_array_constructor_identity(&graph.array, keys, names)?;
        self.publish_string_prototype_methods(graph, keys)?;
        self.publish_number_statics(graph, keys)?;
        self.publish_string_raw_and_constructor_prototype(graph, keys)?;
        self.publish_bigint_intrinsic_properties(&graph.bigint, &graph.dynamic_atoms, keys, names)?;
        self.publish_array_intrinsic_properties(&graph.array, keys, names)?;
        self.publish_locale_string_methods(graph, keys)?;
        self.publish_iterator_intrinsic_properties(&graph.iterators, graph, keys, names)?;
        self.publish_global_value_properties(graph)?;
        self.publish_global_numeric_functions(graph, keys)?;
        self.publish_uri_functions(graph, keys)?;
        self.publish_symbol_intrinsic_properties(&graph.symbol, graph, keys, names)?;
        self.publish_reflect_intrinsic_properties(graph, keys, names)?;
        self.publish_json_intrinsic_properties(graph, keys, names)?;
        self.publish_math_intrinsic_properties(graph, keys, names)?;
        self.append_object_methods(
            graph.base.global_object,
            [
                (&keys.function, graph.base.function_constructor),
                (&keys.object, graph.base.object_constructor),
                (&keys.boolean, graph.boolean.constructor),
                (&keys.number, graph.number.constructor),
                (&keys.bigint, graph.bigint.constructor),
                (&keys.string, graph.string.constructor),
                (&keys.array, graph.array.constructor),
                (&keys.symbol, graph.symbol.constructor),
            ],
        )
    }

    /// Publishes deterministic no-`Intl` `toLocaleString` methods.
    fn publish_locale_string_methods(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        let key = self.predefined_property_key(PredefinedAtom::ToLocaleString);
        let name = predefined_string(&self.atoms, PredefinedAtom::ToLocaleString);
        let targets = [
            graph.base.object_prototype,
            graph.number.prototype,
            graph.bigint.prototype,
            graph.array.prototype,
        ];
        for (target, function) in targets.into_iter().zip(graph.locale_strings) {
            self.objects
                .get_mut(target)
                .expect("locale-string prototype remains live")
                .record
                .append_data(
                    key.clone(),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 0, keys)?;
        }
        Ok(())
    }

    /// Publishes the specification-defined ordinary `%Reflect%` shape.
    fn publish_reflect_intrinsic_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        let reflect_key = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::Reflect).clone(),
        );
        let method_keys =
            ReflectMethod::ALL.map(|method| self.predefined_property_key(method.predefined_atom()));
        {
            let record = &mut self
                .objects
                .get_mut(graph.reflect.object)
                .expect("new Reflect object remains live")
                .record;
            for (key, function) in method_keys.into_iter().zip(graph.reflect.methods) {
                record.append_data(key, METHOD_PROPERTY, StoredValue::Function(function))?;
            }
            record.append_data(
                keys.symbol_to_string_tag.clone(),
                IDENTITY_PROPERTY,
                StoredValue::String(names.reflect.clone()),
            )?;
        }
        for (method, function) in ReflectMethod::ALL.into_iter().zip(graph.reflect.methods) {
            let name = predefined_string(&self.atoms, method.predefined_atom());
            self.append_function_identity(function, &name, method.length(), keys)?;
        }
        self.objects
            .get_mut(graph.base.global_object)
            .expect("new realm global object remains live")
            .record
            .append_data(
                reflect_key,
                METHOD_PROPERTY,
                StoredValue::Object(graph.reflect.object),
            )
    }

    /// Publishes `%JSON%`, its currently complete methods, and its specification tag.
    fn publish_json_intrinsic_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        let is_raw_json_key = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::JsonIsRawJson).clone(),
        );
        let parse_key = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::JsonParse).clone(),
        );
        let raw_json_key = self.predefined_property_key(PredefinedAtom::RawJson);
        let stringify_key = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::JsonStringify).clone(),
        );
        let json = self
            .objects
            .get_mut(graph.json.object)
            .expect("new JSON object remains live");
        json.record.append_data(
            is_raw_json_key,
            METHOD_PROPERTY,
            StoredValue::Function(graph.json.is_raw_json),
        )?;
        json.record.append_data(
            parse_key,
            METHOD_PROPERTY,
            StoredValue::Function(graph.json.parse),
        )?;
        json.record.append_data(
            raw_json_key,
            METHOD_PROPERTY,
            StoredValue::Function(graph.json.raw_json),
        )?;
        json.record.append_data(
            stringify_key,
            METHOD_PROPERTY,
            StoredValue::Function(graph.json.stringify),
        )?;
        json.record.append_data(
            keys.symbol_to_string_tag.clone(),
            IDENTITY_PROPERTY,
            StoredValue::String(names.json.clone()),
        )?;
        self.append_function_identity(graph.json.is_raw_json, &names.is_raw_json, 1, keys)?;
        self.append_function_identity(graph.json.parse, &names.parse, 2, keys)?;
        self.append_function_identity(graph.json.raw_json, &names.raw_json, 1, keys)?;
        self.append_function_identity(graph.json.stringify, &names.stringify, 3, keys)?;
        let json_key = self.predefined_property_key(PredefinedAtom::Json);
        self.objects
            .get_mut(graph.base.global_object)
            .expect("new realm global object remains live")
            .record
            .append_data(
                json_key,
                METHOD_PROPERTY,
                StoredValue::Object(graph.json.object),
            )
    }

    /// Publishes the complete ordinary `%Math%` shape.
    fn publish_math_intrinsic_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        {
            let record = &mut self
                .objects
                .get_mut(graph.math.object)
                .expect("new Math object remains live")
                .record;
            for (method, function) in MathMethod::ALL.into_iter().zip(graph.math.methods) {
                record.append_data(
                    PropertyKey::from_validated_atom(
                        graph
                            .dynamic_atoms
                            .atom(RealmNameId::MathMethod(method))
                            .clone(),
                    ),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            }
            for (name, bits) in MATH_CONSTANTS {
                record.append_data(
                    PropertyKey::from_validated_atom(
                        graph
                            .dynamic_atoms
                            .atom(RealmNameId::MathConstant(name))
                            .clone(),
                    ),
                    FROZEN_PROPERTY,
                    StoredValue::Number(JsNumber::from_f64(f64::from_bits(bits))),
                )?;
            }
            record.append_data(
                keys.symbol_to_string_tag.clone(),
                IDENTITY_PROPERTY,
                StoredValue::String(names.math.clone()),
            )?;
        }
        for (method, function) in MathMethod::ALL.into_iter().zip(graph.math.methods) {
            let atom = graph.dynamic_atoms.atom(RealmNameId::MathMethod(method));
            let name = atom
                .description()
                .expect("interned Math method name has a description")
                .clone();
            self.append_function_identity(function, &name, method.length(), keys)?;
        }
        let math_key = self.predefined_property_key(PredefinedAtom::Math);
        self.objects
            .get_mut(graph.base.global_object)
            .expect("new realm global object remains live")
            .record
            .append_data(
                math_key,
                METHOD_PROPERTY,
                StoredValue::Object(graph.math.object),
            )
    }

    /// Installs the pinned global value properties and `globalThis`.
    ///
    /// `undefined`, `NaN`, and `Infinity` are frozen data properties.
    /// `globalThis` is writable and configurable, and its value is the realm's
    /// actual global object. The compiler lowers these names as constructor-
    /// realm global references, so reads resolve through the global object
    /// exactly like any other realm-global binding.
    fn publish_global_value_properties(
        &mut self,
        graph: &RealmGraph,
    ) -> Result<(), TryReserveError> {
        let undefined_key = self.predefined_property_key(PredefinedAtom::Undefined);
        let nan_key = self.predefined_property_key(PredefinedAtom::Nan);
        let infinity_key = self.predefined_property_key(PredefinedAtom::Infinity);
        let global_this_key = self.predefined_property_key(PredefinedAtom::GlobalThis);
        let record = &mut self
            .objects
            .get_mut(graph.base.global_object)
            .expect("new realm global object remains live")
            .record;
        let frozen = FROZEN_PROPERTY;
        record.append_data(undefined_key, frozen, StoredValue::Undefined)?;
        record.append_data(
            nan_key,
            frozen,
            StoredValue::Number(JsNumber::from_f64(f64::NAN)),
        )?;
        record.append_data(
            infinity_key,
            frozen,
            StoredValue::Number(JsNumber::from_f64(f64::INFINITY)),
        )?;
        record.append_data(
            global_this_key,
            METHOD_PROPERTY,
            StoredValue::Object(graph.base.global_object),
        )
    }

    /// Publishes the four coercing global numeric functions and the two parser
    /// aliases on `Number`.
    ///
    /// `isFinite` and `isNaN` reuse the atoms already interned for their
    /// non-coercing `Number` counterparts. `parseFloat` and `parseInt` share
    /// both their property keys and function identities with the `Number`
    /// aliases, so strict equality observes the specification-required alias.
    fn publish_global_numeric_functions(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        for ((kind, length), function) in GLOBAL_NUMERIC_FUNCTIONS
            .into_iter()
            .zip(graph.global_numeric_functions)
        {
            let atom_id = match kind {
                GlobalNumericFunction::IsFinite => {
                    RealmNameId::NumberPredicate(NumberPredicate::IsFinite)
                }
                GlobalNumericFunction::IsNaN => {
                    RealmNameId::NumberPredicate(NumberPredicate::IsNaN)
                }
                GlobalNumericFunction::ParseFloat => RealmNameId::ParseFloat,
                GlobalNumericFunction::ParseInt => RealmNameId::ParseInt,
            };
            let atom = graph.dynamic_atoms.atom(atom_id).clone();
            let name = atom
                .description()
                .expect("global numeric function name has a description")
                .clone();
            let key = PropertyKey::from_validated_atom(atom);
            self.objects
                .get_mut(graph.base.global_object)
                .expect("new realm global object remains live")
                .record
                .append_data(
                    key.clone(),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            if matches!(
                kind,
                GlobalNumericFunction::ParseFloat | GlobalNumericFunction::ParseInt
            ) {
                self.functions
                    .get_mut(graph.number.constructor)
                    .expect("new Number constructor remains live")
                    .object
                    .append_data(key, METHOD_PROPERTY, StoredValue::Function(function))?;
            }
            self.append_function_identity(function, &name, length, keys)?;
        }
        Ok(())
    }

    /// Publishes the four ordinary URI handling methods on the global object.
    fn publish_uri_functions(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        for ((_, kind), function) in URI_FUNCTIONS.into_iter().zip(graph.uri_functions) {
            let atom = graph.dynamic_atoms.atom(RealmNameId::Uri(kind)).clone();
            let name = atom
                .description()
                .expect("URI function name has a description")
                .clone();
            self.objects
                .get_mut(graph.base.global_object)
                .expect("new realm global object remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 1, keys)?;
        }
        Ok(())
    }

    fn publish_error_intrinsic_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        let errors = graph.errors;
        for kind in ErrorIntrinsicKind::ALL {
            let intrinsic = errors.intrinsic(kind);
            let record = &mut self
                .objects
                .get_mut(intrinsic.prototype)
                .expect("new Error prototype remains live")
                .record;
            if kind == ErrorIntrinsicKind::Error {
                record.append_data(
                    keys.to_string.clone(),
                    METHOD_PROPERTY,
                    StoredValue::Function(errors.to_string),
                )?;
            }
            record.append_data(
                keys.name.clone(),
                METHOD_PROPERTY,
                StoredValue::String(names.errors[kind.index()].clone()),
            )?;
            record.append_data(
                keys.message.clone(),
                METHOD_PROPERTY,
                StoredValue::String(JsString::empty()),
            )?;
            record.append_data(
                keys.constructor.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(intrinsic.constructor),
            )?;
        }

        self.append_function_identity(errors.to_string, &names.to_string, 0, keys)?;
        self.append_function_identity(errors.is_error, &names.is_error, 1, keys)?;

        let is_error_key = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::IsError).clone(),
        );
        for kind in ErrorIntrinsicKind::ALL {
            let intrinsic = errors.intrinsic(kind);
            let length = if kind == ErrorIntrinsicKind::AggregateError {
                2
            } else {
                1
            };
            self.append_function_identity(
                intrinsic.constructor,
                &names.errors[kind.index()],
                length,
                keys,
            )?;
            let constructor = &mut self
                .functions
                .get_mut(intrinsic.constructor)
                .expect("new Error constructor remains live")
                .object;
            if kind == ErrorIntrinsicKind::Error {
                constructor.append_data(
                    is_error_key.clone(),
                    METHOD_PROPERTY,
                    StoredValue::Function(errors.is_error),
                )?;
            }
            constructor.append_data(
                keys.prototype.clone(),
                CONSTRUCTOR_PROTOTYPE_PROPERTY,
                StoredValue::Object(intrinsic.prototype),
            )?;
        }

        self.append_object_methods::<9>(
            graph.base.global_object,
            std::array::from_fn(|index| (&keys.errors[index], errors.entries[index].constructor)),
        )
    }

    fn publish_function_intrinsic_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        {
            let record = &mut self
                .functions
                .get_mut(graph.base.throw_type_error)
                .expect("new %ThrowTypeError% remains live")
                .object;
            record.append_data(
                keys.length.clone(),
                FROZEN_PROPERTY,
                StoredValue::Number(JsNumber::from_i32(0)),
            )?;
            record.append_data(
                keys.name.clone(),
                FROZEN_PROPERTY,
                StoredValue::String(names.empty.clone()),
            )?;
            record.prevent_extensions();
        }

        {
            let record = &mut self
                .functions
                .get_mut(graph.base.function_prototype)
                .expect("new Function.prototype remains live")
                .object;
            record.append_data(
                keys.length.clone(),
                IDENTITY_PROPERTY,
                StoredValue::Number(JsNumber::from_i32(0)),
            )?;
            record.append_data(
                keys.name.clone(),
                IDENTITY_PROPERTY,
                StoredValue::String(names.empty.clone()),
            )?;
            record.append_accessor(
                keys.caller.clone(),
                PropertyLayout::accessor(false, true),
                Some(graph.base.throw_type_error),
                Some(graph.base.throw_type_error),
            )?;
            record.append_accessor(
                keys.arguments.clone(),
                PropertyLayout::accessor(false, true),
                Some(graph.base.throw_type_error),
                Some(graph.base.throw_type_error),
            )?;
            record.append_data(
                PropertyKey::from_validated_atom(
                    graph.dynamic_atoms.atom(RealmNameId::Call).clone(),
                ),
                METHOD_PROPERTY,
                StoredValue::Function(graph.base.function_call),
            )?;
            record.append_data(
                keys.apply.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.base.function_apply),
            )?;
            record.append_data(
                PropertyKey::from_validated_atom(
                    graph.dynamic_atoms.atom(RealmNameId::Bind).clone(),
                ),
                METHOD_PROPERTY,
                StoredValue::Function(graph.base.function_bind),
            )?;
            record.append_data(
                keys.to_string.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.base.function_to_string),
            )?;
            record.append_data(
                keys.constructor.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.base.function_constructor),
            )?;
            record.append_data(
                keys.symbol_has_instance.clone(),
                // QuickJS pins `Function.prototype[Symbol.hasInstance]` as
                // non-writable and non-configurable (`quickjs.c:39511-39523`),
                // matching the specification's frozen descriptor.
                FROZEN_PROPERTY,
                StoredValue::Function(graph.base.function_has_instance),
            )?;
        }

        self.append_constructor_identity(
            graph.base.function_constructor,
            StoredValue::Function(graph.base.function_prototype),
            &names.function,
            keys,
        )?;
        for (function, name, length) in [
            (graph.base.object_to_string, &names.to_string, 0),
            (graph.base.object_value_of, &names.value_of, 0),
            (graph.base.function_to_string, &names.to_string, 0),
            (graph.base.function_call, &names.call, 1),
            (graph.base.function_apply, &names.apply, 2),
            (graph.base.function_bind, &names.bind, 1),
            (graph.base.function_has_instance, &names.has_instance, 1),
        ] {
            self.append_function_identity(function, name, length, keys)?;
        }
        self.publish_object_intrinsic_properties(graph, keys, names)
    }

    /// Publishes the `Object` constructor, its statics, and the
    /// `Object.prototype.constructor` back edge.
    ///
    /// Only reflection operations the current profile can honor completely are
    /// installed; the rest of the pinned surface stays absent so it fails
    /// closed as a missing property instead of behaving incorrectly.
    fn publish_object_intrinsic_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        self.objects
            .get_mut(graph.base.object_prototype)
            .expect("new Object.prototype remains live")
            .record
            .append_data(
                keys.constructor.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.base.object_constructor),
            )?;
        // QuickJS publishes `Object.length` and `Object.name` before its
        // method table, then appends `Object.prototype` after that table.
        // Keeping this order makes `[[OwnPropertyKeys]]` match the pinned
        // constructor shape while retaining the specification descriptors.
        self.append_function_identity(graph.base.object_constructor, &names.object, 1, keys)?;
        for (method, function) in OBJECT_STATIC_METHODS
            .into_iter()
            .zip(graph.base.object_statics)
        {
            let (key, name) = if let Some(atom) = method.predefined_name {
                (
                    self.predefined_property_key(atom),
                    predefined_string(&self.atoms, atom),
                )
            } else if let Some(id) = method.realm_name {
                let atom = graph.dynamic_atoms.atom(id).clone();
                let name = atom
                    .description()
                    .expect("shared dynamic Object static name has a description")
                    .clone();
                (PropertyKey::from_validated_atom(atom), name)
            } else {
                let atom = graph
                    .dynamic_atoms
                    .atom(RealmNameId::ObjectStatic(method.kind))
                    .clone();
                let name = atom
                    .description()
                    .expect("interned Object static name has a description")
                    .clone();
                (PropertyKey::from_validated_atom(atom), name)
            };
            self.functions
                .get_mut(graph.base.object_constructor)
                .expect("new Object constructor remains live")
                .object
                .append_data(key, METHOD_PROPERTY, StoredValue::Function(function))?;
            self.append_function_identity(function, &name, method.length, keys)?;
        }
        self.functions
            .get_mut(graph.base.object_constructor)
            .expect("new Object constructor remains live")
            .object
            .append_data(
                keys.prototype.clone(),
                CONSTRUCTOR_PROTOTYPE_PROPERTY,
                StoredValue::Object(graph.base.object_prototype),
            )?;
        Ok(())
    }

    fn publish_primitive_intrinsic_properties(
        &mut self,
        graph: &PrimitiveIntrinsicGraph,
        spec: PrimitivePropertySpec<'_>,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        if let Some(length) = spec.prototype_length {
            self.objects
                .get_mut(graph.prototype)
                .expect("new primitive prototype remains live")
                .record
                .append_data(
                    keys.length.clone(),
                    IDENTITY_PROPERTY,
                    StoredValue::Number(JsNumber::from_i32(length)),
                )?;
        }
        self.append_object_methods(
            graph.prototype,
            [
                (&keys.constructor, graph.constructor),
                (&keys.to_string, graph.to_string),
                (&keys.value_of, graph.value_of),
            ],
        )?;
        if spec.defer_constructor_prototype {
            self.append_function_identity(graph.constructor, spec.constructor_name, 1, keys)?;
        } else {
            self.append_constructor_identity(
                graph.constructor,
                StoredValue::Object(graph.prototype),
                spec.constructor_name,
                keys,
            )?;
        }
        self.append_function_identity(
            graph.to_string,
            &names.to_string,
            spec.to_string_length,
            keys,
        )?;
        self.append_function_identity(graph.value_of, &names.value_of, 0, keys)
    }

    /// Publishes `String.raw` before the constructor's `prototype` property so
    /// the observable own-key order matches the intrinsic creation algorithm.
    fn publish_string_raw_and_constructor_prototype(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        let name = predefined_string(&self.atoms, PredefinedAtom::Raw);
        let key = self.predefined_property_key(PredefinedAtom::Raw);
        self.functions
            .get_mut(graph.string.constructor)
            .expect("new String constructor remains live")
            .object
            .append_data(
                key,
                METHOD_PROPERTY,
                StoredValue::Function(graph.string_raw),
            )?;
        self.append_function_identity(graph.string_raw, &name, 1, keys)?;
        self.functions
            .get_mut(graph.string.constructor)
            .expect("new String constructor remains live")
            .object
            .append_data(
                keys.prototype.clone(),
                CONSTRUCTOR_PROTOTYPE_PROPERTY,
                StoredValue::Object(graph.string.prototype),
            )
    }

    /// Publishes the `Object.prototype` reflection methods.
    fn publish_object_reflection_methods(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        for ((_, kind, length), function) in OBJECT_PROTOTYPE_REFLECTION
            .into_iter()
            .zip(graph.base.object_reflection)
        {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::ObjectPrototypeMethod(kind))
                .clone();
            let name = atom
                .description()
                .expect("interned Object reflection name has a description")
                .clone();
            self.objects
                .get_mut(graph.base.object_prototype)
                .expect("new Object.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, length, keys)?;
        }
        Ok(())
    }

    /// Publishes every installed `String.prototype` method.
    ///
    /// Each is a `METHOD_PROPERTY`, so it is writable and configurable but not
    /// enumerable, and carries the `name` and `length` the pinned oracle reports.
    fn publish_string_prototype_methods(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        for (method, function) in STRING_PROTOTYPE_METHODS
            .into_iter()
            .zip(graph.string_methods)
        {
            let (key, name) = if let Some(atom) = method.predefined_name {
                (
                    self.predefined_property_key(atom),
                    predefined_string(&self.atoms, atom),
                )
            } else {
                let atom = graph
                    .dynamic_atoms
                    .atom(RealmNameId::StringMethod(method.method))
                    .clone();
                let name = atom
                    .description()
                    .expect("interned String method name has a description")
                    .clone();
                (PropertyKey::from_validated_atom(atom), name)
            };
            self.objects
                .get_mut(graph.string.prototype)
                .expect("new String.prototype remains live")
                .record
                .append_data(key, METHOD_PROPERTY, StoredValue::Function(function))?;
            self.append_function_identity(function, &name, method.length, keys)?;
        }
        Ok(())
    }

    /// Publishes the `Number` value and predicate statics plus `Array.isArray`.
    ///
    /// The value properties are frozen, matching the pinned descriptors for
    /// `Number.MAX_VALUE`; the predicates are ordinary methods.
    #[expect(
        clippy::too_many_lines,
        reason = "one flat publication site keeps the Number, Array, and String constructor statics and their exact descriptors auditable together"
    )]
    fn publish_number_statics(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        for (atom, bits) in NUMBER_PREDEFINED_VALUE_STATICS {
            let key = self.predefined_property_key(atom);
            self.functions
                .get_mut(graph.number.constructor)
                .expect("new Number constructor remains live")
                .object
                .append_data(
                    key,
                    FROZEN_PROPERTY,
                    StoredValue::Number(JsNumber::from_f64(f64::from_bits(bits))),
                )?;
        }
        for (name, bits) in NUMBER_VALUE_STATICS {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::NumberValue(name))
                .clone();
            self.functions
                .get_mut(graph.number.constructor)
                .expect("new Number constructor remains live")
                .object
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    FROZEN_PROPERTY,
                    StoredValue::Number(JsNumber::from_f64(f64::from_bits(bits))),
                )?;
        }
        for ((_, predicate), function) in NUMBER_PREDICATE_STATICS
            .into_iter()
            .zip(graph.number_predicates)
        {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::NumberPredicate(predicate))
                .clone();
            let name = atom
                .description()
                .expect("interned Number static name has a description")
                .clone();
            self.functions
                .get_mut(graph.number.constructor)
                .expect("new Number constructor remains live")
                .object
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 1, keys)?;
        }

        let atom = graph.dynamic_atoms.atom(RealmNameId::ArrayIsArray).clone();
        let name = atom
            .description()
            .expect("interned Array.isArray name has a description")
            .clone();
        self.functions
            .get_mut(graph.array.constructor)
            .expect("new Array constructor remains live")
            .object
            .append_data(
                PropertyKey::from_validated_atom(atom),
                METHOD_PROPERTY,
                StoredValue::Function(graph.array_is_array),
            )?;
        self.append_function_identity(graph.array_is_array, &name, 1, keys)?;

        for (method, function) in ArrayStatic::ALL.into_iter().zip(graph.array_statics) {
            let atom = self.atoms.predefined(method.predefined_atom());
            let name = atom
                .description()
                .expect("predefined Array static name has a description")
                .clone();
            self.functions
                .get_mut(graph.array.constructor)
                .expect("new Array constructor remains live")
                .object
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, method.length(), keys)?;
        }

        for ((_, method), function) in STRING_FROM_STATICS
            .into_iter()
            .zip(graph.string_from_statics)
        {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::StringStatic(method))
                .clone();
            let name = atom
                .description()
                .expect("interned String factory name has a description")
                .clone();
            self.functions
                .get_mut(graph.string.constructor)
                .expect("new String constructor remains live")
                .object
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 1, keys)?;
        }

        for ((_, search), function) in ARRAY_SEARCH_METHODS.into_iter().zip(graph.array_searches) {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::ArraySearch(search))
                .clone();
            let name = atom
                .description()
                .expect("interned Array search name has a description")
                .clone();
            self.objects
                .get_mut(graph.array.prototype)
                .expect("new Array.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 1, keys)?;
        }

        for (mutator, function) in ARRAY_MUTATOR_METHODS.into_iter().zip(graph.array_mutators) {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::ArrayMutator(mutator))
                .clone();
            let name = atom
                .description()
                .expect("interned Array mutator name has a description")
                .clone();
            self.objects
                .get_mut(graph.array.prototype)
                .expect("new Array.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, mutator.arity(), keys)?;
        }

        // The interned names come first, matching the insertion order, and the
        // predefined `concat` follows.
        let copier_keys = ARRAY_COPIER_METHODS
            .into_iter()
            .map(|copier| {
                let atom = graph
                    .dynamic_atoms
                    .atom(RealmNameId::ArrayCopier(copier))
                    .clone();
                let name = atom
                    .description()
                    .expect("interned Array copier name has a description")
                    .clone();
                (copier, PropertyKey::from_validated_atom(atom), name)
            })
            .collect::<Vec<_>>();
        let predefined_keys = ARRAY_PREDEFINED_COPIERS.map(|(atom, copier)| {
            (
                copier,
                self.predefined_property_key(atom),
                predefined_string(&self.atoms, atom),
            )
        });
        for ((copier, key, name), function) in copier_keys
            .into_iter()
            .chain(predefined_keys)
            .zip(graph.array_copiers)
        {
            self.objects
                .get_mut(graph.array.prototype)
                .expect("new Array.prototype remains live")
                .record
                .append_data(key, METHOD_PROPERTY, StoredValue::Function(function))?;
            self.append_function_identity(function, &name, copier.arity(), keys)?;
        }

        for (method, function) in ARRAY_SORT_METHODS.into_iter().zip(graph.array_sorts) {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::ArraySort(method))
                .clone();
            let name = atom
                .description()
                .expect("interned Array sort name has a description")
                .clone();
            self.objects
                .get_mut(graph.array.prototype)
                .expect("new Array.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 1, keys)?;
        }

        for (method, function) in ARRAY_FLATTEN_METHODS.into_iter().zip(graph.array_flattens) {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::ArrayFlatten(method))
                .clone();
            let name = atom
                .description()
                .expect("interned Array flatten name has a description")
                .clone();
            self.objects
                .get_mut(graph.array.prototype)
                .expect("new Array.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, method.arity(), keys)?;
        }

        for (method, function) in NUMBER_FORMAT_METHODS.into_iter().zip(graph.number_formats) {
            let atom = graph
                .dynamic_atoms
                .atom(RealmNameId::NumberFormat(method))
                .clone();
            let name = atom
                .description()
                .expect("interned Number format name has a description")
                .clone();
            self.objects
                .get_mut(graph.number.prototype)
                .expect("new Number.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, 1, keys)?;
        }

        // Every callback method and reduction reports arity 1; `splice` reports
        // 2, which the pinned oracle confirms.
        let callback_arities = ARRAY_CALLBACK_METHODS
            .into_iter()
            .zip(graph.array_callbacks)
            .map(|(method, function)| (function, RealmNameId::ArrayCallback(method), 1))
            .chain(
                ARRAY_REDUCTION_METHODS
                    .into_iter()
                    .zip(graph.array_reductions)
                    .map(|(method, function)| (function, RealmNameId::ArrayReduction(method), 1)),
            )
            .chain(std::iter::once((
                graph.array_splice,
                RealmNameId::ArraySplice,
                2,
            )));
        for (function, atom_id, arity) in callback_arities {
            let atom = graph.dynamic_atoms.atom(atom_id).clone();
            let name = atom
                .description()
                .expect("interned Array callback name has a description")
                .clone();
            self.objects
                .get_mut(graph.array.prototype)
                .expect("new Array.prototype remains live")
                .record
                .append_data(
                    PropertyKey::from_validated_atom(atom),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            self.append_function_identity(function, &name, arity, keys)?;
        }
        Ok(())
    }

    /// Publishes the `BigInt` prototype members and constructor statics.
    ///
    /// The base `BigInt` graph carries `toString`, `valueOf`,
    /// `[Symbol.toStringTag]`, and `constructor`. The no-`Intl`
    /// `toLocaleString` method is published with the other shared locale
    /// methods. The constructor carries `asIntN` and `asUintN`, each with
    /// arity 2.
    fn publish_bigint_intrinsic_properties(
        &mut self,
        graph: &BigIntIntrinsicGraph,
        dynamic_atoms: &RealmAtomBindings,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        {
            let record = &mut self
                .objects
                .get_mut(graph.prototype)
                .expect("new BigInt.prototype remains live")
                .record;
            record.append_data(
                keys.constructor.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.constructor),
            )?;
            record.append_data(
                keys.to_string.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.to_string),
            )?;
            record.append_data(
                keys.value_of.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(graph.value_of),
            )?;
            record.append_data(
                keys.symbol_to_string_tag.clone(),
                // The tag is non-writable and non-enumerable but configurable,
                // which is the specification's descriptor for it.
                IDENTITY_PROPERTY,
                StoredValue::String(names.bigint.clone()),
            )?;
        }
        self.append_constructor_identity(
            graph.constructor,
            StoredValue::Object(graph.prototype),
            &names.bigint,
            keys,
        )?;
        self.append_function_identity(graph.to_string, &names.to_string, 0, keys)?;
        self.append_function_identity(graph.value_of, &names.value_of, 0, keys)?;
        {
            let signed_key = PropertyKey::from_validated_atom(
                dynamic_atoms
                    .atom(RealmNameId::BigIntStatic(NativeFunctionKind::BigIntAsIntN))
                    .clone(),
            );
            let unsigned_key = PropertyKey::from_validated_atom(
                dynamic_atoms
                    .atom(RealmNameId::BigIntStatic(NativeFunctionKind::BigIntAsUintN))
                    .clone(),
            );
            let record = &mut self
                .functions
                .get_mut(graph.constructor)
                .expect("new BigInt constructor remains live")
                .object;
            record.append_data(
                signed_key,
                METHOD_PROPERTY,
                StoredValue::Function(graph.as_int_n),
            )?;
            record.append_data(
                unsigned_key,
                METHOD_PROPERTY,
                StoredValue::Function(graph.as_uint_n),
            )?;
        }
        self.append_function_identity(graph.as_int_n, &names.as_int_n, 2, keys)?;
        self.append_function_identity(graph.as_uint_n, &names.as_uint_n, 2, keys)
    }

    fn publish_array_intrinsic_properties(
        &mut self,
        graph: &ArrayIntrinsicGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        self.append_object_methods(
            graph.prototype,
            [
                (&keys.constructor, graph.constructor),
                (&keys.join, graph.join),
                (&keys.to_string, graph.to_string),
            ],
        )?;
        self.functions
            .get_mut(graph.constructor)
            .expect("new Array constructor remains live")
            .object
            .append_data(
                keys.prototype.clone(),
                CONSTRUCTOR_PROTOTYPE_PROPERTY,
                StoredValue::Object(graph.prototype),
            )?;
        self.functions
            .get_mut(graph.constructor)
            .expect("new Array constructor remains live")
            .object
            .append_accessor(
                keys.symbol_species.clone(),
                PropertyLayout::accessor(false, true),
                Some(graph.species),
                None,
            )?;
        self.append_function_identity(graph.species, &names.get_species, 0, keys)?;
        // The pinned table reports `join` with length 1 and `toString` with
        // length 0 (`quickjs.c:44557-44558`).
        self.append_function_identity(graph.join, &names.join, 1, keys)?;
        self.append_function_identity(graph.to_string, &names.to_string, 0, keys)
    }

    fn publish_array_constructor_identity(
        &mut self,
        graph: &ArrayIntrinsicGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        self.append_function_identity(graph.constructor, &names.array, 1, keys)
    }

    fn publish_iterator_intrinsic_properties(
        &mut self,
        iterators: &IteratorIntrinsicGraph,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        self.append_object_methods(
            iterators.iterator_prototype,
            [(&keys.symbol_iterator, iterators.iterator_method)],
        )?;
        self.append_function_identity(
            iterators.iterator_method,
            &names.symbol_iterator_name,
            0,
            keys,
        )?;

        self.append_object_methods(
            iterators.array_iterator_prototype,
            [(&keys.next, iterators.array_iterator_next)],
        )?;
        self.objects
            .get_mut(iterators.array_iterator_prototype)
            .expect("new Array Iterator prototype remains live")
            .record
            .append_data(
                keys.symbol_to_string_tag.clone(),
                IDENTITY_PROPERTY,
                StoredValue::String(names.array_iterator.clone()),
            )?;
        self.append_function_identity(iterators.array_iterator_next, &names.next, 0, keys)?;

        let entries = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::Entries).clone(),
        );
        self.append_object_methods(
            graph.array.prototype,
            [
                (&keys.values, iterators.array_values),
                (&keys.symbol_iterator, iterators.array_values),
                (&keys.keys, iterators.array_keys),
                (&entries, iterators.array_entries),
            ],
        )?;
        for (function, name) in [
            (iterators.array_values, &names.values),
            (iterators.array_keys, &names.keys),
            (iterators.array_entries, &names.entries),
        ] {
            self.append_function_identity(function, name, 0, keys)?;
        }

        self.append_object_methods(
            iterators.string_iterator_prototype,
            [(&keys.next, iterators.string_iterator_next)],
        )?;
        self.objects
            .get_mut(iterators.string_iterator_prototype)
            .expect("new String Iterator prototype remains live")
            .record
            .append_data(
                keys.symbol_to_string_tag.clone(),
                IDENTITY_PROPERTY,
                StoredValue::String(names.string_iterator.clone()),
            )?;
        self.append_function_identity(iterators.string_iterator_next, &names.next, 0, keys)?;
        self.append_object_methods(
            graph.string.prototype,
            [(&keys.symbol_iterator, iterators.string_iterator)],
        )?;
        self.append_function_identity(
            iterators.string_iterator,
            &names.symbol_iterator_name,
            0,
            keys,
        )
    }

    fn publish_symbol_intrinsic_properties(
        &mut self,
        symbol: &SymbolIntrinsicGraph,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        let description_key = PropertyKey::from_validated_atom(
            graph.dynamic_atoms.atom(RealmNameId::Description).clone(),
        );
        {
            let record = &mut self
                .objects
                .get_mut(symbol.prototype)
                .expect("new Symbol prototype remains live")
                .record;
            for (key, function) in [
                (&keys.constructor, symbol.constructor),
                (&keys.to_string, symbol.to_string),
                (&keys.value_of, symbol.value_of),
            ] {
                record.append_data(
                    key.clone(),
                    METHOD_PROPERTY,
                    StoredValue::Function(function),
                )?;
            }
            record.append_data(
                keys.symbol_to_primitive.clone(),
                IDENTITY_PROPERTY,
                StoredValue::Function(symbol.to_primitive),
            )?;
            record.append_data(
                keys.symbol_to_string_tag.clone(),
                IDENTITY_PROPERTY,
                StoredValue::String(names.symbol.clone()),
            )?;
            record.append_accessor(
                description_key,
                PropertyLayout::accessor(false, true),
                Some(symbol.description),
                None,
            )?;
        }

        self.publish_symbol_constructor_properties(symbol, graph, keys, names)?;
        for (function, name, length) in [
            (symbol.to_string, &names.to_string, 0),
            (symbol.value_of, &names.value_of, 0),
            (symbol.to_primitive, &names.symbol_to_primitive_name, 1),
            (symbol.description, &names.get_description, 0),
            (symbol.symbol_for, &names.symbol_for, 1),
            (symbol.key_for, &names.key_for, 1),
        ] {
            self.append_function_identity(function, name, length, keys)?;
        }
        Ok(())
    }

    fn publish_symbol_constructor_properties(
        &mut self,
        symbol: &SymbolIntrinsicGraph,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
    ) -> Result<(), TryReserveError> {
        let key_for_key =
            PropertyKey::from_validated_atom(graph.dynamic_atoms.atom(RealmNameId::KeyFor).clone());
        let record = &mut self
            .functions
            .get_mut(symbol.constructor)
            .expect("new Symbol constructor remains live")
            .object;
        record.append_data(
            keys.prototype.clone(),
            CONSTRUCTOR_PROTOTYPE_PROPERTY,
            StoredValue::Object(symbol.prototype),
        )?;
        record.append_data(
            keys.length.clone(),
            IDENTITY_PROPERTY,
            StoredValue::Number(JsNumber::from_i32(0)),
        )?;
        record.append_data(
            keys.name.clone(),
            IDENTITY_PROPERTY,
            StoredValue::String(names.symbol.clone()),
        )?;
        record.append_data(
            keys.for_key.clone(),
            METHOD_PROPERTY,
            StoredValue::Function(symbol.symbol_for),
        )?;
        record.append_data(
            key_for_key,
            METHOD_PROPERTY,
            StoredValue::Function(symbol.key_for),
        )?;
        for (index, (_, symbol_atom)) in DYNAMIC_SYMBOL_STATIC_PROPERTIES.iter().enumerate() {
            if index == 6 {
                record.append_data(
                    keys.split.clone(),
                    FROZEN_PROPERTY,
                    StoredValue::Symbol(self.atoms.predefined(PredefinedAtom::SymbolSplit)),
                )?;
            }
            record.append_data(
                PropertyKey::from_validated_atom(
                    graph
                        .dynamic_atoms
                        .atom(RealmNameId::SymbolStatic(*symbol_atom))
                        .clone(),
                ),
                FROZEN_PROPERTY,
                StoredValue::Symbol(self.atoms.predefined(*symbol_atom)),
            )?;
        }
        Ok(())
    }

    fn append_constructor_identity(
        &mut self,
        function: FunctionId,
        prototype: StoredValue,
        name: &JsString,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        self.functions
            .get_mut(function)
            .expect("new intrinsic constructor remains live")
            .object
            .append_data(
                keys.prototype.clone(),
                CONSTRUCTOR_PROTOTYPE_PROPERTY,
                prototype,
            )?;
        self.append_function_identity(function, name, 1, keys)
    }

    fn append_function_identity(
        &mut self,
        function: FunctionId,
        name: &JsString,
        length: i32,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
        let record = &mut self
            .functions
            .get_mut(function)
            .expect("new intrinsic function remains live")
            .object;
        record.append_data(
            keys.length.clone(),
            IDENTITY_PROPERTY,
            StoredValue::Number(JsNumber::from_i32(length)),
        )?;
        record.append_data(
            keys.name.clone(),
            IDENTITY_PROPERTY,
            StoredValue::String(name.clone()),
        )
    }

    fn append_object_methods<const N: usize>(
        &mut self,
        object: ObjectId,
        methods: [(&PropertyKey, FunctionId); N],
    ) -> Result<(), TryReserveError> {
        let record = &mut self
            .objects
            .get_mut(object)
            .expect("new intrinsic object remains live")
            .record;
        for (key, function) in methods {
            record.append_data(
                key.clone(),
                METHOD_PROPERTY,
                StoredValue::Function(function),
            )?;
        }
        Ok(())
    }

    /// Borrows an exclusive execution context for one same-runtime realm.
    ///
    /// # Errors
    ///
    /// Rejects orphaned, foreign, or stale realm handles.
    pub fn context(&mut self, realm: &Realm) -> Result<Context<'_>, HandleError> {
        self.drain_releases();
        let Some(owner) = realm.0.owner.upgrade() else {
            return Err(HandleError::Orphaned {
                kind: HandleKind::Realm,
            });
        };
        if !Arc::ptr_eq(&owner, &self.mailbox) {
            return Err(HandleError::ForeignRuntime {
                kind: HandleKind::Realm,
            });
        }
        if !self.realms.contains(realm.0.id) {
            return Err(HandleError::Stale {
                kind: HandleKind::Realm,
                index: realm.0.id.index(),
                generation: realm.0.id.generation(),
            });
        }
        Ok(Context {
            runtime: self,
            realm: realm.0.id,
        })
    }
}

fn reserved_record(capacity: usize) -> Result<ObjectRecord, RuntimeError> {
    let mut record = ObjectRecord::empty(None);
    record
        .try_reserve_data(capacity)
        .map_err(|_| property_allocation_failed(capacity))?;
    Ok(record)
}

/// Reserves one record per `Object` static method.
///
/// Each static is an ordinary native function object carrying `length` and
/// `name`, so every record reserves exactly two property slots.
fn object_static_records() -> Result<[ObjectRecord; OBJECT_STATIC_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; OBJECT_STATIC_METHODS.len()] =
        [const { None }; OBJECT_STATIC_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Object static record was reserved")))
}

fn array_static_records() -> Result<[ObjectRecord; ArrayStatic::ALL.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ArrayStatic::ALL.len()] =
        [const { None }; ArrayStatic::ALL.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array static record was reserved")))
}

/// Reserves one record per `Object.prototype` reflection method.
fn object_reflection_records()
-> Result<[ObjectRecord; OBJECT_PROTOTYPE_REFLECTION.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; OBJECT_PROTOTYPE_REFLECTION.len()] =
        [const { None }; OBJECT_PROTOTYPE_REFLECTION.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Object reflection record was reserved")))
}

/// Reserves one native function record per `%Reflect%` method.
fn reflect_method_records() -> Result<[ObjectRecord; ReflectMethod::ALL.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ReflectMethod::ALL.len()] =
        [const { None }; ReflectMethod::ALL.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Reflect method record was reserved")))
}

/// Reserves one native function record per installed `%Math%` method.
fn math_method_records() -> Result<[ObjectRecord; MathMethod::ALL.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; MathMethod::ALL.len()] =
        [const { None }; MathMethod::ALL.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Math method record was reserved")))
}

/// Reserves one record per `Array.prototype` reduction.
fn array_reduction_records() -> Result<[ObjectRecord; ARRAY_REDUCTION_METHODS.len()], RuntimeError>
{
    let mut records: [Option<ObjectRecord>; ARRAY_REDUCTION_METHODS.len()] =
        [const { None }; ARRAY_REDUCTION_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array reduction record was reserved")))
}

/// Reserves one record per `Array.prototype` callback method.
fn array_callback_records() -> Result<[ObjectRecord; ARRAY_CALLBACK_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ARRAY_CALLBACK_METHODS.len()] =
        [const { None }; ARRAY_CALLBACK_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array callback record was reserved")))
}

/// Reserves one record per `Number.prototype` decimal rendering.
fn number_format_records() -> Result<[ObjectRecord; NUMBER_FORMAT_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; NUMBER_FORMAT_METHODS.len()] =
        [const { None }; NUMBER_FORMAT_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Number format record was reserved")))
}

/// Reserves one record per `Array.prototype` copying method.
fn array_copier_records() -> Result<[ObjectRecord; ARRAY_COPIER_TOTAL], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ARRAY_COPIER_TOTAL] =
        [const { None }; ARRAY_COPIER_TOTAL];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array copier record was reserved")))
}

/// Reserves one record per stable Array sorting method.
fn array_sort_records() -> Result<[ObjectRecord; ARRAY_SORT_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ARRAY_SORT_METHODS.len()] =
        [const { None }; ARRAY_SORT_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array sort record was reserved")))
}

/// Reserves one record per `FlattenIntoArray` method.
fn array_flatten_records() -> Result<[ObjectRecord; ARRAY_FLATTEN_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ARRAY_FLATTEN_METHODS.len()] =
        [const { None }; ARRAY_FLATTEN_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array flatten record was reserved")))
}

/// Reserves one record per deterministic locale-string method.
fn locale_string_records() -> Result<[ObjectRecord; LOCALE_STRING_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; LOCALE_STRING_METHODS.len()] =
        [const { None }; LOCALE_STRING_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every locale-string record was reserved")))
}

/// Reserves one record per `Array.prototype` mutator.
fn array_mutator_records() -> Result<[ObjectRecord; ARRAY_MUTATOR_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ARRAY_MUTATOR_METHODS.len()] =
        [const { None }; ARRAY_MUTATOR_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array mutator record was reserved")))
}

/// Reserves one record per `Array.prototype` search.
fn array_search_records() -> Result<[ObjectRecord; ARRAY_SEARCH_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; ARRAY_SEARCH_METHODS.len()] =
        [const { None }; ARRAY_SEARCH_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Array search record was reserved")))
}

/// Reserves one record per `String` code-unit factory.
fn string_from_records() -> Result<[ObjectRecord; STRING_FROM_STATICS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; STRING_FROM_STATICS.len()] =
        [const { None }; STRING_FROM_STATICS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every String factory record was reserved")))
}

/// Reserves one record per `Number` predicate static.
fn number_predicate_records() -> Result<[ObjectRecord; NUMBER_PREDICATE_STATICS.len()], RuntimeError>
{
    let mut records: [Option<ObjectRecord>; NUMBER_PREDICATE_STATICS.len()] =
        [const { None }; NUMBER_PREDICATE_STATICS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every Number predicate record was reserved")))
}

/// Reserves one record per coercing global numeric function.
fn global_numeric_function_records()
-> Result<[ObjectRecord; GLOBAL_NUMERIC_FUNCTIONS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; GLOBAL_NUMERIC_FUNCTIONS.len()] =
        [const { None }; GLOBAL_NUMERIC_FUNCTIONS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every global numeric record was reserved")))
}

/// Reserves one record per global URI handling function.
fn uri_function_records() -> Result<[ObjectRecord; URI_FUNCTIONS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; URI_FUNCTIONS.len()] =
        [const { None }; URI_FUNCTIONS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every URI function record was reserved")))
}

/// Reserves one record per installed `String.prototype` method.
fn string_method_records() -> Result<[ObjectRecord; STRING_PROTOTYPE_METHODS.len()], RuntimeError> {
    let mut records: [Option<ObjectRecord>; STRING_PROTOTYPE_METHODS.len()] =
        [const { None }; STRING_PROTOTYPE_METHODS.len()];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every String method record was reserved")))
}

const fn allocation_failed(resource: RuntimeResource, additional: usize) -> RuntimeError {
    RuntimeError::AllocationFailed {
        resource,
        additional,
    }
}

const fn property_allocation_failed(additional: usize) -> RuntimeError {
    allocation_failed(RuntimeResource::ObjectProperties, additional)
}
