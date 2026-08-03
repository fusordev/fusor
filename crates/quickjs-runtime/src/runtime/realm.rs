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

mod allocation;
mod atoms;
mod families;
mod publication;
mod reservation;
mod schema;
mod transaction;
mod validation;

use std::collections::TryReserveError;

use super::{
    Arc, Arena, ArrayCallback, ArrayCopier, ArrayFlatten, ArrayIntrinsics, ArrayMutator,
    ArrayReduction, ArraySearch, ArraySort, ArrayState, ArrayStatic, AtomError, AtomTable,
    BigIntIntrinsics, BooleanIntrinsics, Context, ErrorIntrinsic, ErrorIntrinsicKind,
    ErrorIntrinsics, FunctionId, FunctionImplementation, GlobalNumericFunction, HandleError,
    HandleKind, HashMap, HeapFunction, HeapObject, HeapReference, InterruptState,
    IteratorIntrinsics, JsNumber, JsString, LocaleStringMethod, MathMethod, NativeFunction,
    NativeFunctionKind, NumberFormat, NumberIntrinsics, NumberPredicate, ObjectId, ObjectRecord,
    PredefinedAtom, PropertyKey, PropertyLayout, Realm, RealmHandle, RealmId, RealmIntrinsics,
    RealmState, ReflectMethod, ReleaseMailbox, Runtime, RuntimeError, RuntimeIdentity,
    RuntimeLimits, RuntimeResource, StoredValue, StringIntrinsics, StringMethod, SymbolIntrinsics,
    UriFunction, check_limit, predefined_string, usize_to_u64,
};

use allocation::DeclarativeIntrinsicRecords;
use atoms::{RealmAtomBindings, RealmAtomPlan};
use families::{DeclarativeBatch, RealmFunctionSchema};
use publication::RealmPublicationError;
use reservation::RealmReservationPlan;
use schema::{IntrinsicFunctionId, IntrinsicObjectId, RealmNameId};
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
    function: PropertyKey,
    object: PropertyKey,
    join: PropertyKey,
    array: PropertyKey,
    prototype: PropertyKey,
    constructor: PropertyKey,
    length: PropertyKey,
    name: PropertyKey,
    to_string: PropertyKey,
    value_of: PropertyKey,
    apply: PropertyKey,
    caller: PropertyKey,
    arguments: PropertyKey,
    values: PropertyKey,
    keys: PropertyKey,
    next: PropertyKey,
    symbol_iterator: PropertyKey,
    symbol_to_string_tag: PropertyKey,
    symbol_has_instance: PropertyKey,
    symbol_species: PropertyKey,
}

impl RealmKeys {
    fn new(atoms: &AtomTable) -> Self {
        let key = |atom| PropertyKey::from_validated_atom(atoms.predefined(atom));
        Self {
            function: key(PredefinedAtom::Function),
            object: key(PredefinedAtom::Object),
            join: key(PredefinedAtom::Join),
            array: key(PredefinedAtom::Array),
            prototype: key(PredefinedAtom::Prototype),
            constructor: key(PredefinedAtom::Constructor),
            length: key(PredefinedAtom::Length),
            name: key(PredefinedAtom::Name),
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
            symbol_to_string_tag: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolToStringTag),
            ),
            symbol_has_instance: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolHasInstance),
            ),
            symbol_species: PropertyKey::from_validated_symbol(
                atoms.predefined(PredefinedAtom::SymbolSpecies),
            ),
        }
    }
}

struct RealmNames {
    function: JsString,
    object: JsString,
    join: JsString,
    array: JsString,
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
    description: JsString,
    is_error: JsString,
    array_iterator: JsString,
    string_iterator: JsString,
    symbol_iterator_name: JsString,
    get_species: JsString,
    reflect: JsString,
    is_raw_json: JsString,
    parse: JsString,
    stringify: JsString,
}

