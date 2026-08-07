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

use super::{
    Arc, Arena, ArrayBufferIntrinsics, ArrayCallback, ArrayCopier, ArrayFlatten, ArrayIntrinsics,
    ArrayMutator, ArrayReduction, ArraySearch, ArraySort, ArrayState, ArrayStatic,
    AsyncFunctionIntrinsics, AsyncGeneratorIntrinsics, AtomError, AtomTable, BigIntIntrinsics,
    BooleanIntrinsics, Context, DataViewIntrinsics, DateIntrinsics, ErrorIntrinsic,
    ErrorIntrinsicKind, ErrorIntrinsics, FinalizationRegistryIntrinsics, FunctionId,
    FunctionImplementation, GeneratorIntrinsics, GlobalNumericFunction, HandleError, HandleKind,
    HashMap, HeapFunction, HeapObject, HeapReference, InterruptState, IteratorIntrinsics, JsNumber,
    JsString, LocaleStringMethod, MapIntrinsics, MapMethod, MathMethod, NativeFunction,
    NativeFunctionKind, NumberFormat, NumberIntrinsics, NumberPredicate, ObjectId, ObjectRecord,
    PredefinedAtom, PromiseIntrinsics, PromiseRejectionState, PromiseStatic, PropertyKey,
    PropertyLayout, Rc, Realm, RealmHandle, RealmId, RealmIntrinsics, RealmState, RefCell,
    ReflectMethod, RegExpIntrinsics, ReleaseMailbox, Runtime, RuntimeError, RuntimeIdentity,
    RuntimeLimits, RuntimeResource, SetIntrinsics, SetMethod, ShapeInterner,
    SharedArrayBufferIntrinsics, StoredValue, StringIntrinsics, StringMethod, SymbolIntrinsics,
    TemporalIntrinsics, TypedArrayIntrinsics, UriFunction, VecDeque, WeakMapIntrinsics,
    WeakRefIntrinsics, WeakSetIntrinsics, check_limit, predefined_string, usize_to_u64,
};
use crate::object::TypedArrayElementType;

use allocation::IntrinsicRecords;
use atoms::{RealmAtomBindings, RealmAtomPlan};
use families::{DeclarativeBatch, RealmIntrinsicSchema};
use publication::RealmPublicationError;
use reservation::RealmReservationPlan;
use schema::{IntrinsicFunctionId, IntrinsicObjectId, RealmNameId};
use transaction::RealmBuildTransaction;

/// The `BigInt` static names that have no predefined atom.
const BIGINT_INTERNED_STATICS: [&str; 2] = ["asIntN", "asUintN"];

/// The `String.prototype` methods this profile installs.
///
/// The set is deliberately narrower than the pinned oracle's: `match`,
/// `search`, `replace`, `replaceAll`, and `split` implement their corresponding
/// well-known-symbol protocols and exact fallbacks. The order is the pinned
/// `QuickJS` own-key order.
///
/// Each `length` matches the pinned oracle, which reports `1` for most methods
/// and `2` for `replace`, `replaceAll`, `split`, `slice`, and `substring`.
const STRING_PROTOTYPE_METHODS: [StringPrototypeMethod; 32] = [
    StringPrototypeMethod::interned("at", StringMethod::At, 1),
    StringPrototypeMethod::interned("charCodeAt", StringMethod::CharCodeAt, 1),
    StringPrototypeMethod::interned("charAt", StringMethod::CharAt, 1),
    StringPrototypeMethod::predefined(PredefinedAtom::Concat, StringMethod::Concat, 1),
    StringPrototypeMethod::interned("codePointAt", StringMethod::CodePointAt, 1),
    StringPrototypeMethod::interned("isWellFormed", StringMethod::IsWellFormed, 0),
    StringPrototypeMethod::interned("toWellFormed", StringMethod::ToWellFormed, 0),
    StringPrototypeMethod::interned("indexOf", StringMethod::IndexOf, 1),
    StringPrototypeMethod::interned("lastIndexOf", StringMethod::LastIndexOf, 1),
    StringPrototypeMethod::interned("includes", StringMethod::Includes, 1),
    StringPrototypeMethod::interned("endsWith", StringMethod::EndsWith, 1),
    StringPrototypeMethod::interned("startsWith", StringMethod::StartsWith, 1),
    StringPrototypeMethod::interned("match", StringMethod::Match, 1),
    StringPrototypeMethod::interned("matchAll", StringMethod::MatchAll, 1),
    StringPrototypeMethod::interned("search", StringMethod::Search, 1),
    StringPrototypeMethod::interned("split", StringMethod::Split, 2),
    StringPrototypeMethod::interned("substring", StringMethod::Substring, 2),
    StringPrototypeMethod::interned("slice", StringMethod::Slice, 2),
    StringPrototypeMethod::interned("repeat", StringMethod::Repeat, 1),
    StringPrototypeMethod::interned("replace", StringMethod::Replace, 2),
    StringPrototypeMethod::interned("replaceAll", StringMethod::ReplaceAll, 2),
    StringPrototypeMethod::interned("padEnd", StringMethod::PadEnd, 1),
    StringPrototypeMethod::interned("padStart", StringMethod::PadStart, 1),
    StringPrototypeMethod::interned("trim", StringMethod::Trim, 0),
    StringPrototypeMethod::interned("trimEnd", StringMethod::TrimEnd, 0),
    StringPrototypeMethod::interned("trimStart", StringMethod::TrimStart, 0),
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
/// set is the complete ECMA-262 2025 static surface, including `groupBy` and
/// `fromEntries`; every method routes through the shared exotic internal-method
/// layer rather than an ordinary-object-only shortcut.
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

struct RealmPublicationState {
    realm: RealmId,
    dynamic_atoms: RealmAtomBindings,
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
            shape_interner: Rc::new(RefCell::new(ShapeInterner::default())),
            cells: Arena::new(runtime_identity),
            global_bindings: Arena::new(runtime_identity),
            limits,
            installed_templates: 0,
            installed_atoms: 0,
            installed_constants: 0,
            object_properties: 0,
            array_buffer_bytes: 0,
            for_in_entries: 0,
            collection_entries: 0,
            public_roots: 0,
            collection_pending: false,
            interrupts: InterruptState::default(),
            promise_rejections: PromiseRejectionState::default(),
            promise_jobs: VecDeque::new(),
            finalization_jobs: VecDeque::new(),
            kept_alive: Vec::new(),
            generator_states: HashMap::new(),
            async_function_states: HashMap::new(),
            async_generator_states: HashMap::new(),
            array_from_async_states: HashMap::new(),
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
        let intrinsic_schema = RealmIntrinsicSchema::try_new()?;
        intrinsic_schema
            .validate()
            .expect("the immutable complete Realm schema is valid");
        let atom_plan = RealmAtomPlan::try_new(&intrinsic_schema)?;
        let reservation = RealmReservationPlan::try_new(&atom_plan, &intrinsic_schema)?;
        reservation.preflight_and_reserve(self)?;
        let records = IntrinsicRecords::try_new(&intrinsic_schema)?;
        let mut transaction = RealmBuildTransaction::try_new(self, reservation)?;
        let graph = transaction.build_realm_graph(records, &atom_plan, &intrinsic_schema)?;
        transaction
            .allocated
            .assert_matches(intrinsic_schema.specs());

        if let Err(error) = transaction.publish_realm_properties(&graph, &intrinsic_schema) {
            return Err(error.into_runtime_error());
        }

        let id = graph.realm;
        let intrinsics = transaction.ready_realm_intrinsics();
        let state = transaction
            .realms
            .get_mut(id)
            .expect("new realm remains live");
        state.intrinsics = intrinsics;
        transaction.commit();
        transaction.canonicalize_all_shapes();
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
        records: IntrinsicRecords,
        atom_plan: &RealmAtomPlan,
        intrinsic_schema: &RealmIntrinsicSchema,
    ) -> Result<RealmPublicationState, RuntimeError> {
        let dynamic_atoms = self.intern_realm_atom_plan(atom_plan)?;
        self.record_atoms(&dynamic_atoms);
        let mut records = records;
        let realm = self.insert_realm_kernel(intrinsic_schema, &mut records);
        self.insert_intrinsics(realm, intrinsic_schema, records);

        self.allocated.assert_complete();

        Ok(RealmPublicationState {
            realm,
            dynamic_atoms,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive typed publication binds every validated Realm intrinsic identity"
    )]
    fn ready_realm_intrinsics(&self) -> RealmIntrinsics {
        let object = |id| self.allocated.object(id);
        let function = |kind| self.allocated.function(IntrinsicFunctionId(kind));
        RealmIntrinsics::Ready {
            function_prototype: function(NativeFunctionKind::FunctionPrototype),
            throw_type_error: function(NativeFunctionKind::ThrowTypeError),
            function_constructor: function(NativeFunctionKind::OrdinaryFunctionConstructor),
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
                prototype: object(IntrinsicObjectId::ArrayPrototype),
                constructor: function(NativeFunctionKind::ArrayConstructor),
            },
            array_buffer: ArrayBufferIntrinsics {
                prototype: object(IntrinsicObjectId::ArrayBufferPrototype),
                constructor: function(NativeFunctionKind::ArrayBufferConstructor),
            },
            shared_array_buffer: SharedArrayBufferIntrinsics {
                prototype: object(IntrinsicObjectId::SharedArrayBufferPrototype),
                constructor: function(NativeFunctionKind::SharedArrayBufferConstructor),
            },
            data_view: DataViewIntrinsics {
                prototype: object(IntrinsicObjectId::DataViewPrototype),
                constructor: function(NativeFunctionKind::DataViewConstructor),
            },
            typed_array: TypedArrayIntrinsics {
                prototype: object(IntrinsicObjectId::TypedArrayPrototype),
                instance_prototypes: TypedArrayElementType::ALL
                    .map(|element| object(IntrinsicObjectId::TypedArrayInstancePrototype(element))),
                constructors: TypedArrayElementType::ALL
                    .map(|element| function(NativeFunctionKind::TypedArrayConstructor(element))),
            },
            date: DateIntrinsics {
                prototype: object(IntrinsicObjectId::DatePrototype),
                constructor: function(NativeFunctionKind::DateConstructor),
            },
            temporal: TemporalIntrinsics {
                namespace: object(IntrinsicObjectId::Temporal),
                duration_prototype: object(IntrinsicObjectId::TemporalDurationPrototype),
                duration_constructor: function(NativeFunctionKind::TemporalDurationConstructor),
                instant_prototype: object(IntrinsicObjectId::TemporalInstantPrototype),
                instant_constructor: function(NativeFunctionKind::TemporalInstantConstructor),
                plain_date_prototype: object(IntrinsicObjectId::TemporalPlainDatePrototype),
                plain_date_constructor: function(NativeFunctionKind::TemporalPlainDateConstructor),
            },
            map: MapIntrinsics {
                prototype: object(IntrinsicObjectId::MapPrototype),
                constructor: function(NativeFunctionKind::MapConstructor),
                iterator_prototype: object(IntrinsicObjectId::MapIteratorPrototype),
            },
            set: SetIntrinsics {
                prototype: object(IntrinsicObjectId::SetPrototype),
                constructor: function(NativeFunctionKind::SetConstructor),
                iterator_prototype: object(IntrinsicObjectId::SetIteratorPrototype),
            },
            weak_map: WeakMapIntrinsics {
                prototype: object(IntrinsicObjectId::WeakMapPrototype),
                constructor: function(NativeFunctionKind::WeakMapConstructor),
            },
            weak_set: WeakSetIntrinsics {
                prototype: object(IntrinsicObjectId::WeakSetPrototype),
                constructor: function(NativeFunctionKind::WeakSetConstructor),
            },
            weak_ref: WeakRefIntrinsics {
                prototype: object(IntrinsicObjectId::WeakRefPrototype),
                constructor: function(NativeFunctionKind::WeakRefConstructor),
            },
            finalization_registry: FinalizationRegistryIntrinsics {
                prototype: object(IntrinsicObjectId::FinalizationRegistryPrototype),
                constructor: function(NativeFunctionKind::FinalizationRegistryConstructor),
            },
            promise: PromiseIntrinsics {
                prototype: object(IntrinsicObjectId::PromisePrototype),
                constructor: function(NativeFunctionKind::PromiseConstructor),
            },
            regexp: RegExpIntrinsics {
                prototype: object(IntrinsicObjectId::RegExpPrototype),
                constructor: function(NativeFunctionKind::RegExpConstructor),
            },
            symbol: SymbolIntrinsics {
                prototype: object(IntrinsicObjectId::SymbolPrototype),
                constructor: function(NativeFunctionKind::SymbolConstructor),
            },
            iterators: IteratorIntrinsics {
                iterator_prototype: object(IntrinsicObjectId::IteratorPrototype),
                async_iterator_prototype: object(IntrinsicObjectId::AsyncIteratorPrototype),
                async_from_sync_iterator_prototype: object(
                    IntrinsicObjectId::AsyncFromSyncIteratorPrototype,
                ),
                async_from_sync_iterator_next: function(
                    NativeFunctionKind::AsyncFromSyncIteratorNext,
                ),
                array_iterator_prototype: object(IntrinsicObjectId::ArrayIteratorPrototype),
                string_iterator_prototype: object(IntrinsicObjectId::StringIteratorPrototype),
                regexp_string_iterator_prototype: object(
                    IntrinsicObjectId::RegExpStringIteratorPrototype,
                ),
                array_values: function(NativeFunctionKind::ArrayPrototypeValues),
            },
            generators: GeneratorIntrinsics {
                function_constructor: function(NativeFunctionKind::GeneratorFunctionConstructor),
                function_prototype: object(IntrinsicObjectId::GeneratorFunctionPrototype),
                generator_prototype: object(IntrinsicObjectId::GeneratorPrototype),
            },
            async_functions: AsyncFunctionIntrinsics {
                function_constructor: function(NativeFunctionKind::AsyncFunctionConstructor),
                function_prototype: object(IntrinsicObjectId::AsyncFunctionPrototype),
            },
            async_generators: AsyncGeneratorIntrinsics {
                function_constructor: function(
                    NativeFunctionKind::AsyncGeneratorFunctionConstructor,
                ),
                function_prototype: object(IntrinsicObjectId::AsyncGeneratorFunctionPrototype),
                generator_prototype: object(IntrinsicObjectId::AsyncGeneratorPrototype),
            },
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
            .insert_heap_function(HeapFunction {
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
            .insert_heap_object(object)
            .expect("the realm transaction reserved all intrinsic object slots");
        self.record_object(id, object);
        object
    }
}

impl RealmBuildTransaction<'_> {
    #[allow(
        clippy::too_many_lines,
        reason = "Realm property batches stay in normative publication order at one transaction boundary"
    )]
    fn publish_realm_properties(
        &mut self,
        graph: &RealmPublicationState,
        intrinsic_schema: &RealmIntrinsicSchema,
    ) -> Result<(), RealmPublicationError> {
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Kernel,
        )?;
        self.finalize_realm_kernel();
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
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Arrays,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Primitives,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Dates,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Iterators,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::GlobalValues,
        )?;
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
            DeclarativeBatch::Regexps,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::Promises,
        )?;
        self.publish_collection_properties(graph, intrinsic_schema)?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::NamespaceObjects,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::KernelGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::PrimitiveGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::ArrayGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::DateGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::SymbolGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::RegExpGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::PromiseGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::MapGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::SetGlobals,
        )?;
        self.publish_intrinsic_schema_batch(
            intrinsic_schema,
            &graph.dynamic_atoms,
            DeclarativeBatch::WeakCollectionGlobals,
        )?;
        Ok(())
    }

    fn publish_collection_properties(
        &mut self,
        graph: &RealmPublicationState,
        intrinsic_schema: &RealmIntrinsicSchema,
    ) -> Result<(), RealmPublicationError> {
        for batch in [
            DeclarativeBatch::Maps,
            DeclarativeBatch::Sets,
            DeclarativeBatch::WeakCollections,
        ] {
            self.publish_intrinsic_schema_batch(intrinsic_schema, &graph.dynamic_atoms, batch)?;
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

const fn allocation_failed(resource: RuntimeResource, additional: usize) -> RuntimeError {
    RuntimeError::AllocationFailed {
        resource,
        additional,
    }
}

const fn property_allocation_failed(additional: usize) -> RuntimeError {
    allocation_failed(RuntimeResource::ObjectProperties, additional)
}