impl RealmNames {
    fn try_new(atoms: &AtomTable) -> Result<Self, RuntimeError> {
        Ok(Self {
            function: predefined_string(atoms, PredefinedAtom::Function),
            object: predefined_string(atoms, PredefinedAtom::Object),
            join: predefined_string(atoms, PredefinedAtom::Join),
            array: predefined_string(atoms, PredefinedAtom::Array),
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
            description: JsString::from_utf8("description").map_err(AtomError::from)?,
            is_error: JsString::from_utf8("isError").map_err(AtomError::from)?,
            array_iterator: predefined_string(atoms, PredefinedAtom::ArrayIterator),
            string_iterator: predefined_string(atoms, PredefinedAtom::StringIterator),
            symbol_iterator_name: JsString::from_utf8("[Symbol.iterator]")
                .map_err(AtomError::from)?,
            get_species: JsString::from_utf8("get [Symbol.species]").map_err(AtomError::from)?,
            reflect: JsString::from_utf8("Reflect").map_err(AtomError::from)?,
            is_raw_json: JsString::from_utf8("isRawJSON").map_err(AtomError::from)?,
            parse: JsString::from_utf8("parse").map_err(AtomError::from)?,
            stringify: JsString::from_utf8("stringify").map_err(AtomError::from)?,
        })
    }
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

struct RealmRecords {
    base: RealmBaseRecords,
    array_searches: [ObjectRecord; ARRAY_SEARCH_METHODS.len()],
    array_mutators: [ObjectRecord; ARRAY_MUTATOR_METHODS.len()],
    array_copiers: [ObjectRecord; ARRAY_COPIER_TOTAL],
    array_sorts: [ObjectRecord; ARRAY_SORT_METHODS.len()],
    array_flattens: [ObjectRecord; ARRAY_FLATTEN_METHODS.len()],
    array_callbacks: [ObjectRecord; ARRAY_CALLBACK_METHODS.len()],
    array_reductions: [ObjectRecord; ARRAY_REDUCTION_METHODS.len()],
    array_splice: ObjectRecord,
    array_is_array: ObjectRecord,
    array_statics: [ObjectRecord; ArrayStatic::ALL.len()],
    array: ArrayIntrinsicRecords,
    iterators: IteratorIntrinsicRecords,
    declarative: DeclarativeIntrinsicRecords,
}

impl RealmRecords {
    fn try_new(
        length_key: &PropertyKey,
        schema: &RealmFunctionSchema,
    ) -> Result<Self, RuntimeError> {
        // Keep these reservations in the original transaction order so a
        // recoverable allocation failure reports the same `additional` value.
        let base = RealmBaseRecords {
            global: reserved_record(31)?,
            object_prototype: reserved_record(4 + OBJECT_PROTOTYPE_REFLECTION.len())?,
            function_prototype: reserved_record(10)?,
            throw_type_error: reserved_record(2)?,
            function_constructor: reserved_record(3)?,
            object_constructor: reserved_record(3 + OBJECT_STATIC_METHODS.len())?,
            object_statics: reserved_function_records()?,
            object_to_string: reserved_record(2)?,
            object_value_of: reserved_record(2)?,
            object_reflection: reserved_function_records()?,
            function_to_string: reserved_record(2)?,
            function_call: reserved_record(2)?,
            function_apply: reserved_record(2)?,
            function_bind: reserved_record(2)?,
            function_has_instance: reserved_record(2)?,
        };
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
        let declarative = DeclarativeIntrinsicRecords::try_new(schema)?;
        Ok(Self {
            base,
            array_searches: reserved_function_records()?,
            array_mutators: reserved_function_records()?,
            array_copiers: reserved_function_records()?,
            array_sorts: reserved_function_records()?,
            array_flattens: reserved_function_records()?,
            array_callbacks: reserved_function_records()?,
            array_reductions: reserved_function_records()?,
            array_splice: reserved_record(2)?,
            array_is_array: reserved_record(2)?,
            array_statics: reserved_function_records()?,
            array,
            iterators,
            declarative,
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

struct RealmGraph {
    base: RealmBase,
    dynamic_atoms: RealmAtomBindings,
    array_searches: [FunctionId; ARRAY_SEARCH_METHODS.len()],
    array_mutators: [FunctionId; ARRAY_MUTATOR_METHODS.len()],
    array_copiers: [FunctionId; ARRAY_COPIER_TOTAL],
    array_sorts: [FunctionId; ARRAY_SORT_METHODS.len()],
    array_flattens: [FunctionId; ARRAY_FLATTEN_METHODS.len()],
    array_callbacks: [FunctionId; ARRAY_CALLBACK_METHODS.len()],
    array_reductions: [FunctionId; ARRAY_REDUCTION_METHODS.len()],
    array_splice: FunctionId,
    array_is_array: FunctionId,
    array_statics: [FunctionId; ArrayStatic::ALL.len()],
    array: ArrayIntrinsicGraph,
    iterators: IteratorIntrinsicGraph,
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
        let intrinsic_schema = RealmFunctionSchema::try_new()?;
        intrinsic_schema
            .validate()
            .expect("the immutable complete Realm schema is valid");
        let reservation = RealmReservationPlan::try_new(&atom_plan, &intrinsic_schema)?;
        reservation.preflight_and_reserve(self)?;
        let records = RealmRecords::try_new(&keys.length, &intrinsic_schema)?;
        let mut transaction = RealmBuildTransaction::try_new(self, reservation)?;
        let graph = transaction.build_realm_graph(records, &atom_plan, &intrinsic_schema)?;
        transaction
            .allocated
            .assert_matches(intrinsic_schema.specs());

        if let Err(error) =
            transaction.publish_realm_properties(&graph, &keys, &names, &intrinsic_schema)
        {
            return Err(error.into_runtime_error());
        }

        let id = graph.base.realm;
        let intrinsics = transaction.ready_realm_intrinsics(&graph);
        let state = transaction
            .realms
            .get_mut(id)
            .expect("new realm remains live");
        state.intrinsics = intrinsics;
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
    fn build_realm_graph(
        &mut self,
        records: RealmRecords,
        atom_plan: &RealmAtomPlan<'_>,
        intrinsic_schema: &RealmFunctionSchema,
    ) -> Result<RealmGraph, RuntimeError> {
        let dynamic_atoms = self.intern_realm_atom_plan(atom_plan)?;
        self.record_atoms(&dynamic_atoms);
        let base = self.insert_realm_base(records.base);

        let array = self.insert_array_intrinsics(&base, records.array);
        let iterators = self.insert_iterator_intrinsics(&base, records.iterators);
        self.insert_declarative_intrinsics(base.realm, intrinsic_schema, records.declarative);
        let array_searches = self.insert_array_searches(&base, records.array_searches);
        let array_mutators = self.insert_array_mutators(&base, records.array_mutators);
        let array_copiers = self.insert_array_copiers(&base, records.array_copiers);
        let array_sorts = self.insert_array_sorts(&base, records.array_sorts);
        let array_flattens = self.insert_array_flattens(&base, records.array_flattens);
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

        self.allocated.assert_complete();

        Ok(RealmGraph {
            base,
            dynamic_atoms,
            array_searches,
            array_mutators,
            array_copiers,
            array_sorts,
            array_flattens,
            array_callbacks,
            array_reductions,
            array_splice,
            array_is_array,
            array_statics,
            array,
            iterators,
        })
    }

    fn ready_realm_intrinsics(&self, graph: &RealmGraph) -> RealmIntrinsics {
        let object = |id| self.allocated.object(id);
        let function = |kind| self.allocated.function(IntrinsicFunctionId(kind));
        RealmIntrinsics::Ready {
            function_prototype: graph.base.function_prototype,
            throw_type_error: graph.base.throw_type_error,
            function_constructor: graph.base.function_constructor,
            errors: ErrorIntrinsics {
                entries: ErrorIntrinsicKind::ALL.map(|kind| ErrorIntrinsic {
                    prototype: object(IntrinsicObjectId::ErrorPrototype(kind)),
                    constructor: function(NativeFunctionKind::ErrorConstructor(kind)),
                }),
                to_string: function(NativeFunctionKind::ErrorPrototypeToString),
                is_error: function(NativeFunctionKind::ErrorIsError),
            },
            boolean: BooleanIntrinsics {
                prototype: object(IntrinsicObjectId::BooleanPrototype),
                constructor: function(NativeFunctionKind::BooleanConstructor),
            },
            number: NumberIntrinsics {
                prototype: object(IntrinsicObjectId::NumberPrototype),
                constructor: function(NativeFunctionKind::NumberConstructor),
            },
            bigint: BigIntIntrinsics {
                prototype: object(IntrinsicObjectId::BigIntPrototype),
                constructor: function(NativeFunctionKind::BigIntConstructor),
            },
            string: StringIntrinsics {
                prototype: object(IntrinsicObjectId::StringPrototype),
                constructor: function(NativeFunctionKind::StringConstructor),
            },
            array: ArrayIntrinsics {
                prototype: graph.array.prototype,
                constructor: graph.array.constructor,
            },
            symbol: SymbolIntrinsics {
                prototype: object(IntrinsicObjectId::SymbolPrototype),
                constructor: function(NativeFunctionKind::SymbolConstructor),
            },
            iterators: IteratorIntrinsics {
                iterator_prototype: graph.iterators.iterator_prototype,
                array_iterator_prototype: graph.iterators.array_iterator_prototype,
                string_iterator_prototype: graph.iterators.string_iterator_prototype,
                array_values: graph.iterators.array_values,
            },
        }
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
        let object_prototype = self.insert_reserved_object(
            IntrinsicObjectId::ObjectPrototype,
            HeapObject::ordinary(records.object_prototype),
        );
        records
            .global
            .replace_prototype(Some(HeapReference::Object(object_prototype)));
        let global_object = self.insert_reserved_object(
            IntrinsicObjectId::GlobalObject,
            HeapObject::ordinary(records.global),
        );
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

    fn insert_array_intrinsics(
        &mut self,
        base: &RealmBase,
        mut records: ArrayIntrinsicRecords,
    ) -> ArrayIntrinsicGraph {
        records
            .prototype
            .replace_prototype(Some(HeapReference::Object(base.object_prototype)));
        let prototype = self.insert_reserved_object(
            IntrinsicObjectId::ArrayPrototype,
            HeapObject::array(records.prototype, ArrayState::new(0)),
        );
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
        let iterator_prototype = self.insert_reserved_object(
            IntrinsicObjectId::IteratorPrototype,
            HeapObject::ordinary(records.iterator_prototype),
        );
        let iterator_method = self.insert_reserved_native(
            base.realm,
            HeapReference::Function(base.function_prototype),
            NativeFunctionKind::IteratorPrototypeIterator,
            records.iterator_method,
        );

        records
            .array_iterator_prototype
            .replace_prototype(Some(HeapReference::Object(iterator_prototype)));
        let array_iterator_prototype = self.insert_reserved_object(
            IntrinsicObjectId::ArrayIteratorPrototype,
            HeapObject::ordinary(records.array_iterator_prototype),
        );
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
        let string_iterator_prototype = self.insert_reserved_object(
            IntrinsicObjectId::StringIteratorPrototype,
            HeapObject::ordinary(records.string_iterator_prototype),
        );
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
        self.record_function(IntrinsicFunctionId(kind), function);
        function
    }

    fn insert_reserved_object(&mut self, id: IntrinsicObjectId, object: HeapObject) -> ObjectId {
        let object = self
            .objects
            .try_insert(object)
            .expect("the realm transaction reserved all intrinsic object slots");
        self.record_object(id, object);
        object
    }
}

impl RealmBuildTransaction<'_> {
    fn publish_realm_properties(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
        names: &RealmNames,
        intrinsic_schema: &RealmFunctionSchema,
    ) -> Result<(), RealmPublicationError> {
        self.append_object_methods(
            graph.base.object_prototype,
            [
                (&keys.to_string, graph.base.object_to_string),
                (&keys.value_of, graph.base.object_value_of),
            ],
        )?;
        self.publish_object_reflection_methods(graph, keys)?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Errors,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::ErrorGlobals,
        )?;
        self.publish_function_intrinsic_properties(graph, keys, names)?;
        self.publish_array_constructor_identity(&graph.array, keys, names)?;
        self.publish_number_statics(graph, keys)?;
        self.publish_array_intrinsic_properties(&graph.array, keys, names)?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Primitives,
        )?;
        self.publish_iterator_intrinsic_properties(&graph.iterators, graph, keys, names)?;
        self.publish_global_value_properties(graph)?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Globals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Symbols,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::NamespaceObjects,
        )?;
        self.append_object_methods(
            graph.base.global_object,
            [
                (&keys.function, graph.base.function_constructor),
                (&keys.object, graph.base.object_constructor),
            ],
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::PrimitiveGlobals,
        )?;
        self.append_object_methods(
            graph.base.global_object,
            [(&keys.array, graph.array.constructor)],
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::SymbolGlobals,
        )?;
        Ok(())
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

    /// Publishes the Array method families that have not migrated yet.
    #[expect(
        clippy::too_many_lines,
        reason = "the remaining Array method families preserve their exact declaration order until the Array schema migration"
    )]
    fn publish_number_statics(
        &mut self,
        graph: &RealmGraph,
        keys: &RealmKeys,
    ) -> Result<(), TryReserveError> {
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
        self.append_function_identity(
            iterators.string_iterator,
            &names.symbol_iterator_name,
            0,
            keys,
        )
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
}

impl Runtime {
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

/// Reserves records for an ordinary native function family.
///
/// Every ordinary Realm-native function starts with exactly the non-writable
/// `length` and `name` own properties. Family-specific extra properties remain
/// reserved with their holder's declaration.
fn reserved_function_records<const N: usize>() -> Result<[ObjectRecord; N], RuntimeError> {
    let mut records: [Option<ObjectRecord>; N] = [const { None }; N];
    for slot in &mut records {
        *slot = Some(reserved_record(2)?);
    }
    Ok(records.map(|record| record.expect("every native function record was reserved")))
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
