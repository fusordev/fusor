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

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::{Arc, Weak},
};

use quickjs_bytecode::{
    CompilerBindingKind, CompilerBindingPolicy, CompilerCaptureLayout, CompilerCapturedBinding,
    CompilerClosureBinding, CompilerConstant, CompilerConstantValue, CompilerExecutableKind,
    FinalOpcode, FunctionTemplateId, Instruction, Operands, VerifiedBytecode,
};

use crate::promise_rejection::PromiseRejectionState;
use crate::{
    ArrayIndex, Atom, AtomError, AtomLimits, AtomTable, AtomUsage, DynamicFunctionScriptError,
    ErrorObjectKind, ExceptionKind, ExecutionLimits, Function, GlobalScriptError, HandleError,
    HandleKind, InstallError, JsBigInt, JsNumber, JsString, JsValue,
    OrdinaryDynamicFunctionCompiler, PredefinedAtom, PropertyKey, PropertyLayout,
    PropertyLayoutKind, RuntimeError, RuntimeResource,
    arena::{Arena, RuntimeIdentity},
    ids::{BindingCellId, FunctionId, InstalledCodeId, ObjectId, RealmGlobalBindingId, RealmId},
    interrupt::InterruptState,
    object::{
        ArrayBufferState, ArrayIterator, ArrayIteratorKind, ArrayState, BoxedPrimitive,
        DataViewState, DateState, ForInIterator, ForInSnapshot, HeapObject, KeyPhases,
        ObjectRecord, OwnProperty, PromiseCapability, PromiseReaction, PropertyDeletion,
        ProxyState, RegExpState, RegExpStringIterator, ShapeInterner, StringIterator,
        TypedArrayElementType, TypedArrayState,
    },
    value::{HeapReference, PrimitiveValue, ReleaseMailbox, RootTarget, SlotValue, StoredValue},
};

mod array_buffers;
mod async_functions;
mod data_views;
mod dates;
mod iterators;
mod limits;
mod maps;
mod promises;
mod proxies;
mod regexps;
mod sets;
mod symbols;
mod temporals;
mod typed_arrays;
mod weak_collections;
mod weak_references;
pub(crate) use iterators::PreparedIteratorResultPlan;
pub use limits::{RuntimeLimits, RuntimeUsage};
pub(crate) use typed_arrays::{
    TypedArrayElementValue, TypedArrayOwnProperty, TypedArrayPropertyKey, TypedArrayStoreOutcome,
    TypedArrayView,
};

struct RealmState {
    object_prototype: ObjectId,
    global_object: ObjectId,
    intrinsics: RealmIntrinsics,
    global_bindings: HashMap<Atom, RealmGlobalBindingId>,
    /// Realm-local state for the implementation-defined `%Math.random%`
    /// pseudorandom sequence. Xorshift64* requires a non-zero state.
    math_random_state: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "realm construction needs one non-ready sentinel while the ready intrinsic table stays inline, Copy, and allocation-free"
)]
enum RealmIntrinsics {
    Initializing,
    Ready {
        function_prototype: FunctionId,
        throw_type_error: FunctionId,
        function_constructor: FunctionId,
        errors: ErrorIntrinsics,
        boolean: BooleanIntrinsics,
        number: NumberIntrinsics,
        bigint: BigIntIntrinsics,
        string: StringIntrinsics,
        array: ArrayIntrinsics,
        array_buffer: ArrayBufferIntrinsics,
        shared_array_buffer: SharedArrayBufferIntrinsics,
        data_view: DataViewIntrinsics,
        typed_array: TypedArrayIntrinsics,
        date: DateIntrinsics,
        temporal: TemporalIntrinsics,
        map: MapIntrinsics,
        set: SetIntrinsics,
        weak_map: WeakMapIntrinsics,
        weak_set: WeakSetIntrinsics,
        weak_ref: WeakRefIntrinsics,
        finalization_registry: FinalizationRegistryIntrinsics,
        promise: PromiseIntrinsics,
        regexp: RegExpIntrinsics,
        symbol: SymbolIntrinsics,
        iterators: IteratorIntrinsics,
        generators: GeneratorIntrinsics,
        async_functions: AsyncFunctionIntrinsics,
        async_generators: AsyncGeneratorIntrinsics,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ErrorIntrinsicKind {
    Error,
    EvalError,
    RangeError,
    ReferenceError,
    SyntaxError,
    TypeError,
    UriError,
    InternalError,
    AggregateError,
}

impl ErrorIntrinsicKind {
    pub(crate) const ALL: [Self; 9] = [
        Self::Error,
        Self::EvalError,
        Self::RangeError,
        Self::ReferenceError,
        Self::SyntaxError,
        Self::TypeError,
        Self::UriError,
        Self::InternalError,
        Self::AggregateError,
    ];

    const fn public_kind(self) -> ErrorObjectKind {
        match self {
            Self::Error => ErrorObjectKind::Error,
            Self::EvalError => ErrorObjectKind::EvalError,
            Self::RangeError => ErrorObjectKind::RangeError,
            Self::ReferenceError => ErrorObjectKind::ReferenceError,
            Self::SyntaxError => ErrorObjectKind::SyntaxError,
            Self::TypeError => ErrorObjectKind::TypeError,
            Self::UriError => ErrorObjectKind::UriError,
            Self::InternalError => ErrorObjectKind::InternalError,
            Self::AggregateError => ErrorObjectKind::AggregateError,
        }
    }

    #[cfg(test)]
    const fn name(self) -> &'static str {
        self.public_kind().constructor_name()
    }

    const fn index(self) -> usize {
        match self {
            Self::Error => 0,
            Self::EvalError => 1,
            Self::RangeError => 2,
            Self::ReferenceError => 3,
            Self::SyntaxError => 4,
            Self::TypeError => 5,
            Self::UriError => 6,
            Self::InternalError => 7,
            Self::AggregateError => 8,
        }
    }

    const fn predefined_atom(self) -> PredefinedAtom {
        match self {
            Self::Error => PredefinedAtom::Error,
            Self::EvalError => PredefinedAtom::EvalError,
            Self::RangeError => PredefinedAtom::RangeError,
            Self::ReferenceError => PredefinedAtom::ReferenceError,
            Self::SyntaxError => PredefinedAtom::SyntaxError,
            Self::TypeError => PredefinedAtom::TypeError,
            Self::UriError => PredefinedAtom::UriError,
            Self::InternalError => PredefinedAtom::InternalError,
            Self::AggregateError => PredefinedAtom::AggregateError,
        }
    }

    const fn from_exception_kind(kind: ExceptionKind) -> Self {
        match kind {
            ExceptionKind::InternalError => Self::InternalError,
            ExceptionKind::RangeError => Self::RangeError,
            ExceptionKind::ReferenceError => Self::ReferenceError,
            ExceptionKind::SyntaxError => Self::SyntaxError,
            ExceptionKind::TypeError => Self::TypeError,
            ExceptionKind::UriError => Self::UriError,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ErrorIntrinsic {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ErrorIntrinsics {
    entries: [ErrorIntrinsic; ErrorIntrinsicKind::ALL.len()],
    to_string: FunctionId,
    is_error: FunctionId,
}

impl ErrorIntrinsics {
    const fn intrinsic(self, kind: ErrorIntrinsicKind) -> ErrorIntrinsic {
        self.entries[kind.index()]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BooleanIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NumberIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

/// The realm's `BigInt` constructor and prototype.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BigIntIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StringIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArrayIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArrayBufferIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SharedArrayBufferIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DataViewIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypedArrayIntrinsics {
    prototype: ObjectId,
    instance_prototypes: [ObjectId; 12],
    constructors: [FunctionId; 12],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DateIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TemporalIntrinsics {
    namespace: ObjectId,
    duration_prototype: ObjectId,
    duration_constructor: FunctionId,
    instant_prototype: ObjectId,
    instant_constructor: FunctionId,
    plain_date_prototype: ObjectId,
    plain_date_constructor: FunctionId,
    plain_date_time_prototype: ObjectId,
    plain_date_time_constructor: FunctionId,
    plain_time_prototype: ObjectId,
    plain_time_constructor: FunctionId,
    plain_month_day_prototype: ObjectId,
    plain_month_day_constructor: FunctionId,
    plain_year_month_prototype: ObjectId,
    plain_year_month_constructor: FunctionId,
    zoned_date_time_prototype: ObjectId,
    zoned_date_time_constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MapIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
    iterator_prototype: ObjectId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SetIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
    iterator_prototype: ObjectId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WeakMapIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WeakSetIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WeakRefIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalizationRegistryIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PromiseIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegExpIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SymbolIntrinsics {
    prototype: ObjectId,
    constructor: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "the intrinsic names make each hidden iterator prototype ownership edge explicit"
)]
struct IteratorIntrinsics {
    constructor: FunctionId,
    iterator_prototype: ObjectId,
    helper_prototype: ObjectId,
    wrapper_prototype: ObjectId,
    async_iterator_prototype: ObjectId,
    async_from_sync_iterator_prototype: ObjectId,
    async_from_sync_iterator_next: FunctionId,
    array_iterator_prototype: ObjectId,
    string_iterator_prototype: ObjectId,
    regexp_string_iterator_prototype: ObjectId,
    array_values: FunctionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GeneratorIntrinsics {
    function_constructor: FunctionId,
    function_prototype: ObjectId,
    generator_prototype: ObjectId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AsyncFunctionIntrinsics {
    function_constructor: FunctionId,
    function_prototype: ObjectId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AsyncGeneratorIntrinsics {
    function_constructor: FunctionId,
    function_prototype: ObjectId,
    generator_prototype: ObjectId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayDefineOutcome {
    Complete,
    ReadOnlyLength,
    NonExtensible,
}

/// The outcome of ECMAScript `OrdinarySetPrototypeOf`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetPrototypeOutcome {
    /// The prototype was installed, or already had the requested value.
    Complete,
    /// The object is not extensible and the prototype differs.
    NonExtensible,
    /// The requested prototype chain already reaches the target.
    CyclicPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayLengthWriteOutcome {
    Complete,
    ReadOnly,
    BlockedByNonConfigurable {
        index: ArrayIndex,
        final_length: u32,
    },
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    reason = "the prior closed-range check and exact round trip are the required Array length test"
)]
pub(crate) fn array_length_from_number(value: JsNumber) -> Option<u32> {
    let value = value.as_f64();
    if !(0.0..=f64::from(u32::MAX)).contains(&value) {
        return None;
    }
    let length = value as u32;
    (f64::from(length) == value).then_some(length)
}

#[cfg(test)]
pub(crate) enum ForInAdvance {
    Continue { work: u64 },
    Yield { key: PropertyKey, work: u64 },
    Done { work: u64 },
}

#[cfg(test)]
impl ForInAdvance {
    pub(crate) const fn work(&self) -> u64 {
        match self {
            Self::Continue { work } | Self::Yield { work, .. } | Self::Done { work } => *work,
        }
    }
}

struct RealmHandle {
    owner: Weak<ReleaseMailbox>,
    id: RealmId,
}

/// A cloned handle to one runtime-local realm.
///
/// Realm state stays in the uniquely owned `Runtime`; only this immutable
/// identity header uses [`Arc`].
///
/// ```compile_fail
/// use quickjs_runtime::Realm;
///
/// fn require_send<T: Send>() {}
/// require_send::<Realm>();
/// ```
#[derive(Clone)]
pub struct Realm(Arc<RealmHandle>);

impl std::fmt::Debug for Realm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Realm")
            .field("index", &self.0.id.index())
            .field("generation", &self.0.id.generation())
            .field("orphaned", &self.0.owner.upgrade().is_none())
            .finish()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum FrameBindingAddress {
    Argument(u32),
    Local(u32),
}

pub(crate) enum InstalledConstant {
    Number(JsNumber),
    String(JsString),
    BigInt(Arc<JsBigInt>),
    TemplateObject(InstalledTemplateObject),
    Function(FunctionTemplateId),
}

#[derive(Clone)]
pub(crate) struct InstalledTemplateElement {
    pub(crate) cooked: Option<JsString>,
    pub(crate) raw: JsString,
}

pub(crate) struct InstalledTemplateObject {
    pub(crate) elements: Arc<[InstalledTemplateElement]>,
    pub(crate) object: Option<ObjectId>,
}

pub(crate) struct InstalledTemplate {
    pub(crate) atoms: Vec<Atom>,
    pub(crate) constants: Vec<InstalledConstant>,
    pub(crate) own_cell_bindings: Vec<FrameBindingAddress>,
    pub(crate) mapped_arguments: Option<Arc<[u32]>>,
}

pub(crate) struct InstalledCode {
    pub(crate) authority: Arc<VerifiedBytecode>,
    pub(crate) realm: RealmId,
    pub(crate) templates: Vec<InstalledTemplate>,
    pub(crate) live_functions: u64,
}

pub(crate) struct BytecodeFunction {
    pub(crate) code: InstalledCodeId,
    pub(crate) template: FunctionTemplateId,
    pub(crate) environment: Vec<EnvironmentBinding>,
    pub(crate) lexical_receiver: Option<StoredValue>,
    pub(crate) lexical_new_target: Option<FunctionId>,
    /// The ECMAScript `[[HomeObject]]` installed when this closure becomes a
    /// class method, class constructor, or object-literal method.  It is an
    /// internal GC edge, not a JavaScript-visible property.
    pub(crate) home_object: Option<HeapReference>,
}

/// Which decimal rendering a `Number.prototype` method performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumberFormat {
    Fixed,
    Exponential,
    Precision,
}

impl NumberFormat {
    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fixed => "toFixed",
            Self::Exponential => "toExponential",
            Self::Precision => "toPrecision",
        }
    }
}

/// Which reduction a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayReduction {
    Reduce,
    ReduceRight,
}

impl ArrayReduction {
    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Reduce => "reduce",
            Self::ReduceRight => "reduceRight",
        }
    }

    /// Returns whether the reduction walks the indices in descending order.
    pub(crate) const fn is_backward(self) -> bool {
        matches!(self, Self::ReduceRight)
    }
}

/// Which callback method a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayCallback {
    ForEach,
    Map,
    Filter,
    Every,
    Some,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
}

impl ArrayCallback {
    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ForEach => "forEach",
            Self::Map => "map",
            Self::Filter => "filter",
            Self::Every => "every",
            Self::Some => "some",
            Self::Find => "find",
            Self::FindIndex => "findIndex",
            Self::FindLast => "findLast",
            Self::FindLastIndex => "findLastIndex",
        }
    }

    /// Returns whether a missing index is skipped rather than visited.
    pub(crate) const fn skips_holes(self) -> bool {
        !matches!(
            self,
            Self::Find | Self::FindIndex | Self::FindLast | Self::FindLastIndex
        )
    }

    /// Returns whether the loop walks the indices in descending order.
    pub(crate) const fn is_backward(self) -> bool {
        matches!(self, Self::FindLast | Self::FindLastIndex)
    }

    /// Returns whether the method builds a fresh Array.
    pub(crate) const fn builds_array(self) -> bool {
        matches!(self, Self::Map | Self::Filter)
    }
}

/// Which copying method a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayCopier {
    Slice,
    Concat,
    At,
    ToReversed,
    ToSpliced,
    With,
}

impl ArrayCopier {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            Self::Slice | Self::ToSpliced | Self::With => 2,
            // `concat` is variadic and `at` takes one index; both report 1.
            Self::Concat | Self::At => 1,
            Self::ToReversed => 0,
        }
    }

    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Slice => "slice",
            Self::Concat => "concat",
            Self::At => "at",
            Self::ToReversed => "toReversed",
            Self::ToSpliced => "toSpliced",
            Self::With => "with",
        }
    }
}

/// Which mutator a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayMutator {
    Push,
    Pop,
    Shift,
    Unshift,
    Reverse,
    Fill,
    CopyWithin,
}

impl ArrayMutator {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            // `push` and `unshift` are variadic but report 1; `fill` reports 1
            // even though it accepts three arguments.
            Self::Push | Self::Unshift | Self::Fill => 1,
            Self::CopyWithin => 2,
            Self::Pop | Self::Shift | Self::Reverse => 0,
        }
    }

    /// Returns the property name this mutator is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Pop => "pop",
            Self::Shift => "shift",
            Self::Unshift => "unshift",
            Self::Reverse => "reverse",
            Self::Fill => "fill",
            Self::CopyWithin => "copyWithin",
        }
    }
}

/// Which `SortIndexedProperties`-based Array method a continuation performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArraySort {
    Sort,
    ToSorted,
}

impl ArraySort {
    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Sort => "sort",
            Self::ToSorted => "toSorted",
        }
    }

    /// Returns whether the method produces a fresh dense Array.
    pub(crate) const fn copies(self) -> bool {
        matches!(self, Self::ToSorted)
    }
}

/// Which `FlattenIntoArray`-based Array method a continuation performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayFlatten {
    Flat,
    FlatMap,
}

/// Which no-`Intl` locale-string built-in is being invoked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocaleStringMethod {
    Object,
    Number,
    BigInt,
    Array,
}

impl ArrayFlatten {
    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::FlatMap => "flatMap",
        }
    }

    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            Self::Flat => 0,
            Self::FlatMap => 1,
        }
    }

    /// Returns whether the root flattening call applies a mapper.
    pub(crate) const fn maps(self) -> bool {
        matches!(self, Self::FlatMap)
    }
}

/// Which search a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArraySearch {
    IndexOf,
    LastIndexOf,
    Includes,
}

impl ArraySearch {
    /// Returns whether a missing index is skipped rather than read as
    /// `undefined`.
    pub(crate) const fn skips_holes(self) -> bool {
        matches!(self, Self::IndexOf | Self::LastIndexOf)
    }

    /// Returns whether the search walks the indices in descending order.
    pub(crate) const fn is_backward(self) -> bool {
        matches!(self, Self::LastIndexOf)
    }

    /// Returns whether the result is a Boolean rather than an index.
    pub(crate) const fn answers_boolean(self) -> bool {
        matches!(self, Self::Includes)
    }
}

/// One `Number` predicate static.
///
/// Each answers `false` for a non-Number argument rather than converting it,
/// which is what separates `Number.isNaN` from the global `isNaN`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumberPredicate {
    IsInteger,
    IsSafeInteger,
    IsFinite,
    IsNaN,
}

/// One of the coercing numeric functions installed on the global object.
///
/// These are deliberately distinct from [`NumberPredicate`]: the global
/// predicates apply `ToNumber`, and the parsers apply the string-prefix
/// algorithms after their observable argument conversions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GlobalNumericFunction {
    IsFinite,
    IsNaN,
    ParseFloat,
    ParseInt,
}

/// One URI handling function installed on the global object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UriFunction {
    DecodeUri,
    DecodeUriComponent,
    EncodeUri,
    EncodeUriComponent,
}

impl UriFunction {
    /// Returns whether this function operates on a URI component rather than
    /// a complete URI.
    pub(crate) const fn is_component(self) -> bool {
        matches!(self, Self::DecodeUriComponent | Self::EncodeUriComponent)
    }

    /// Returns whether this function percent-encodes rather than decodes.
    pub(crate) const fn is_encode(self) -> bool {
        matches!(self, Self::EncodeUri | Self::EncodeUriComponent)
    }
}

/// How one argument is coerced before the method body runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StringArgument {
    /// `ToString`, which an absent argument still applies to `undefined`.
    String,
    /// `ToIntegerOrInfinity`, which an absent argument resolves to `0`.
    Integer,
    /// `ToIntegerOrInfinity`, but an absent or `undefined` argument stays absent.
    ///
    /// This is what separates `"hello".slice(1, undefined)` (`"ello"`) from a
    /// present `0` end position.
    OptionalInteger,
    /// `ToString`, but an absent or `undefined` argument stays absent.
    OptionalString,
    /// `ToNumber`, kept as a Number so `NaN` remains distinguishable.
    Number,
}

/// One `String.prototype` method's identity and argument shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StringMethod {
    At,
    CharAt,
    CharCodeAt,
    CodePointAt,
    Concat,
    EndsWith,
    Includes,
    IndexOf,
    LastIndexOf,
    PadEnd,
    PadStart,
    Repeat,
    /// `String.prototype.match`, whose `@@match` lookup precedes receiver
    /// coercion and whose fallback constructs a fresh intrinsic `RegExp`.
    Match,
    /// `String.prototype.matchAll`, including the global `RegExp` guard and
    /// intrinsic-RegExp fallback.
    MatchAll,
    /// `String.prototype.replace`, whose `@@replace` protocol dispatch must run
    /// before the receiver and fallback arguments are string-coerced.
    Replace,
    /// `String.prototype.replaceAll`, which additionally performs the
    /// observable `IsRegExp` and global-flags checks before `@@replace`.
    ReplaceAll,
    /// `String.prototype.search`, with the same protocol-first shape as
    /// `match` and an intrinsic-RegExp fallback.
    Search,
    /// `String.prototype.split`, whose `@@split` protocol dispatch precedes
    /// the receiver, limit, and fallback separator conversions.
    Split,
    Slice,
    StartsWith,
    Substring,
    Trim,
    TrimEnd,
    TrimStart,
    IsWellFormed,
    ToWellFormed,
    LocaleCompare,
    Normalize,
    ToLocaleLowerCase,
    ToLocaleUpperCase,
    ToLowerCase,
    ToUpperCase,
    /// `String.fromCharCode`, which is a static rather than a prototype method.
    FromCharCode,
    /// `String.fromCodePoint`, likewise a static.
    FromCodePoint,
}

impl StringMethod {
    /// Returns how each declared argument is coerced, in order.
    ///
    /// `concat` is absent because it takes a variable number of arguments and
    /// converts them in its own loop.
    pub(crate) const fn argument_shape(self) -> &'static [StringArgument] {
        match self {
            // A no-argument method and the variadic `concat` share an empty
            // declared shape; `concat` converts its own arguments instead.
            Self::Trim
            | Self::TrimEnd
            | Self::TrimStart
            | Self::IsWellFormed
            | Self::ToWellFormed
            | Self::ToLocaleLowerCase
            | Self::ToLocaleUpperCase
            | Self::ToLowerCase
            | Self::ToUpperCase
            | Self::Concat
            | Self::Match
            | Self::MatchAll
            | Self::Replace
            | Self::ReplaceAll
            | Self::Search
            | Self::Split
            | Self::FromCharCode
            | Self::FromCodePoint => &[],
            Self::At | Self::CharAt | Self::CharCodeAt | Self::CodePointAt => {
                &[StringArgument::Integer]
            }
            // The search argument is converted before the position argument.
            Self::EndsWith | Self::Includes | Self::IndexOf | Self::StartsWith => {
                &[StringArgument::String, StringArgument::OptionalInteger]
            }
            // `lastIndexOf` keeps its position as a Number because `NaN` means
            // "search from the end" rather than "position zero".
            Self::LastIndexOf => &[StringArgument::String, StringArgument::Number],
            Self::PadEnd | Self::PadStart => {
                &[StringArgument::Integer, StringArgument::OptionalString]
            }
            Self::Repeat => &[StringArgument::Integer],
            Self::Slice | Self::Substring => {
                &[StringArgument::Integer, StringArgument::OptionalInteger]
            }
            Self::LocaleCompare => &[StringArgument::String],
            Self::Normalize => &[StringArgument::OptionalString],
        }
    }

    /// Returns whether the method consumes every remaining argument.
    pub(crate) const fn is_variadic(self) -> bool {
        matches!(
            self,
            Self::Concat | Self::FromCharCode | Self::FromCodePoint
        )
    }

    /// Returns whether the method converts its receiver with `ToString`.
    ///
    /// The two `String` statics do not: they are installed on the constructor,
    /// so they ignore their receiver entirely.
    pub(crate) const fn converts_receiver(self) -> bool {
        !matches!(self, Self::FromCharCode | Self::FromCodePoint)
    }

    /// Returns how a variadic method coerces each of its arguments.
    pub(crate) const fn variadic_argument(self) -> StringArgument {
        match self {
            // `fromCharCode` and `fromCodePoint` both need a Number; the width
            // and the validation differ in the body.
            Self::FromCharCode | Self::FromCodePoint => StringArgument::Number,
            _ => StringArgument::String,
        }
    }
}

/// The first specification-order slice of methods installed on `%Math%`.
///
/// Keeping one ordered enum makes the ordinary object's observable own-key
/// order explicit while later tranches append the remaining methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MathMethod {
    Min,
    Max,
    Abs,
    Floor,
    Ceil,
    Round,
    Sqrt,
    Acos,
    Asin,
    Atan,
    Atan2,
    Cos,
    Exp,
    Log,
    Pow,
    Sin,
    Tan,
    Trunc,
    Sign,
    Cosh,
    Sinh,
    Tanh,
    Acosh,
    Asinh,
    Atanh,
    Expm1,
    Log1p,
    Log2,
    Log10,
    Cbrt,
    Hypot,
    Random,
    F16Round,
    FRound,
    Imul,
    Clz32,
    SumPrecise,
}

impl MathMethod {
    pub(crate) const ALL: [Self; 37] = [
        Self::Min,
        Self::Max,
        Self::Abs,
        Self::Floor,
        Self::Ceil,
        Self::Round,
        Self::Sqrt,
        Self::Acos,
        Self::Asin,
        Self::Atan,
        Self::Atan2,
        Self::Cos,
        Self::Exp,
        Self::Log,
        Self::Pow,
        Self::Sin,
        Self::Tan,
        Self::Trunc,
        Self::Sign,
        Self::Cosh,
        Self::Sinh,
        Self::Tanh,
        Self::Acosh,
        Self::Asinh,
        Self::Atanh,
        Self::Expm1,
        Self::Log1p,
        Self::Log2,
        Self::Log10,
        Self::Cbrt,
        Self::Hypot,
        Self::Random,
        Self::F16Round,
        Self::FRound,
        Self::Imul,
        Self::Clz32,
        Self::SumPrecise,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
            Self::Abs => "abs",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Round => "round",
            Self::Sqrt => "sqrt",
            Self::Acos => "acos",
            Self::Asin => "asin",
            Self::Atan => "atan",
            Self::Atan2 => "atan2",
            Self::Cos => "cos",
            Self::Exp => "exp",
            Self::Log => "log",
            Self::Pow => "pow",
            Self::Sin => "sin",
            Self::Tan => "tan",
            Self::Trunc => "trunc",
            Self::Sign => "sign",
            Self::Cosh => "cosh",
            Self::Sinh => "sinh",
            Self::Tanh => "tanh",
            Self::Acosh => "acosh",
            Self::Asinh => "asinh",
            Self::Atanh => "atanh",
            Self::Expm1 => "expm1",
            Self::Log1p => "log1p",
            Self::Log2 => "log2",
            Self::Log10 => "log10",
            Self::Cbrt => "cbrt",
            Self::Hypot => "hypot",
            Self::Random => "random",
            Self::F16Round => "f16round",
            Self::FRound => "fround",
            Self::Imul => "imul",
            Self::Clz32 => "clz32",
            Self::SumPrecise => "sumPrecise",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Min | Self::Max | Self::Atan2 | Self::Pow | Self::Hypot | Self::Imul => 2,
            Self::Random => 0,
            Self::Abs
            | Self::Floor
            | Self::Ceil
            | Self::Round
            | Self::Sqrt
            | Self::Acos
            | Self::Asin
            | Self::Atan
            | Self::Cos
            | Self::Exp
            | Self::Log
            | Self::Sin
            | Self::Tan
            | Self::Trunc
            | Self::Sign
            | Self::Cosh
            | Self::Sinh
            | Self::Tanh
            | Self::Acosh
            | Self::Asinh
            | Self::Atanh
            | Self::Expm1
            | Self::Log1p
            | Self::Log2
            | Self::Log10
            | Self::Cbrt
            | Self::F16Round
            | Self::FRound
            | Self::Clz32
            | Self::SumPrecise => 1,
        }
    }

    pub(crate) const fn is_extrema(self) -> bool {
        matches!(self, Self::Min | Self::Max)
    }

    pub(crate) const fn is_binary(self) -> bool {
        matches!(self, Self::Atan2 | Self::Pow | Self::Imul)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeFunctionKind {
    FunctionPrototype,
    FunctionPrototypeApply,
    FunctionPrototypeCall,
    FunctionPrototypeBind,
    FunctionPrototypeHasInstance,
    ThrowTypeError,
    OrdinaryFunctionConstructor,
    ObjectConstructor,
    ObjectGetPrototypeOf,
    ObjectSetPrototypeOf,
    ObjectPreventExtensions,
    ObjectIsExtensible,
    ObjectSeal,
    ObjectFreeze,
    ObjectIsSealed,
    ObjectIsFrozen,
    ObjectKeys,
    ObjectGetOwnPropertyNames,
    ObjectGetOwnPropertySymbols,
    ObjectDefineProperty,
    ObjectDefineProperties,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyDescriptors,
    ObjectIs,
    ObjectHasOwn,
    ObjectValues,
    ObjectEntries,
    ObjectAssign,
    ObjectFromEntries,
    ObjectGroupBy,
    ObjectCreate,
    ObjectPrototypeToString,
    ObjectPrototypeValueOf,
    ObjectPrototypeHasOwnProperty,
    ObjectPrototypeIsPrototypeOf,
    ObjectPrototypePropertyIsEnumerable,
    /// One method on the ordinary `%Reflect%` object.
    Reflect(ReflectMethod),
    /// The `%Proxy%` constructor.
    ProxyConstructor,
    /// `Proxy.revocable`.
    ProxyRevocable,
    /// `JSON.parse`.
    JsonParse,
    /// `JSON.isRawJSON`.
    JsonIsRawJson,
    /// `JSON.rawJSON`.
    JsonRawJson,
    /// `JSON.stringify`.
    JsonStringify,
    /// One method on the ordinary `%Math%` object.
    Math(MathMethod),
    /// One method on the ordinary `%Atomics%` object.
    Atomics(AtomicsMethod),
    ArrayBufferConstructor,
    ArrayBufferIsView,
    ArrayBufferSpeciesGetter,
    ArrayBufferPrototype(ArrayBufferPrototypeMethod),
    SharedArrayBufferConstructor,
    SharedArrayBufferSpeciesGetter,
    SharedArrayBufferPrototype(SharedArrayBufferPrototypeMethod),
    DataViewConstructor,
    DataViewPrototype(DataViewPrototypeMethod),
    /// The hidden abstract `%TypedArray%` constructor shared by every
    /// concrete typed-array constructor. It is reachable through
    /// `Object.getPrototypeOf(Int8Array)`, but never installed globally.
    TypedArrayBaseConstructor,
    TypedArrayConstructor(TypedArrayElementType),
    TypedArrayStatic(ArrayStatic),
    TypedArraySpeciesGetter,
    TypedArrayPrototype(TypedArrayPrototypeMethod),
    DateConstructor,
    DateStatic(DateStaticMethod),
    DatePrototype(DatePrototypeMethod),
    TemporalDurationConstructor,
    TemporalDurationStatic(TemporalDurationStaticMethod),
    TemporalDurationPrototype(TemporalDurationPrototypeMethod),
    TemporalInstantConstructor,
    TemporalInstantStatic(TemporalInstantStaticMethod),
    TemporalInstantPrototype(TemporalInstantPrototypeMethod),
    TemporalPlainDateConstructor,
    TemporalPlainDateStatic(TemporalPlainDateStaticMethod),
    TemporalPlainDatePrototype(TemporalPlainDatePrototypeMethod),
    TemporalPlainDateTimeConstructor,
    TemporalPlainDateTimeStatic(TemporalPlainDateTimeStaticMethod),
    TemporalPlainDateTimePrototype(TemporalPlainDateTimePrototypeMethod),
    TemporalPlainTimeConstructor,
    TemporalPlainTimeStatic(TemporalPlainTimeStaticMethod),
    TemporalPlainTimePrototype(TemporalPlainTimePrototypeMethod),
    TemporalPlainMonthDayConstructor,
    TemporalPlainMonthDayStatic(TemporalPlainMonthDayStaticMethod),
    TemporalPlainMonthDayPrototype(TemporalPlainMonthDayPrototypeMethod),
    TemporalPlainYearMonthConstructor,
    TemporalPlainYearMonthStatic(TemporalPlainYearMonthStaticMethod),
    TemporalPlainYearMonthPrototype(TemporalPlainYearMonthPrototypeMethod),
    TemporalZonedDateTimeConstructor,
    TemporalZonedDateTimeStatic(TemporalZonedDateTimeStaticMethod),
    TemporalZonedDateTimePrototype(TemporalZonedDateTimePrototypeMethod),
    FunctionPrototypeToString,
    ErrorConstructor(ErrorIntrinsicKind),
    ErrorPrototypeToString,
    ErrorIsError,
    BooleanConstructor,
    BooleanPrototypeToString,
    BooleanPrototypeValueOf,
    NumberConstructor,
    NumberPrototypeToString,
    NumberPrototypeValueOf,
    /// One `Number.prototype` decimal rendering.
    NumberPrototypeFormat(NumberFormat),
    BigIntConstructor,
    BigIntPrototypeToString,
    BigIntPrototypeValueOf,
    BigIntAsIntN,
    BigIntAsUintN,
    StringConstructor,
    StringPrototypeToString,
    StringPrototypeValueOf,
    /// One `String.prototype` method sharing the resumable coercion machine.
    StringPrototypeMethod(StringMethod),
    /// `String.raw`.
    StringRaw,
    /// One `Number` predicate static.
    NumberPredicateStatic(NumberPredicate),
    /// One coercing numeric function on the realm's global object.
    GlobalNumeric(GlobalNumericFunction),
    /// One global URI encoder or decoder.
    GlobalUri(UriFunction),
    /// `Array.isArray`.
    ArrayIsArray,
    /// One generic factory method on the `Array` constructor.
    ArrayStatic(ArrayStatic),
    /// One `Array.prototype` search sharing the resumable element loop.
    ArrayPrototypeSearch(ArraySearch),
    /// One `Array.prototype` mutator sharing the resumable element driver.
    ArrayPrototypeMutator(ArrayMutator),
    /// One `Array.prototype` copying method sharing the resumable element read.
    ArrayPrototypeCopier(ArrayCopier),
    /// One stable `SortIndexedProperties`-based Array method.
    ArrayPrototypeSort(ArraySort),
    /// One `FlattenIntoArray`-based Array method.
    ArrayPrototypeFlatten(ArrayFlatten),
    /// One `Array.prototype` callback method sharing the resumable loop.
    ArrayPrototypeCallback(ArrayCallback),
    /// One `Array.prototype` reduction sharing the resumable fold.
    ArrayPrototypeReduction(ArrayReduction),
    /// `Array.prototype.splice`.
    ArrayPrototypeSplice,
    ArrayConstructor,
    /// The `%Array%[Symbol.species]` getter.
    ArraySpeciesGetter,
    RegExpConstructor,
    RegExpEscape,
    RegExpSpeciesGetter,
    RegExpPrototypeFlags,
    RegExpPrototypeSource,
    RegExpPrototypeFlag(RegExpFlag),
    RegExpPrototypeExec,
    RegExpPrototypeCompile,
    RegExpPrototypeTest,
    RegExpPrototypeToString,
    RegExpPrototypeSymbol(RegExpSymbolMethod),
    SymbolConstructor,
    SymbolPrototypeToString,
    SymbolPrototypeValueOf,
    SymbolPrototypeToPrimitive,
    SymbolPrototypeDescription,
    SymbolFor,
    SymbolKeyFor,
    IteratorConstructor,
    IteratorFrom,
    IteratorPrototypeDrop,
    IteratorPrototypeFilter,
    IteratorPrototypeMap,
    IteratorPrototypeTake,
    IteratorPrototypeToArray,
    IteratorPrototypeConstructorGetter,
    IteratorPrototypeConstructorSetter,
    IteratorPrototypeToStringTagGetter,
    IteratorPrototypeToStringTagSetter,
    IteratorPrototypeIterator,
    IteratorWrapperNext,
    IteratorWrapperReturn,
    IteratorHelperNext,
    IteratorHelperReturn,
    AsyncIteratorPrototypeAsyncIterator,
    AsyncFromSyncIteratorNext,
    AsyncFromSyncIteratorReturn,
    AsyncFromSyncIteratorThrow,
    AsyncFromSyncIteratorUnwrap,
    AsyncFromSyncIteratorClose,
    ArrayPrototypeJoin,
    ArrayPrototypeToString,
    /// One no-`Intl` `toLocaleString` implementation.
    LocaleString(LocaleStringMethod),
    ArrayPrototypeValues,
    ArrayPrototypeKeys,
    ArrayPrototypeEntries,
    ArrayIteratorNext,
    MapConstructor,
    MapGroupBy,
    MapSpeciesGetter,
    MapPrototype(MapMethod),
    MapIteratorNext,
    SetConstructor,
    SetGroupBy,
    SetSpeciesGetter,
    SetPrototype(SetMethod),
    SetIteratorNext,
    WeakMapConstructor,
    WeakMapPrototype(WeakMapMethod),
    WeakSetConstructor,
    WeakSetPrototype(WeakSetMethod),
    WeakRefConstructor,
    WeakRefPrototypeDeref,
    FinalizationRegistryConstructor,
    FinalizationRegistryPrototype(FinalizationRegistryMethod),
    StringPrototypeIterator,
    StringIteratorNext,
    RegExpStringIteratorNext,
    GeneratorFunctionConstructor,
    AsyncFunctionConstructor,
    GeneratorPrototypeNext,
    GeneratorPrototypeReturn,
    GeneratorPrototypeThrow,
    AsyncGeneratorFunctionConstructor,
    AsyncGeneratorPrototypeNext,
    AsyncGeneratorPrototypeReturn,
    AsyncGeneratorPrototypeThrow,
    PromiseConstructor,
    PromiseResolve,
    PromiseReject,
    PromiseStatic(PromiseStatic),
    PromiseSpeciesGetter,
    PromisePrototypeThen,
    PromisePrototypeCatch,
    PromisePrototypeFinally,
}

/// Synchronous operations exposed by the `%Atomics%` namespace.
///
/// `waitAsync` is deliberately absent from this enumeration until the runtime
/// owns a spec-ordered waiter and Promise-job scheduler. `wait` and `notify`
/// provide the single-agent synchronous semantics; multi-agent wakeups remain
/// a host-agent capability rather than Tokio-scheduled JavaScript jobs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomicsMethod {
    Add,
    And,
    CompareExchange,
    Exchange,
    IsLockFree,
    Load,
    Notify,
    Or,
    Store,
    Sub,
    Wait,
    Xor,
    Pause,
}

impl AtomicsMethod {
    pub(crate) const ALL: [Self; 13] = [
        Self::Add,
        Self::And,
        Self::CompareExchange,
        Self::Exchange,
        Self::IsLockFree,
        Self::Load,
        Self::Notify,
        Self::Or,
        Self::Store,
        Self::Sub,
        Self::Wait,
        Self::Xor,
        Self::Pause,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::And => "and",
            Self::CompareExchange => "compareExchange",
            Self::Exchange => "exchange",
            Self::IsLockFree => "isLockFree",
            Self::Load => "load",
            Self::Notify => "notify",
            Self::Or => "or",
            Self::Store => "store",
            Self::Sub => "sub",
            Self::Wait => "wait",
            Self::Xor => "xor",
            Self::Pause => "pause",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::CompareExchange | Self::Wait => 4,
            Self::Add
            | Self::And
            | Self::Exchange
            | Self::Notify
            | Self::Or
            | Self::Store
            | Self::Sub
            | Self::Xor => 3,
            Self::IsLockFree => 1,
            Self::Load => 2,
            Self::Pause => 0,
        }
    }

    pub(crate) const fn requires_value(self) -> bool {
        matches!(
            self,
            Self::Add
                | Self::And
                | Self::CompareExchange
                | Self::Exchange
                | Self::Or
                | Self::Store
                | Self::Sub
                | Self::Wait
                | Self::Xor
                | Self::Notify
        )
    }

    pub(crate) const fn requires_waitable_element(self) -> bool {
        matches!(self, Self::Notify | Self::Wait)
    }

    pub(crate) const fn requires_shared_buffer(self) -> bool {
        matches!(self, Self::Wait)
    }

    pub(crate) const fn requires_writable_buffer(self) -> bool {
        matches!(
            self,
            Self::Add
                | Self::And
                | Self::CompareExchange
                | Self::Exchange
                | Self::Or
                | Self::Store
                | Self::Sub
                | Self::Xor
        )
    }
}

/// Static methods on `%Date%` in pinned `QuickJS` publication order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DateStaticMethod {
    Now,
    Parse,
    Utc,
}

impl DateStaticMethod {
    pub(crate) const ALL: [Self; 3] = [Self::Now, Self::Parse, Self::Utc];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Parse => "parse",
            Self::Utc => "UTC",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Now => 0,
            Self::Parse => 1,
            Self::Utc => 7,
        }
    }
}

/// Implemented methods on `%Date.prototype%` in pinned `QuickJS` publication order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DatePrototypeMethod {
    ValueOf,
    ToString,
    ToUtcString,
    ToIsoString,
    ToDateString,
    ToTimeString,
    ToLocaleString,
    ToLocaleDateString,
    ToLocaleTimeString,
    GetTimezoneOffset,
    GetTime,
    GetFullYear,
    GetUtcFullYear,
    GetMonth,
    GetUtcMonth,
    GetDate,
    GetUtcDate,
    GetHours,
    GetUtcHours,
    GetMinutes,
    GetUtcMinutes,
    GetSeconds,
    GetUtcSeconds,
    GetMilliseconds,
    GetUtcMilliseconds,
    GetDay,
    GetUtcDay,
    SetTime,
    SetMilliseconds,
    SetUtcMilliseconds,
    SetSeconds,
    SetUtcSeconds,
    SetMinutes,
    SetUtcMinutes,
    SetHours,
    SetUtcHours,
    SetDate,
    SetUtcDate,
    SetMonth,
    SetUtcMonth,
    SetFullYear,
    SetUtcFullYear,
    ToTemporalInstant,
    ToJson,
    SymbolToPrimitive,
}

impl DatePrototypeMethod {
    pub(crate) const ALL: [Self; 45] = [
        Self::ValueOf,
        Self::ToString,
        Self::ToUtcString,
        Self::ToIsoString,
        Self::ToDateString,
        Self::ToTimeString,
        Self::ToLocaleString,
        Self::ToLocaleDateString,
        Self::ToLocaleTimeString,
        Self::GetTimezoneOffset,
        Self::GetTime,
        Self::GetFullYear,
        Self::GetUtcFullYear,
        Self::GetMonth,
        Self::GetUtcMonth,
        Self::GetDate,
        Self::GetUtcDate,
        Self::GetHours,
        Self::GetUtcHours,
        Self::GetMinutes,
        Self::GetUtcMinutes,
        Self::GetSeconds,
        Self::GetUtcSeconds,
        Self::GetMilliseconds,
        Self::GetUtcMilliseconds,
        Self::GetDay,
        Self::GetUtcDay,
        Self::SetTime,
        Self::SetMilliseconds,
        Self::SetUtcMilliseconds,
        Self::SetSeconds,
        Self::SetUtcSeconds,
        Self::SetMinutes,
        Self::SetUtcMinutes,
        Self::SetHours,
        Self::SetUtcHours,
        Self::SetDate,
        Self::SetUtcDate,
        Self::SetMonth,
        Self::SetUtcMonth,
        Self::SetFullYear,
        Self::SetUtcFullYear,
        Self::ToTemporalInstant,
        Self::ToJson,
        Self::SymbolToPrimitive,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ValueOf => "valueOf",
            Self::ToString => "toString",
            Self::ToUtcString => "toUTCString",
            Self::ToIsoString => "toISOString",
            Self::ToDateString => "toDateString",
            Self::ToTimeString => "toTimeString",
            Self::ToLocaleString => "toLocaleString",
            Self::ToLocaleDateString => "toLocaleDateString",
            Self::ToLocaleTimeString => "toLocaleTimeString",
            Self::GetTimezoneOffset => "getTimezoneOffset",
            Self::GetTime => "getTime",
            Self::GetFullYear => "getFullYear",
            Self::GetUtcFullYear => "getUTCFullYear",
            Self::GetMonth => "getMonth",
            Self::GetUtcMonth => "getUTCMonth",
            Self::GetDate => "getDate",
            Self::GetUtcDate => "getUTCDate",
            Self::GetHours => "getHours",
            Self::GetUtcHours => "getUTCHours",
            Self::GetMinutes => "getMinutes",
            Self::GetUtcMinutes => "getUTCMinutes",
            Self::GetSeconds => "getSeconds",
            Self::GetUtcSeconds => "getUTCSeconds",
            Self::GetMilliseconds => "getMilliseconds",
            Self::GetUtcMilliseconds => "getUTCMilliseconds",
            Self::GetDay => "getDay",
            Self::GetUtcDay => "getUTCDay",
            Self::SetTime => "setTime",
            Self::SetMilliseconds => "setMilliseconds",
            Self::SetUtcMilliseconds => "setUTCMilliseconds",
            Self::SetSeconds => "setSeconds",
            Self::SetUtcSeconds => "setUTCSeconds",
            Self::SetMinutes => "setMinutes",
            Self::SetUtcMinutes => "setUTCMinutes",
            Self::SetHours => "setHours",
            Self::SetUtcHours => "setUTCHours",
            Self::SetDate => "setDate",
            Self::SetUtcDate => "setUTCDate",
            Self::SetMonth => "setMonth",
            Self::SetUtcMonth => "setUTCMonth",
            Self::SetFullYear => "setFullYear",
            Self::SetUtcFullYear => "setUTCFullYear",
            Self::ToTemporalInstant => "toTemporalInstant",
            Self::ToJson => "toJSON",
            Self::SymbolToPrimitive => "[Symbol.toPrimitive]",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::SetHours | Self::SetUtcHours => 4,
            Self::SetMinutes | Self::SetUtcMinutes | Self::SetFullYear | Self::SetUtcFullYear => 3,
            Self::SetSeconds | Self::SetUtcSeconds | Self::SetMonth | Self::SetUtcMonth => 2,
            Self::SetTime
            | Self::SetMilliseconds
            | Self::SetUtcMilliseconds
            | Self::SetDate
            | Self::SetUtcDate
            | Self::ToJson
            | Self::SymbolToPrimitive => 1,
            Self::ValueOf
            | Self::ToString
            | Self::ToUtcString
            | Self::ToIsoString
            | Self::ToDateString
            | Self::ToTimeString
            | Self::ToLocaleString
            | Self::ToLocaleDateString
            | Self::ToLocaleTimeString
            | Self::GetTimezoneOffset
            | Self::GetTime
            | Self::GetFullYear
            | Self::GetUtcFullYear
            | Self::GetMonth
            | Self::GetUtcMonth
            | Self::GetDate
            | Self::GetUtcDate
            | Self::GetHours
            | Self::GetUtcHours
            | Self::GetMinutes
            | Self::GetUtcMinutes
            | Self::GetSeconds
            | Self::GetUtcSeconds
            | Self::GetMilliseconds
            | Self::GetUtcMilliseconds
            | Self::GetDay
            | Self::GetUtcDay
            | Self::ToTemporalInstant => 0,
        }
    }
}

/// Methods and accessors published on `%ArrayBuffer.prototype%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayBufferPrototypeMethod {
    ByteLength,
    Detached,
    Immutable,
    MaxByteLength,
    Resizable,
    Resize,
    Slice,
    SliceToImmutable,
    Transfer,
    TransferToFixedLength,
    TransferToImmutable,
}

/// Methods and accessors published on `%SharedArrayBuffer.prototype%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SharedArrayBufferPrototypeMethod {
    ByteLength,
    Growable,
    MaxByteLength,
    Grow,
    Slice,
}

impl SharedArrayBufferPrototypeMethod {
    pub(crate) const ALL: [Self; 5] = [
        Self::ByteLength,
        Self::Growable,
        Self::MaxByteLength,
        Self::Grow,
        Self::Slice,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ByteLength => "byteLength",
            Self::Growable => "growable",
            Self::MaxByteLength => "maxByteLength",
            Self::Grow => "grow",
            Self::Slice => "slice",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Grow => 1,
            Self::Slice => 2,
            Self::ByteLength | Self::Growable | Self::MaxByteLength => 0,
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::ByteLength | Self::Growable | Self::MaxByteLength
        )
    }
}

/// Element representations shared by `DataView` methods and the later
/// typed-array indexed exotic implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DataViewElementType {
    BigInt64,
    BigUint64,
    Float16,
    Float32,
    Float64,
    Int8,
    Int16,
    Int32,
    Uint8,
    Uint16,
    Uint32,
}

impl DataViewElementType {
    #[must_use]
    pub(crate) const fn byte_width(self) -> usize {
        match self {
            Self::BigInt64 | Self::BigUint64 | Self::Float64 => 8,
            Self::Float32 | Self::Int32 | Self::Uint32 => 4,
            Self::Float16 | Self::Int16 | Self::Uint16 => 2,
            Self::Int8 | Self::Uint8 => 1,
        }
    }

    #[must_use]
    pub(crate) const fn is_bigint(self) -> bool {
        matches!(self, Self::BigInt64 | Self::BigUint64)
    }
}

/// Methods and accessors published on `%DataView.prototype%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DataViewPrototypeMethod {
    Buffer,
    ByteLength,
    ByteOffset,
    GetBigInt64,
    GetBigUint64,
    GetFloat16,
    GetFloat32,
    GetFloat64,
    GetInt8,
    GetInt16,
    GetInt32,
    GetUint8,
    GetUint16,
    GetUint32,
    SetBigInt64,
    SetBigUint64,
    SetFloat16,
    SetFloat32,
    SetFloat64,
    SetInt8,
    SetInt16,
    SetInt32,
    SetUint8,
    SetUint16,
    SetUint32,
}

impl DataViewPrototypeMethod {
    pub(crate) const ALL: [Self; 25] = [
        Self::Buffer,
        Self::ByteLength,
        Self::ByteOffset,
        Self::GetBigInt64,
        Self::GetBigUint64,
        Self::GetFloat16,
        Self::GetFloat32,
        Self::GetFloat64,
        Self::GetInt8,
        Self::GetInt16,
        Self::GetInt32,
        Self::GetUint8,
        Self::GetUint16,
        Self::GetUint32,
        Self::SetBigInt64,
        Self::SetBigUint64,
        Self::SetFloat16,
        Self::SetFloat32,
        Self::SetFloat64,
        Self::SetInt8,
        Self::SetInt16,
        Self::SetInt32,
        Self::SetUint8,
        Self::SetUint16,
        Self::SetUint32,
    ];

    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Buffer => "buffer",
            Self::ByteLength => "byteLength",
            Self::ByteOffset => "byteOffset",
            Self::GetBigInt64 => "getBigInt64",
            Self::GetBigUint64 => "getBigUint64",
            Self::GetFloat16 => "getFloat16",
            Self::GetFloat32 => "getFloat32",
            Self::GetFloat64 => "getFloat64",
            Self::GetInt8 => "getInt8",
            Self::GetInt16 => "getInt16",
            Self::GetInt32 => "getInt32",
            Self::GetUint8 => "getUint8",
            Self::GetUint16 => "getUint16",
            Self::GetUint32 => "getUint32",
            Self::SetBigInt64 => "setBigInt64",
            Self::SetBigUint64 => "setBigUint64",
            Self::SetFloat16 => "setFloat16",
            Self::SetFloat32 => "setFloat32",
            Self::SetFloat64 => "setFloat64",
            Self::SetInt8 => "setInt8",
            Self::SetInt16 => "setInt16",
            Self::SetInt32 => "setInt32",
            Self::SetUint8 => "setUint8",
            Self::SetUint16 => "setUint16",
            Self::SetUint32 => "setUint32",
        }
    }

    #[must_use]
    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Buffer | Self::ByteLength | Self::ByteOffset => 0,
            Self::GetBigInt64
            | Self::GetBigUint64
            | Self::GetFloat16
            | Self::GetFloat32
            | Self::GetFloat64
            | Self::GetInt8
            | Self::GetInt16
            | Self::GetInt32
            | Self::GetUint8
            | Self::GetUint16
            | Self::GetUint32 => 1,
            Self::SetBigInt64
            | Self::SetBigUint64
            | Self::SetFloat16
            | Self::SetFloat32
            | Self::SetFloat64
            | Self::SetInt8
            | Self::SetInt16
            | Self::SetInt32
            | Self::SetUint8
            | Self::SetUint16
            | Self::SetUint32 => 2,
        }
    }

    #[must_use]
    pub(crate) const fn is_accessor(self) -> bool {
        matches!(self, Self::Buffer | Self::ByteLength | Self::ByteOffset)
    }

    #[must_use]
    pub(crate) const fn is_setter(self) -> bool {
        matches!(
            self,
            Self::SetBigInt64
                | Self::SetBigUint64
                | Self::SetFloat16
                | Self::SetFloat32
                | Self::SetFloat64
                | Self::SetInt8
                | Self::SetInt16
                | Self::SetInt32
                | Self::SetUint8
                | Self::SetUint16
                | Self::SetUint32
        )
    }

    #[must_use]
    pub(crate) const fn element_type(self) -> Option<DataViewElementType> {
        match self {
            Self::Buffer | Self::ByteLength | Self::ByteOffset => None,
            Self::GetBigInt64 | Self::SetBigInt64 => Some(DataViewElementType::BigInt64),
            Self::GetBigUint64 | Self::SetBigUint64 => Some(DataViewElementType::BigUint64),
            Self::GetFloat16 | Self::SetFloat16 => Some(DataViewElementType::Float16),
            Self::GetFloat32 | Self::SetFloat32 => Some(DataViewElementType::Float32),
            Self::GetFloat64 | Self::SetFloat64 => Some(DataViewElementType::Float64),
            Self::GetInt8 | Self::SetInt8 => Some(DataViewElementType::Int8),
            Self::GetInt16 | Self::SetInt16 => Some(DataViewElementType::Int16),
            Self::GetInt32 | Self::SetInt32 => Some(DataViewElementType::Int32),
            Self::GetUint8 | Self::SetUint8 => Some(DataViewElementType::Uint8),
            Self::GetUint16 | Self::SetUint16 => Some(DataViewElementType::Uint16),
            Self::GetUint32 | Self::SetUint32 => Some(DataViewElementType::Uint32),
        }
    }
}

/// Accessors shared by every concrete typed-array prototype through
/// `%TypedArray%.prototype`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedArrayPrototypeMethod {
    Buffer,
    ByteLength,
    ByteOffset,
    Length,
    ToStringTag,
    Set,
    Subarray,
    At,
    Includes,
    IndexOf,
    LastIndexOf,
    Fill,
    CopyWithin,
    Reverse,
    Slice,
    Sort,
    Entries,
    Keys,
    Values,
    Join,
    ToReversed,
    ToSorted,
    With,
    Every,
    Filter,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
    ForEach,
    Map,
    Reduce,
    ReduceRight,
    Some,
}

impl TypedArrayPrototypeMethod {
    pub(crate) const ALL: [Self; 34] = [
        Self::Buffer,
        Self::ByteLength,
        Self::ByteOffset,
        Self::Length,
        Self::ToStringTag,
        Self::Set,
        Self::Subarray,
        Self::At,
        Self::Includes,
        Self::IndexOf,
        Self::LastIndexOf,
        Self::Fill,
        Self::CopyWithin,
        Self::Reverse,
        Self::Slice,
        Self::Sort,
        Self::Entries,
        Self::Keys,
        Self::Values,
        Self::Join,
        Self::ToReversed,
        Self::ToSorted,
        Self::With,
        Self::Every,
        Self::Filter,
        Self::Find,
        Self::FindIndex,
        Self::FindLast,
        Self::FindLastIndex,
        Self::ForEach,
        Self::Map,
        Self::Reduce,
        Self::ReduceRight,
        Self::Some,
    ];

    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Buffer => "buffer",
            Self::ByteLength => "byteLength",
            Self::ByteOffset => "byteOffset",
            Self::Length => "length",
            Self::ToStringTag => "[Symbol.toStringTag]",
            Self::Set => "set",
            Self::Subarray => "subarray",
            Self::At => "at",
            Self::Includes => "includes",
            Self::IndexOf => "indexOf",
            Self::LastIndexOf => "lastIndexOf",
            Self::Fill => "fill",
            Self::CopyWithin => "copyWithin",
            Self::Reverse => "reverse",
            Self::Slice => "slice",
            Self::Sort => "sort",
            Self::Entries => "entries",
            Self::Keys => "keys",
            Self::Values => "values",
            Self::Join => "join",
            Self::ToReversed => "toReversed",
            Self::ToSorted => "toSorted",
            Self::With => "with",
            Self::Every => "every",
            Self::Filter => "filter",
            Self::Find => "find",
            Self::FindIndex => "findIndex",
            Self::FindLast => "findLast",
            Self::FindLastIndex => "findLastIndex",
            Self::ForEach => "forEach",
            Self::Map => "map",
            Self::Reduce => "reduce",
            Self::ReduceRight => "reduceRight",
            Self::Some => "some",
        }
    }

    #[must_use]
    pub(crate) const fn accessor_name(self) -> &'static str {
        match self {
            Self::Buffer => "get buffer",
            Self::ByteLength => "get byteLength",
            Self::ByteOffset => "get byteOffset",
            Self::Length => "get length",
            Self::ToStringTag => "get [Symbol.toStringTag]",
            Self::Set => "set",
            Self::Subarray => "subarray",
            Self::At => "at",
            Self::Includes => "includes",
            Self::IndexOf => "indexOf",
            Self::LastIndexOf => "lastIndexOf",
            Self::Fill => "fill",
            Self::CopyWithin => "copyWithin",
            Self::Reverse => "reverse",
            Self::Slice => "slice",
            Self::Sort => "sort",
            Self::Entries => "entries",
            Self::Keys => "keys",
            Self::Values => "values",
            Self::Join => "join",
            Self::ToReversed => "toReversed",
            Self::ToSorted => "toSorted",
            Self::With => "with",
            Self::Every => "every",
            Self::Filter => "filter",
            Self::Find => "find",
            Self::FindIndex => "findIndex",
            Self::FindLast => "findLast",
            Self::FindLastIndex => "findLastIndex",
            Self::ForEach => "forEach",
            Self::Map => "map",
            Self::Reduce => "reduce",
            Self::ReduceRight => "reduceRight",
            Self::Some => "some",
        }
    }

    #[must_use]
    pub(crate) const fn is_accessor(self) -> bool {
        !matches!(
            self,
            Self::Set
                | Self::Subarray
                | Self::At
                | Self::Includes
                | Self::IndexOf
                | Self::LastIndexOf
                | Self::Fill
                | Self::CopyWithin
                | Self::Reverse
                | Self::Slice
                | Self::Sort
                | Self::Entries
                | Self::Keys
                | Self::Values
                | Self::Join
                | Self::ToReversed
                | Self::ToSorted
                | Self::With
                | Self::Every
                | Self::Filter
                | Self::Find
                | Self::FindIndex
                | Self::FindLast
                | Self::FindLastIndex
                | Self::ForEach
                | Self::Map
                | Self::Reduce
                | Self::ReduceRight
                | Self::Some
        )
    }

    #[must_use]
    pub(crate) const fn arity(self) -> i32 {
        match self {
            Self::Set
            | Self::At
            | Self::Includes
            | Self::IndexOf
            | Self::LastIndexOf
            | Self::Fill
            | Self::Join
            | Self::Sort
            | Self::ToSorted
            | Self::Every
            | Self::Filter
            | Self::Find
            | Self::FindIndex
            | Self::FindLast
            | Self::FindLastIndex
            | Self::ForEach
            | Self::Map
            | Self::Reduce
            | Self::ReduceRight
            | Self::Some => 1,
            Self::CopyWithin | Self::Subarray | Self::Slice | Self::With => 2,
            Self::Reverse
            | Self::Entries
            | Self::Keys
            | Self::Values
            | Self::ToReversed
            | Self::Buffer
            | Self::ByteLength
            | Self::ByteOffset
            | Self::Length
            | Self::ToStringTag => 0,
        }
    }
}

impl ArrayBufferPrototypeMethod {
    pub(crate) const ALL: [Self; 11] = [
        Self::ByteLength,
        Self::Detached,
        Self::Immutable,
        Self::MaxByteLength,
        Self::Resizable,
        Self::Resize,
        Self::Slice,
        Self::SliceToImmutable,
        Self::Transfer,
        Self::TransferToFixedLength,
        Self::TransferToImmutable,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ByteLength => "byteLength",
            Self::Detached => "detached",
            Self::Immutable => "immutable",
            Self::MaxByteLength => "maxByteLength",
            Self::Resizable => "resizable",
            Self::Resize => "resize",
            Self::Slice => "slice",
            Self::SliceToImmutable => "sliceToImmutable",
            Self::Transfer => "transfer",
            Self::TransferToFixedLength => "transferToFixedLength",
            Self::TransferToImmutable => "transferToImmutable",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Resize => 1,
            Self::Slice | Self::SliceToImmutable => 2,
            Self::ByteLength
            | Self::Detached
            | Self::Immutable
            | Self::MaxByteLength
            | Self::Resizable
            | Self::Transfer
            | Self::TransferToFixedLength
            | Self::TransferToImmutable => 0,
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::ByteLength
                | Self::Detached
                | Self::Immutable
                | Self::MaxByteLength
                | Self::Resizable
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalInstantStaticMethod {
    From,
    Compare,
    FromEpochMilliseconds,
    FromEpochNanoseconds,
}

impl TemporalInstantStaticMethod {
    pub(crate) const ALL: [Self; 4] = [
        Self::From,
        Self::Compare,
        Self::FromEpochMilliseconds,
        Self::FromEpochNanoseconds,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
            Self::FromEpochMilliseconds => "fromEpochMilliseconds",
            Self::FromEpochNanoseconds => "fromEpochNanoseconds",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Compare => 2,
            Self::From | Self::FromEpochMilliseconds | Self::FromEpochNanoseconds => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalInstantPrototypeMethod {
    EpochMilliseconds,
    EpochNanoseconds,
    Add,
    Subtract,
    Until,
    Since,
    Round,
    Equals,
    ToString,
    ToLocaleString,
    ToJson,
    ToZonedDateTimeISO,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainDateStaticMethod {
    From,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainDateTimeStaticMethod {
    From,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainTimeStaticMethod {
    From,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainMonthDayStaticMethod {
    From,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainYearMonthStaticMethod {
    From,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalZonedDateTimeStaticMethod {
    From,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalZonedDateTimePrototypeMethod {
    CalendarId,
    TimeZoneId,
    Year,
    Month,
    MonthCode,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    Offset,
    OffsetNanoseconds,
    DayOfWeek,
    DayOfYear,
    WeekOfYear,
    YearOfWeek,
    DaysInWeek,
    DaysInMonth,
    DaysInYear,
    MonthsInYear,
    InLeapYear,
    Era,
    EraYear,
    EpochMilliseconds,
    EpochNanoseconds,
    HoursInDay,
    ToInstant,
    ToPlainDate,
    ToPlainTime,
    ToPlainDateTime,
    StartOfDay,
    Equals,
    GetTimeZoneTransition,
    With,
    WithCalendar,
    WithPlainTime,
    WithTimeZone,
    Add,
    Subtract,
    Until,
    Since,
    Round,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainDatePrototypeMethod {
    CalendarId,
    Year,
    Month,
    MonthCode,
    Day,
    DayOfWeek,
    DayOfYear,
    WeekOfYear,
    YearOfWeek,
    DaysInWeek,
    DaysInMonth,
    DaysInYear,
    MonthsInYear,
    InLeapYear,
    Era,
    EraYear,
    With,
    Add,
    Subtract,
    Until,
    Since,
    Equals,
    ToPlainDateTime,
    ToPlainMonthDay,
    ToPlainYearMonth,
    ToZonedDateTime,
    WithCalendar,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainDateTimePrototypeMethod {
    CalendarId,
    Year,
    Month,
    MonthCode,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    DayOfWeek,
    DayOfYear,
    WeekOfYear,
    YearOfWeek,
    DaysInWeek,
    DaysInMonth,
    DaysInYear,
    MonthsInYear,
    InLeapYear,
    Era,
    EraYear,
    With,
    Add,
    Subtract,
    Round,
    Until,
    Since,
    Equals,
    ToZonedDateTime,
    ToPlainDate,
    ToPlainTime,
    WithCalendar,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainTimePrototypeMethod {
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    Add,
    Subtract,
    With,
    Round,
    Until,
    Since,
    Equals,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainMonthDayPrototypeMethod {
    CalendarId,
    MonthCode,
    Day,
    With,
    Equals,
    ToPlainDate,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalPlainYearMonthPrototypeMethod {
    CalendarId,
    Era,
    EraYear,
    Year,
    Month,
    MonthCode,
    DaysInYear,
    DaysInMonth,
    MonthsInYear,
    InLeapYear,
    With,
    Add,
    Subtract,
    Until,
    Since,
    Equals,
    ToPlainDate,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalDurationPrototypeMethod {
    Years,
    Months,
    Weeks,
    Days,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
    Sign,
    Blank,
    Abs,
    Negated,
    With,
    Add,
    Subtract,
    Round,
    Total,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemporalDurationStaticMethod {
    From,
    Compare,
}

impl TemporalDurationStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalDurationPrototypeMethod {
    pub(crate) const ALL: [Self; 23] = [
        Self::Years,
        Self::Months,
        Self::Weeks,
        Self::Days,
        Self::Hours,
        Self::Minutes,
        Self::Seconds,
        Self::Milliseconds,
        Self::Microseconds,
        Self::Nanoseconds,
        Self::Sign,
        Self::Blank,
        Self::Abs,
        Self::Negated,
        Self::With,
        Self::Add,
        Self::Subtract,
        Self::Round,
        Self::Total,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Years => "years",
            Self::Months => "months",
            Self::Weeks => "weeks",
            Self::Days => "days",
            Self::Hours => "hours",
            Self::Minutes => "minutes",
            Self::Seconds => "seconds",
            Self::Milliseconds => "milliseconds",
            Self::Microseconds => "microseconds",
            Self::Nanoseconds => "nanoseconds",
            Self::Sign => "sign",
            Self::Blank => "blank",
            Self::Abs => "abs",
            Self::Negated => "negated",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Round => "round",
            Self::Total => "total",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::Years => "get years",
            Self::Months => "get months",
            Self::Weeks => "get weeks",
            Self::Days => "get days",
            Self::Hours => "get hours",
            Self::Minutes => "get minutes",
            Self::Seconds => "get seconds",
            Self::Milliseconds => "get milliseconds",
            Self::Microseconds => "get microseconds",
            Self::Nanoseconds => "get nanoseconds",
            Self::Sign => "get sign",
            Self::Blank => "get blank",
            Self::Abs => "abs",
            Self::Negated => "negated",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Round => "round",
            Self::Total => "total",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::Years
                | Self::Months
                | Self::Weeks
                | Self::Days
                | Self::Hours
                | Self::Minutes
                | Self::Seconds
                | Self::Milliseconds
                | Self::Microseconds
                | Self::Nanoseconds
                | Self::Sign
                | Self::Blank
        )
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::With | Self::Add | Self::Subtract | Self::Round | Self::Total => 1,
            _ => 0,
        }
    }
}

impl TemporalInstantPrototypeMethod {
    pub(crate) const ALL: [Self; 13] = [
        Self::EpochMilliseconds,
        Self::EpochNanoseconds,
        Self::Add,
        Self::Subtract,
        Self::Until,
        Self::Since,
        Self::Round,
        Self::Equals,
        Self::ToString,
        Self::ToLocaleString,
        Self::ToJson,
        Self::ToZonedDateTimeISO,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::EpochMilliseconds => "epochMilliseconds",
            Self::EpochNanoseconds => "epochNanoseconds",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Until => "until",
            Self::Since => "since",
            Self::Round => "round",
            Self::Equals => "equals",
            Self::ToString => "toString",
            Self::ToLocaleString => "toLocaleString",
            Self::ToJson => "toJSON",
            Self::ToZonedDateTimeISO => "toZonedDateTimeISO",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::EpochMilliseconds => "get epochMilliseconds",
            Self::EpochNanoseconds => "get epochNanoseconds",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Until => "until",
            Self::Since => "since",
            Self::Round => "round",
            Self::Equals => "equals",
            Self::ToString => "toString",
            Self::ToLocaleString => "toLocaleString",
            Self::ToJson => "toJSON",
            Self::ToZonedDateTimeISO => "toZonedDateTimeISO",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Add
            | Self::Subtract
            | Self::Until
            | Self::Since
            | Self::Round
            | Self::Equals
            | Self::ToZonedDateTimeISO => 1,
            Self::EpochMilliseconds
            | Self::EpochNanoseconds
            | Self::ToString
            | Self::ToLocaleString
            | Self::ToJson
            | Self::ValueOf => 0,
        }
    }
}

impl TemporalPlainDateStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalPlainDateTimeStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalPlainTimeStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalPlainMonthDayStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalPlainYearMonthStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalZonedDateTimeStaticMethod {
    pub(crate) const ALL: [Self; 2] = [Self::From, Self::Compare];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Compare => "compare",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From => 1,
            Self::Compare => 2,
        }
    }
}

impl TemporalZonedDateTimePrototypeMethod {
    pub(crate) const ALL: [Self; 48] = [
        Self::CalendarId,
        Self::TimeZoneId,
        Self::Year,
        Self::Month,
        Self::MonthCode,
        Self::Day,
        Self::Hour,
        Self::Minute,
        Self::Second,
        Self::Millisecond,
        Self::Microsecond,
        Self::Nanosecond,
        Self::Offset,
        Self::OffsetNanoseconds,
        Self::DayOfWeek,
        Self::DayOfYear,
        Self::WeekOfYear,
        Self::YearOfWeek,
        Self::DaysInWeek,
        Self::DaysInMonth,
        Self::DaysInYear,
        Self::MonthsInYear,
        Self::InLeapYear,
        Self::Era,
        Self::EraYear,
        Self::EpochMilliseconds,
        Self::EpochNanoseconds,
        Self::HoursInDay,
        Self::ToInstant,
        Self::ToPlainDate,
        Self::ToPlainTime,
        Self::ToPlainDateTime,
        Self::StartOfDay,
        Self::Equals,
        Self::GetTimeZoneTransition,
        Self::With,
        Self::WithCalendar,
        Self::WithPlainTime,
        Self::WithTimeZone,
        Self::Add,
        Self::Subtract,
        Self::Until,
        Self::Since,
        Self::Round,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CalendarId => "calendarId",
            Self::TimeZoneId => "timeZoneId",
            Self::Year => "year",
            Self::Month => "month",
            Self::MonthCode => "monthCode",
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
            Self::Millisecond => "millisecond",
            Self::Microsecond => "microsecond",
            Self::Nanosecond => "nanosecond",
            Self::Offset => "offset",
            Self::OffsetNanoseconds => "offsetNanoseconds",
            Self::DayOfWeek => "dayOfWeek",
            Self::DayOfYear => "dayOfYear",
            Self::WeekOfYear => "weekOfYear",
            Self::YearOfWeek => "yearOfWeek",
            Self::DaysInWeek => "daysInWeek",
            Self::DaysInMonth => "daysInMonth",
            Self::DaysInYear => "daysInYear",
            Self::MonthsInYear => "monthsInYear",
            Self::InLeapYear => "inLeapYear",
            Self::Era => "era",
            Self::EraYear => "eraYear",
            Self::EpochMilliseconds => "epochMilliseconds",
            Self::EpochNanoseconds => "epochNanoseconds",
            Self::HoursInDay => "hoursInDay",
            Self::ToInstant => "toInstant",
            Self::ToPlainDate => "toPlainDate",
            Self::ToPlainTime => "toPlainTime",
            Self::ToPlainDateTime => "toPlainDateTime",
            Self::StartOfDay => "startOfDay",
            Self::Equals => "equals",
            Self::GetTimeZoneTransition => "getTimeZoneTransition",
            Self::With => "with",
            Self::WithCalendar => "withCalendar",
            Self::WithPlainTime => "withPlainTime",
            Self::WithTimeZone => "withTimeZone",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Until => "until",
            Self::Since => "since",
            Self::Round => "round",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::CalendarId => "get calendarId",
            Self::TimeZoneId => "get timeZoneId",
            Self::Year => "get year",
            Self::Month => "get month",
            Self::MonthCode => "get monthCode",
            Self::Day => "get day",
            Self::Hour => "get hour",
            Self::Minute => "get minute",
            Self::Second => "get second",
            Self::Millisecond => "get millisecond",
            Self::Microsecond => "get microsecond",
            Self::Nanosecond => "get nanosecond",
            Self::Offset => "get offset",
            Self::OffsetNanoseconds => "get offsetNanoseconds",
            Self::DayOfWeek => "get dayOfWeek",
            Self::DayOfYear => "get dayOfYear",
            Self::WeekOfYear => "get weekOfYear",
            Self::YearOfWeek => "get yearOfWeek",
            Self::DaysInWeek => "get daysInWeek",
            Self::DaysInMonth => "get daysInMonth",
            Self::DaysInYear => "get daysInYear",
            Self::MonthsInYear => "get monthsInYear",
            Self::InLeapYear => "get inLeapYear",
            Self::Era => "get era",
            Self::EraYear => "get eraYear",
            Self::EpochMilliseconds => "get epochMilliseconds",
            Self::EpochNanoseconds => "get epochNanoseconds",
            Self::HoursInDay => "get hoursInDay",
            method => method.name(),
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        !matches!(
            self,
            Self::ToInstant
                | Self::ToPlainDate
                | Self::ToPlainTime
                | Self::ToPlainDateTime
                | Self::StartOfDay
                | Self::Equals
                | Self::GetTimeZoneTransition
                | Self::With
                | Self::WithCalendar
                | Self::WithPlainTime
                | Self::WithTimeZone
                | Self::Add
                | Self::Subtract
                | Self::Until
                | Self::Since
                | Self::Round
                | Self::ToString
                | Self::ToJson
                | Self::ToLocaleString
                | Self::ValueOf
        )
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Equals
            | Self::GetTimeZoneTransition
            | Self::Add
            | Self::Subtract
            | Self::Until
            | Self::Since
            | Self::Round
            | Self::With
            | Self::WithCalendar
            | Self::WithTimeZone => 1,
            _ => 0,
        }
    }
}

impl TemporalPlainTimePrototypeMethod {
    pub(crate) const ALL: [Self; 17] = [
        Self::Hour,
        Self::Minute,
        Self::Second,
        Self::Millisecond,
        Self::Microsecond,
        Self::Nanosecond,
        Self::Add,
        Self::Subtract,
        Self::With,
        Self::Round,
        Self::Until,
        Self::Since,
        Self::Equals,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
            Self::Millisecond => "millisecond",
            Self::Microsecond => "microsecond",
            Self::Nanosecond => "nanosecond",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::With => "with",
            Self::Round => "round",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::Hour => "get hour",
            Self::Minute => "get minute",
            Self::Second => "get second",
            Self::Millisecond => "get millisecond",
            Self::Microsecond => "get microsecond",
            Self::Nanosecond => "get nanosecond",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::With => "with",
            Self::Round => "round",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::Hour
                | Self::Minute
                | Self::Second
                | Self::Millisecond
                | Self::Microsecond
                | Self::Nanosecond
        )
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Add
            | Self::Subtract
            | Self::With
            | Self::Round
            | Self::Until
            | Self::Since
            | Self::Equals => 1,
            _ => 0,
        }
    }
}

impl TemporalPlainMonthDayPrototypeMethod {
    pub(crate) const ALL: [Self; 10] = [
        Self::CalendarId,
        Self::MonthCode,
        Self::Day,
        Self::With,
        Self::Equals,
        Self::ToPlainDate,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CalendarId => "calendarId",
            Self::MonthCode => "monthCode",
            Self::Day => "day",
            Self::With => "with",
            Self::Equals => "equals",
            Self::ToPlainDate => "toPlainDate",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::CalendarId => "get calendarId",
            Self::MonthCode => "get monthCode",
            Self::Day => "get day",
            method => method.name(),
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(self, Self::CalendarId | Self::MonthCode | Self::Day)
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::With | Self::Equals | Self::ToPlainDate => 1,
            _ => 0,
        }
    }
}

impl TemporalPlainYearMonthPrototypeMethod {
    pub(crate) const ALL: [Self; 21] = [
        Self::CalendarId,
        Self::Era,
        Self::EraYear,
        Self::Year,
        Self::Month,
        Self::MonthCode,
        Self::DaysInYear,
        Self::DaysInMonth,
        Self::MonthsInYear,
        Self::InLeapYear,
        Self::With,
        Self::Add,
        Self::Subtract,
        Self::Until,
        Self::Since,
        Self::Equals,
        Self::ToPlainDate,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CalendarId => "calendarId",
            Self::Era => "era",
            Self::EraYear => "eraYear",
            Self::Year => "year",
            Self::Month => "month",
            Self::MonthCode => "monthCode",
            Self::DaysInYear => "daysInYear",
            Self::DaysInMonth => "daysInMonth",
            Self::MonthsInYear => "monthsInYear",
            Self::InLeapYear => "inLeapYear",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToPlainDate => "toPlainDate",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::CalendarId => "get calendarId",
            Self::Era => "get era",
            Self::EraYear => "get eraYear",
            Self::Year => "get year",
            Self::Month => "get month",
            Self::MonthCode => "get monthCode",
            Self::DaysInYear => "get daysInYear",
            Self::DaysInMonth => "get daysInMonth",
            Self::MonthsInYear => "get monthsInYear",
            Self::InLeapYear => "get inLeapYear",
            method => method.name(),
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::CalendarId
                | Self::Era
                | Self::EraYear
                | Self::Year
                | Self::Month
                | Self::MonthCode
                | Self::DaysInYear
                | Self::DaysInMonth
                | Self::MonthsInYear
                | Self::InLeapYear
        )
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::With
            | Self::Add
            | Self::Subtract
            | Self::Until
            | Self::Since
            | Self::Equals
            | Self::ToPlainDate => 1,
            _ => 0,
        }
    }
}

impl TemporalPlainDatePrototypeMethod {
    pub(crate) const ALL: [Self; 31] = [
        Self::CalendarId,
        Self::Year,
        Self::Month,
        Self::MonthCode,
        Self::Day,
        Self::DayOfWeek,
        Self::DayOfYear,
        Self::WeekOfYear,
        Self::YearOfWeek,
        Self::DaysInWeek,
        Self::DaysInMonth,
        Self::DaysInYear,
        Self::MonthsInYear,
        Self::InLeapYear,
        Self::Era,
        Self::EraYear,
        Self::With,
        Self::Add,
        Self::Subtract,
        Self::Until,
        Self::Since,
        Self::Equals,
        Self::ToPlainDateTime,
        Self::ToPlainMonthDay,
        Self::ToPlainYearMonth,
        Self::ToZonedDateTime,
        Self::WithCalendar,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CalendarId => "calendarId",
            Self::Year => "year",
            Self::Month => "month",
            Self::MonthCode => "monthCode",
            Self::Day => "day",
            Self::DayOfWeek => "dayOfWeek",
            Self::DayOfYear => "dayOfYear",
            Self::WeekOfYear => "weekOfYear",
            Self::YearOfWeek => "yearOfWeek",
            Self::DaysInWeek => "daysInWeek",
            Self::DaysInMonth => "daysInMonth",
            Self::DaysInYear => "daysInYear",
            Self::MonthsInYear => "monthsInYear",
            Self::InLeapYear => "inLeapYear",
            Self::Era => "era",
            Self::EraYear => "eraYear",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToPlainDateTime => "toPlainDateTime",
            Self::ToPlainMonthDay => "toPlainMonthDay",
            Self::ToPlainYearMonth => "toPlainYearMonth",
            Self::ToZonedDateTime => "toZonedDateTime",
            Self::WithCalendar => "withCalendar",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::CalendarId => "get calendarId",
            Self::Year => "get year",
            Self::Month => "get month",
            Self::MonthCode => "get monthCode",
            Self::Day => "get day",
            Self::DayOfWeek => "get dayOfWeek",
            Self::DayOfYear => "get dayOfYear",
            Self::WeekOfYear => "get weekOfYear",
            Self::YearOfWeek => "get yearOfWeek",
            Self::DaysInWeek => "get daysInWeek",
            Self::DaysInMonth => "get daysInMonth",
            Self::DaysInYear => "get daysInYear",
            Self::MonthsInYear => "get monthsInYear",
            Self::InLeapYear => "get inLeapYear",
            Self::Era => "get era",
            Self::EraYear => "get eraYear",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToPlainDateTime => "toPlainDateTime",
            Self::ToPlainMonthDay => "toPlainMonthDay",
            Self::ToPlainYearMonth => "toPlainYearMonth",
            Self::ToZonedDateTime => "toZonedDateTime",
            Self::WithCalendar => "withCalendar",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::CalendarId
                | Self::Year
                | Self::Month
                | Self::MonthCode
                | Self::Day
                | Self::DayOfWeek
                | Self::DayOfYear
                | Self::WeekOfYear
                | Self::YearOfWeek
                | Self::DaysInWeek
                | Self::DaysInMonth
                | Self::DaysInYear
                | Self::MonthsInYear
                | Self::InLeapYear
                | Self::Era
                | Self::EraYear
        )
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::With
            | Self::Add
            | Self::Subtract
            | Self::Until
            | Self::Since
            | Self::Equals
            | Self::ToZonedDateTime
            | Self::WithCalendar => 1,
            _ => 0,
        }
    }
}

impl TemporalPlainDateTimePrototypeMethod {
    pub(crate) const ALL: [Self; 37] = [
        Self::CalendarId,
        Self::Year,
        Self::Month,
        Self::MonthCode,
        Self::Day,
        Self::Hour,
        Self::Minute,
        Self::Second,
        Self::Millisecond,
        Self::Microsecond,
        Self::Nanosecond,
        Self::DayOfWeek,
        Self::DayOfYear,
        Self::WeekOfYear,
        Self::YearOfWeek,
        Self::DaysInWeek,
        Self::DaysInMonth,
        Self::DaysInYear,
        Self::MonthsInYear,
        Self::InLeapYear,
        Self::Era,
        Self::EraYear,
        Self::With,
        Self::Add,
        Self::Subtract,
        Self::Round,
        Self::Until,
        Self::Since,
        Self::Equals,
        Self::ToZonedDateTime,
        Self::ToPlainDate,
        Self::ToPlainTime,
        Self::WithCalendar,
        Self::ToString,
        Self::ToJson,
        Self::ToLocaleString,
        Self::ValueOf,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CalendarId => "calendarId",
            Self::Year => "year",
            Self::Month => "month",
            Self::MonthCode => "monthCode",
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
            Self::Millisecond => "millisecond",
            Self::Microsecond => "microsecond",
            Self::Nanosecond => "nanosecond",
            Self::DayOfWeek => "dayOfWeek",
            Self::DayOfYear => "dayOfYear",
            Self::WeekOfYear => "weekOfYear",
            Self::YearOfWeek => "yearOfWeek",
            Self::DaysInWeek => "daysInWeek",
            Self::DaysInMonth => "daysInMonth",
            Self::DaysInYear => "daysInYear",
            Self::MonthsInYear => "monthsInYear",
            Self::InLeapYear => "inLeapYear",
            Self::Era => "era",
            Self::EraYear => "eraYear",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Round => "round",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToZonedDateTime => "toZonedDateTime",
            Self::ToPlainDate => "toPlainDate",
            Self::ToPlainTime => "toPlainTime",
            Self::WithCalendar => "withCalendar",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn function_name(self) -> &'static str {
        match self {
            Self::CalendarId => "get calendarId",
            Self::Year => "get year",
            Self::Month => "get month",
            Self::MonthCode => "get monthCode",
            Self::Day => "get day",
            Self::Hour => "get hour",
            Self::Minute => "get minute",
            Self::Second => "get second",
            Self::Millisecond => "get millisecond",
            Self::Microsecond => "get microsecond",
            Self::Nanosecond => "get nanosecond",
            Self::DayOfWeek => "get dayOfWeek",
            Self::DayOfYear => "get dayOfYear",
            Self::WeekOfYear => "get weekOfYear",
            Self::YearOfWeek => "get yearOfWeek",
            Self::DaysInWeek => "get daysInWeek",
            Self::DaysInMonth => "get daysInMonth",
            Self::DaysInYear => "get daysInYear",
            Self::MonthsInYear => "get monthsInYear",
            Self::InLeapYear => "get inLeapYear",
            Self::Era => "get era",
            Self::EraYear => "get eraYear",
            Self::With => "with",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Round => "round",
            Self::Until => "until",
            Self::Since => "since",
            Self::Equals => "equals",
            Self::ToZonedDateTime => "toZonedDateTime",
            Self::ToPlainDate => "toPlainDate",
            Self::ToPlainTime => "toPlainTime",
            Self::WithCalendar => "withCalendar",
            Self::ToString => "toString",
            Self::ToJson => "toJSON",
            Self::ToLocaleString => "toLocaleString",
            Self::ValueOf => "valueOf",
        }
    }

    pub(crate) const fn is_accessor(self) -> bool {
        matches!(
            self,
            Self::CalendarId
                | Self::Year
                | Self::Month
                | Self::MonthCode
                | Self::Day
                | Self::Hour
                | Self::Minute
                | Self::Second
                | Self::Millisecond
                | Self::Microsecond
                | Self::Nanosecond
                | Self::DayOfWeek
                | Self::DayOfYear
                | Self::WeekOfYear
                | Self::YearOfWeek
                | Self::DaysInWeek
                | Self::DaysInMonth
                | Self::DaysInYear
                | Self::MonthsInYear
                | Self::InLeapYear
                | Self::Era
                | Self::EraYear
        )
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::With
            | Self::Add
            | Self::Subtract
            | Self::Round
            | Self::Until
            | Self::Since
            | Self::Equals
            | Self::ToZonedDateTime
            | Self::WithCalendar => 1,
            _ => 0,
        }
    }
}

/// Boolean accessors backed by one `RegExp` instance's `[[OriginalFlags]]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegExpFlag {
    Global,
    IgnoreCase,
    Multiline,
    DotAll,
    Unicode,
    UnicodeSets,
    Sticky,
    HasIndices,
}

impl RegExpFlag {
    pub(crate) const ALL: [Self; 8] = [
        Self::Global,
        Self::IgnoreCase,
        Self::Multiline,
        Self::DotAll,
        Self::Unicode,
        Self::UnicodeSets,
        Self::Sticky,
        Self::HasIndices,
    ];

    pub(crate) fn code_unit(self) -> u16 {
        match self {
            Self::Global => u16::from(b'g'),
            Self::IgnoreCase => u16::from(b'i'),
            Self::Multiline => u16::from(b'm'),
            Self::DotAll => u16::from(b's'),
            Self::Unicode => u16::from(b'u'),
            Self::UnicodeSets => u16::from(b'v'),
            Self::Sticky => u16::from(b'y'),
            Self::HasIndices => u16::from(b'd'),
        }
    }

    pub(crate) const fn atom(self) -> PredefinedAtom {
        match self {
            Self::Global => PredefinedAtom::Global,
            Self::IgnoreCase => PredefinedAtom::IgnoreCase,
            Self::Multiline => PredefinedAtom::Multiline,
            Self::DotAll => PredefinedAtom::DotAll,
            Self::Unicode => PredefinedAtom::Unicode,
            Self::UnicodeSets => PredefinedAtom::UnicodeSets,
            Self::Sticky => PredefinedAtom::Sticky,
            Self::HasIndices => PredefinedAtom::HasIndices,
        }
    }
}

/// Well-known-symbol methods on `%RegExp.prototype%`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegExpSymbolMethod {
    Replace,
    Match,
    MatchAll,
    Search,
    Split,
}

impl RegExpSymbolMethod {
    pub(crate) const ALL: [Self; 5] = [
        Self::Replace,
        Self::Match,
        Self::MatchAll,
        Self::Search,
        Self::Split,
    ];

    pub(crate) const fn atom(self) -> PredefinedAtom {
        match self {
            Self::Replace => PredefinedAtom::SymbolReplace,
            Self::Match => PredefinedAtom::SymbolMatch,
            Self::MatchAll => PredefinedAtom::SymbolMatchAll,
            Self::Search => PredefinedAtom::SymbolSearch,
            Self::Split => PredefinedAtom::SymbolSplit,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Replace => "[Symbol.replace]",
            Self::Match => "[Symbol.match]",
            Self::MatchAll => "[Symbol.matchAll]",
            Self::Search => "[Symbol.search]",
            Self::Split => "[Symbol.split]",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Replace | Self::Split => 2,
            Self::Match | Self::MatchAll | Self::Search => 1,
        }
    }
}

/// Methods installed on `%Map.prototype%` in pinned `QuickJS` property order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MapMethod {
    Set,
    Get,
    GetOrInsert,
    GetOrInsertComputed,
    Has,
    Delete,
    Clear,
    Size,
    ForEach,
    Values,
    Keys,
    Entries,
}

impl MapMethod {
    pub(crate) const ALL: [Self; 12] = [
        Self::Set,
        Self::Get,
        Self::GetOrInsert,
        Self::GetOrInsertComputed,
        Self::Has,
        Self::Delete,
        Self::Clear,
        Self::Size,
        Self::ForEach,
        Self::Values,
        Self::Keys,
        Self::Entries,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Get => "get",
            Self::GetOrInsert => "getOrInsert",
            Self::GetOrInsertComputed => "getOrInsertComputed",
            Self::Has => "has",
            Self::Delete => "delete",
            Self::Clear => "clear",
            Self::Size => "size",
            Self::ForEach => "forEach",
            Self::Values => "values",
            Self::Keys => "keys",
            Self::Entries => "entries",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Set | Self::GetOrInsert | Self::GetOrInsertComputed => 2,
            Self::Get | Self::Has | Self::Delete | Self::ForEach => 1,
            Self::Clear | Self::Size | Self::Values | Self::Keys | Self::Entries => 0,
        }
    }
}

/// Function identities installed on `%Set.prototype%` in pinned `QuickJS` order.
/// `keys` and `@@iterator` alias the single `values` identity and therefore do
/// not receive their own enum variants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetMethod {
    Add,
    Has,
    Delete,
    Clear,
    Size,
    ForEach,
    IsDisjointFrom,
    IsSubsetOf,
    IsSupersetOf,
    Intersection,
    Difference,
    SymmetricDifference,
    Union,
    Values,
    Entries,
}

impl SetMethod {
    pub(crate) const ALL: [Self; 15] = [
        Self::Add,
        Self::Has,
        Self::Delete,
        Self::Clear,
        Self::Size,
        Self::ForEach,
        Self::IsDisjointFrom,
        Self::IsSubsetOf,
        Self::IsSupersetOf,
        Self::Intersection,
        Self::Difference,
        Self::SymmetricDifference,
        Self::Union,
        Self::Values,
        Self::Entries,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Has => "has",
            Self::Delete => "delete",
            Self::Clear => "clear",
            Self::Size => "size",
            Self::ForEach => "forEach",
            Self::IsDisjointFrom => "isDisjointFrom",
            Self::IsSubsetOf => "isSubsetOf",
            Self::IsSupersetOf => "isSupersetOf",
            Self::Intersection => "intersection",
            Self::Difference => "difference",
            Self::SymmetricDifference => "symmetricDifference",
            Self::Union => "union",
            Self::Values => "values",
            Self::Entries => "entries",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Clear | Self::Size | Self::Values | Self::Entries => 0,
            Self::Add
            | Self::Has
            | Self::Delete
            | Self::ForEach
            | Self::IsDisjointFrom
            | Self::IsSubsetOf
            | Self::IsSupersetOf
            | Self::Intersection
            | Self::Difference
            | Self::SymmetricDifference
            | Self::Union => 1,
        }
    }
}

/// Methods installed on `%WeakMap.prototype%` in pinned `QuickJS` order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WeakMapMethod {
    Set,
    Get,
    GetOrInsert,
    GetOrInsertComputed,
    Has,
    Delete,
}

impl WeakMapMethod {
    pub(crate) const ALL: [Self; 6] = [
        Self::Set,
        Self::Get,
        Self::GetOrInsert,
        Self::GetOrInsertComputed,
        Self::Has,
        Self::Delete,
    ];

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Set | Self::GetOrInsert | Self::GetOrInsertComputed => 2,
            Self::Get | Self::Has | Self::Delete => 1,
        }
    }
}

/// Methods installed on `%WeakSet.prototype%` in pinned `QuickJS` order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WeakSetMethod {
    Add,
    Has,
    Delete,
}

/// Methods installed on `%FinalizationRegistry.prototype%` in pinned
/// `QuickJS` order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FinalizationRegistryMethod {
    Register,
    Unregister,
}

impl FinalizationRegistryMethod {
    pub(crate) const ALL: [Self; 2] = [Self::Register, Self::Unregister];

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Register => 2,
            Self::Unregister => 1,
        }
    }
}

impl WeakSetMethod {
    pub(crate) const ALL: [Self; 3] = [Self::Add, Self::Has, Self::Delete];

    pub(crate) const fn length() -> i32 {
        1
    }
}

/// The remaining methods installed on the `Promise` constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseStatic {
    All,
    AllSettled,
    Any,
    Try,
    Race,
    WithResolvers,
}

impl PromiseStatic {
    /// Pinned `QuickJS` 2026-06-04 own-property publication order.
    pub(crate) const ALL: [Self; 6] = [
        Self::All,
        Self::AllSettled,
        Self::Any,
        Self::Try,
        Self::Race,
        Self::WithResolvers,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::AllSettled => "allSettled",
            Self::Any => "any",
            Self::Try => "try",
            Self::Race => "race",
            Self::WithResolvers => "withResolvers",
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::WithResolvers => 0,
            Self::All | Self::AllSettled | Self::Any | Self::Try | Self::Race => 1,
        }
    }
}

/// The generic factories installed on the `Array` constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayStatic {
    From,
    FromAsync,
    Of,
}

impl ArrayStatic {
    pub(crate) const ALL: [Self; 3] = [Self::From, Self::FromAsync, Self::Of];

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::From | Self::FromAsync => 1,
            Self::Of => 0,
        }
    }

    pub(crate) const fn predefined_atom(self) -> Option<PredefinedAtom> {
        match self {
            Self::From => Some(PredefinedAtom::From),
            Self::FromAsync => None,
            Self::Of => Some(PredefinedAtom::Of),
        }
    }
}

/// The ECMAScript 2025 `%Reflect%` method set, in specification property order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReflectMethod {
    Apply,
    Construct,
    DefineProperty,
    DeleteProperty,
    Get,
    GetOwnPropertyDescriptor,
    GetPrototypeOf,
    Has,
    IsExtensible,
    OwnKeys,
    PreventExtensions,
    Set,
    SetPrototypeOf,
}

impl ReflectMethod {
    pub(crate) const ALL: [Self; 13] = [
        Self::Apply,
        Self::Construct,
        Self::DefineProperty,
        Self::DeleteProperty,
        Self::Get,
        Self::GetOwnPropertyDescriptor,
        Self::GetPrototypeOf,
        Self::Has,
        Self::IsExtensible,
        Self::OwnKeys,
        Self::PreventExtensions,
        Self::Set,
        Self::SetPrototypeOf,
    ];

    pub(crate) const fn predefined_atom(self) -> PredefinedAtom {
        match self {
            Self::Apply => PredefinedAtom::Apply,
            Self::Construct => PredefinedAtom::Construct,
            Self::DefineProperty => PredefinedAtom::DefineProperty,
            Self::DeleteProperty => PredefinedAtom::DeleteProperty,
            Self::Get => PredefinedAtom::Get,
            Self::GetOwnPropertyDescriptor => PredefinedAtom::GetOwnPropertyDescriptor,
            Self::GetPrototypeOf => PredefinedAtom::GetPrototypeOf,
            Self::Has => PredefinedAtom::Has,
            Self::IsExtensible => PredefinedAtom::IsExtensible,
            Self::OwnKeys => PredefinedAtom::OwnKeys,
            Self::PreventExtensions => PredefinedAtom::PreventExtensions,
            Self::Set => PredefinedAtom::SetProperty,
            Self::SetPrototypeOf => PredefinedAtom::SetPrototypeOf,
        }
    }

    pub(crate) const fn length(self) -> i32 {
        match self {
            Self::Apply | Self::DefineProperty | Self::Set => 3,
            Self::Construct
            | Self::DeleteProperty
            | Self::Get
            | Self::GetOwnPropertyDescriptor
            | Self::Has
            | Self::SetPrototypeOf => 2,
            Self::GetPrototypeOf | Self::IsExtensible | Self::OwnKeys | Self::PreventExtensions => {
                1
            }
        }
    }
}

impl NativeFunctionKind {
    pub(crate) const fn is_constructor(self) -> bool {
        matches!(
            self,
            Self::OrdinaryFunctionConstructor
                | Self::ObjectConstructor
                | Self::ErrorConstructor(_)
                | Self::BooleanConstructor
                | Self::NumberConstructor
                | Self::StringConstructor
                | Self::ArrayConstructor
                | Self::ArrayBufferConstructor
                | Self::SharedArrayBufferConstructor
                | Self::DataViewConstructor
                | Self::TypedArrayConstructor(_)
                | Self::DateConstructor
                | Self::TemporalDurationConstructor
                | Self::TemporalInstantConstructor
                | Self::TemporalPlainDateConstructor
                | Self::TemporalPlainDateTimeConstructor
                | Self::TemporalPlainTimeConstructor
                | Self::TemporalPlainMonthDayConstructor
                | Self::TemporalPlainYearMonthConstructor
                | Self::TemporalZonedDateTimeConstructor
                | Self::RegExpConstructor
                | Self::IteratorConstructor
                | Self::GeneratorFunctionConstructor
                | Self::AsyncFunctionConstructor
                | Self::AsyncGeneratorFunctionConstructor
                | Self::PromiseConstructor
                | Self::MapConstructor
                | Self::SetConstructor
                | Self::WeakMapConstructor
                | Self::WeakSetConstructor
                | Self::WeakRefConstructor
                | Self::FinalizationRegistryConstructor
                | Self::ProxyConstructor
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeFunction {
    pub(crate) realm: RealmId,
    pub(crate) kind: NativeFunctionKind,
}

pub(crate) enum FunctionImplementation {
    Bytecode(BytecodeFunction),
    Native(NativeFunction),
    Bound(BoundFunction),
    PromiseResolving(PromiseResolvingFunction),
    PromiseCapabilityExecutor(PromiseCapabilityExecutor),
    PromiseFinally(PromiseFinallyFunction),
    PromiseCombinatorElement(PromiseCombinatorElementFunction),
    /// A callable Proxy exotic object. Non-callable proxies live in the object
    /// arena with the same state representation.
    Proxy(ProxyState),
    /// The idempotent closure created by `Proxy.revocable`.
    ProxyRevoker(ProxyRevokerFunction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProxyRevokerFunction {
    pub(crate) proxy: HeapReference,
    pub(crate) realm: RealmId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseResolvingKind {
    Resolve,
    Reject,
}

pub(crate) struct PromiseResolvingFunction {
    pub(crate) promise: ObjectId,
    pub(crate) realm: RealmId,
    pub(crate) kind: PromiseResolvingKind,
    pub(crate) already_resolved: Rc<Cell<bool>>,
}

impl Clone for PromiseResolvingFunction {
    fn clone(&self) -> Self {
        Self {
            promise: self.promise,
            realm: self.realm,
            kind: self.kind,
            already_resolved: Rc::clone(&self.already_resolved),
        }
    }
}

#[derive(Default)]
pub(crate) struct PromiseCapabilityCapture {
    pub(crate) resolve: Option<StoredValue>,
    pub(crate) reject: Option<StoredValue>,
}

pub(crate) struct PromiseCapabilityExecutor {
    pub(crate) realm: RealmId,
    pub(crate) capture: Rc<RefCell<PromiseCapabilityCapture>>,
}

impl Clone for PromiseCapabilityExecutor {
    fn clone(&self) -> Self {
        Self {
            realm: self.realm,
            capture: Rc::clone(&self.capture),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseFinallyHandlerKind {
    Then,
    Catch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseFinallyThunkKind {
    Return,
    Throw,
}

pub(crate) enum PromiseFinallyFunction {
    Handler {
        realm: RealmId,
        on_finally: FunctionId,
        constructor: FunctionId,
        kind: PromiseFinallyHandlerKind,
    },
    Thunk {
        realm: RealmId,
        completion: StoredValue,
        kind: PromiseFinallyThunkKind,
    },
}

impl PromiseFinallyFunction {
    pub(crate) const fn realm(&self) -> RealmId {
        match self {
            Self::Handler { realm, .. } | Self::Thunk { realm, .. } => *realm,
        }
    }
}

impl Clone for PromiseFinallyFunction {
    fn clone(&self) -> Self {
        match self {
            Self::Handler {
                realm,
                on_finally,
                constructor,
                kind,
            } => Self::Handler {
                realm: *realm,
                on_finally: *on_finally,
                constructor: *constructor,
                kind: *kind,
            },
            Self::Thunk {
                realm,
                completion,
                kind,
            } => Self::Thunk {
                realm: *realm,
                completion: completion.duplicate(),
                kind: *kind,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseCombinatorKind {
    All,
    AllSettled,
    Any,
    Race,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseCombinatorElementKind {
    AllResolve,
    AllSettledResolve,
    AllSettledReject,
    AnyReject,
}

pub(crate) struct PromiseCombinatorShared {
    pub(crate) kind: PromiseCombinatorKind,
    pub(crate) capability: PromiseCapability,
    pub(crate) values: Vec<Option<StoredValue>>,
    pub(crate) remaining: u64,
}

pub(crate) struct PromiseCombinatorElementFunction {
    pub(crate) realm: RealmId,
    pub(crate) kind: PromiseCombinatorElementKind,
    pub(crate) index: usize,
    pub(crate) shared: Rc<RefCell<PromiseCombinatorShared>>,
    pub(crate) already_called: Rc<Cell<bool>>,
}

impl Clone for PromiseCombinatorElementFunction {
    fn clone(&self) -> Self {
        Self {
            realm: self.realm,
            kind: self.kind,
            index: self.index,
            shared: Rc::clone(&self.shared),
            already_called: Rc::clone(&self.already_called),
        }
    }
}

pub(crate) enum PromiseJob {
    Reaction {
        reaction: PromiseReaction,
        argument: StoredValue,
    },
    Thenable {
        promise: ObjectId,
        realm: RealmId,
        thenable: StoredValue,
        then: FunctionId,
    },
}

/// One `Function.prototype.bind` result: a callable/constructable function
/// whose own receiver and leading arguments override the target's call.
pub(crate) struct BoundFunction {
    pub(crate) target: FunctionId,
    pub(crate) bound_this: StoredValue,
    pub(crate) bound_arguments: Vec<StoredValue>,
}

pub(crate) struct HeapFunction {
    pub(crate) implementation: FunctionImplementation,
    pub(crate) object: ObjectRecord,
    pub(crate) public_roots: u32,
}

impl HeapFunction {
    pub(crate) fn bytecode(&self) -> Result<&BytecodeFunction, crate::EngineFault> {
        match &self.implementation {
            FunctionImplementation::Bytecode(function) => Ok(function),
            FunctionImplementation::Native(_) => Err(crate::EngineFault::RuntimeInvariant {
                message: "native function reached the bytecode execution path",
            }),
            FunctionImplementation::Bound(_) => Err(crate::EngineFault::RuntimeInvariant {
                message: "bound function reached the bytecode execution path",
            }),
            FunctionImplementation::PromiseResolving(_) => {
                Err(crate::EngineFault::RuntimeInvariant {
                    message: "Promise resolving function reached the bytecode execution path",
                })
            }
            FunctionImplementation::PromiseCapabilityExecutor(_) => {
                Err(crate::EngineFault::RuntimeInvariant {
                    message: "Promise capability executor reached the bytecode execution path",
                })
            }
            FunctionImplementation::PromiseFinally(_) => {
                Err(crate::EngineFault::RuntimeInvariant {
                    message: "Promise finally function reached the bytecode execution path",
                })
            }
            FunctionImplementation::PromiseCombinatorElement(_) => {
                Err(crate::EngineFault::RuntimeInvariant {
                    message: "Promise combinator element function reached the bytecode execution path",
                })
            }
            FunctionImplementation::Proxy(_) => Err(crate::EngineFault::RuntimeInvariant {
                message: "Proxy function reached the bytecode execution path",
            }),
            FunctionImplementation::ProxyRevoker(_) => Err(crate::EngineFault::RuntimeInvariant {
                message: "Proxy revoker reached the bytecode execution path",
            }),
        }
    }

    pub(crate) const fn native(&self) -> Option<&NativeFunction> {
        match &self.implementation {
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Bound(_)
            | FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::PromiseCombinatorElement(_)
            | FunctionImplementation::Proxy(_)
            | FunctionImplementation::ProxyRevoker(_) => None,
            FunctionImplementation::Native(function) => Some(function),
        }
    }

    pub(crate) fn bound(&self) -> Option<&BoundFunction> {
        match &self.implementation {
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Native(_)
            | FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::PromiseCombinatorElement(_)
            | FunctionImplementation::Proxy(_)
            | FunctionImplementation::ProxyRevoker(_) => None,
            FunctionImplementation::Bound(bound) => Some(bound),
        }
    }

    pub(crate) fn promise_resolving(&self) -> Option<&PromiseResolvingFunction> {
        match &self.implementation {
            FunctionImplementation::PromiseResolving(resolving) => Some(resolving),
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Native(_)
            | FunctionImplementation::Bound(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::PromiseCombinatorElement(_)
            | FunctionImplementation::Proxy(_)
            | FunctionImplementation::ProxyRevoker(_) => None,
        }
    }

    pub(crate) fn promise_capability_executor(&self) -> Option<&PromiseCapabilityExecutor> {
        match &self.implementation {
            FunctionImplementation::PromiseCapabilityExecutor(executor) => Some(executor),
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Native(_)
            | FunctionImplementation::Bound(_)
            | FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::PromiseCombinatorElement(_)
            | FunctionImplementation::Proxy(_)
            | FunctionImplementation::ProxyRevoker(_) => None,
        }
    }

    pub(crate) fn promise_finally(&self) -> Option<&PromiseFinallyFunction> {
        match &self.implementation {
            FunctionImplementation::PromiseFinally(function) => Some(function),
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Native(_)
            | FunctionImplementation::Bound(_)
            | FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseCombinatorElement(_)
            | FunctionImplementation::Proxy(_)
            | FunctionImplementation::ProxyRevoker(_) => None,
        }
    }

    pub(crate) fn promise_combinator_element(&self) -> Option<&PromiseCombinatorElementFunction> {
        match &self.implementation {
            FunctionImplementation::PromiseCombinatorElement(function) => Some(function),
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Native(_)
            | FunctionImplementation::Bound(_)
            | FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::Proxy(_)
            | FunctionImplementation::ProxyRevoker(_) => None,
        }
    }

    pub(crate) const fn proxy(&self) -> Option<&ProxyState> {
        match &self.implementation {
            FunctionImplementation::Proxy(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn proxy_mut(&mut self) -> Option<&mut ProxyState> {
        match &mut self.implementation {
            FunctionImplementation::Proxy(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn proxy_revoker(&self) -> Option<ProxyRevokerFunction> {
        match self.implementation {
            FunctionImplementation::ProxyRevoker(revoker) => Some(revoker),
            _ => None,
        }
    }
}

pub(crate) struct BindingCell {
    pub(crate) value: SlotValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnvironmentBinding {
    Captured(BindingCellId),
    RealmGlobal(RealmGlobalBindingId),
}

pub(crate) struct RealmGlobalBinding {
    pub(crate) realm: RealmId,
    pub(crate) name: Atom,
    pub(crate) state: RealmGlobalBindingState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RealmGlobalBindingState {
    Unresolved,
    Object,
    Lexical { cell: BindingCellId, mutable: bool },
}

#[derive(Clone, Copy)]
enum RealmGlobalRequest {
    Lookup,
    Var,
    Function,
    Let,
    Const,
}

impl RealmGlobalRequest {
    fn from_policy(policy: CompilerBindingPolicy) -> Result<Self, InstallError> {
        match (
            policy.kind(),
            policy.initialization(),
            policy.writes(),
            policy.has_temporal_dead_zone(),
        ) {
            (
                CompilerBindingKind::GlobalReference,
                quickjs_bytecode::CompilerInitializationPolicy::ConstructorRealmLookup,
                quickjs_bytecode::CompilerWritePolicy::Mutable,
                false,
            ) => Ok(Self::Lookup),
            (
                CompilerBindingKind::Var,
                quickjs_bytecode::CompilerInitializationPolicy::UndefinedAtInstantiation,
                quickjs_bytecode::CompilerWritePolicy::Mutable,
                false,
            ) => Ok(Self::Var),
            (
                CompilerBindingKind::Function,
                quickjs_bytecode::CompilerInitializationPolicy::FunctionAtInstantiation,
                quickjs_bytecode::CompilerWritePolicy::Mutable,
                false,
            ) => Ok(Self::Function),
            (
                CompilerBindingKind::Let,
                quickjs_bytecode::CompilerInitializationPolicy::AtDeclaration,
                quickjs_bytecode::CompilerWritePolicy::Mutable,
                true,
            ) => Ok(Self::Let),
            (
                CompilerBindingKind::Const,
                quickjs_bytecode::CompilerInitializationPolicy::AtDeclaration,
                quickjs_bytecode::CompilerWritePolicy::Immutable,
                true,
            ) => Ok(Self::Const),
            _ => Err(InstallError::AuthorityInvariant {
                message: "unsupported constructor-realm global declaration policy",
            }),
        }
    }

    const fn initial_nonlexical_state(self) -> Option<RealmGlobalBindingState> {
        match self {
            Self::Lookup => Some(RealmGlobalBindingState::Unresolved),
            Self::Var | Self::Function => Some(RealmGlobalBindingState::Object),
            Self::Let | Self::Const => None,
        }
    }

    const fn upgraded_object_state(
        self,
        current: RealmGlobalBindingState,
    ) -> Option<RealmGlobalBindingState> {
        match (self, current) {
            (Self::Lookup, current)
            | (Self::Var | Self::Function, current @ RealmGlobalBindingState::Object) => {
                Some(current)
            }
            (Self::Var | Self::Function, RealmGlobalBindingState::Unresolved) => {
                Some(RealmGlobalBindingState::Object)
            }
            (Self::Var | Self::Function, RealmGlobalBindingState::Lexical { .. })
            | (Self::Let | Self::Const, _) => None,
        }
    }

    const fn declares_object_property(self) -> bool {
        matches!(self, Self::Var | Self::Function)
    }

    const fn lexical_mutability(self) -> Option<bool> {
        match self {
            Self::Let => Some(true),
            Self::Const => Some(false),
            Self::Lookup | Self::Var | Self::Function => None,
        }
    }
}

const fn global_declaration_property_layout(
    executable_kind: CompilerExecutableKind,
) -> PropertyLayout {
    PropertyLayout::data(
        true,
        true,
        matches!(
            executable_kind,
            CompilerExecutableKind::DynamicFunctionScript
        ),
    )
}

fn global_function_replacement_layout(
    existing: PropertyLayout,
    declaration: PropertyLayout,
) -> Option<PropertyLayout> {
    if existing.is_configurable() {
        Some(declaration)
    } else if existing.writable() == Some(true) && existing.is_enumerable() {
        Some(existing)
    } else {
        None
    }
}

fn rejected_global_declaration(
    authority: &VerifiedBytecode,
    closure: u32,
    name: &Atom,
) -> Result<InstallError, InstallError> {
    let root = authority.root();
    let constant = root
        .metadata()
        .closures()
        .get(closure as usize)
        .and_then(quickjs_bytecode::ClosureVariableDefinition::function_initializer);
    let instructions = root.function().control_flow().instructions();
    let site = if let Some(constant) = constant {
        instructions
            .windows(2)
            .enumerate()
            .find_map(|(index, pair)| {
                let initializer = pair[0].decoded().instruction();
                let initializer_constant = match (initializer.opcode(), initializer.operands()) {
                    (FinalOpcode::FClosure, Operands::Const(value)) => Some(value),
                    (FinalOpcode::FClosure8, Operands::Const8(value)) => Some(u32::from(value)),
                    _ => None,
                };
                let put = pair[1].decoded().instruction();
                (initializer_constant == Some(constant)
                    && matches!(
                        (put.opcode(), put.operands()),
                        (FinalOpcode::PutVar, Operands::VarRef(slot))
                            if u32::from(slot) == closure
                    ))
                .then_some((index, pair[0].decoded().pc()))
            })
            .ok_or(InstallError::AuthorityInvariant {
                message: "global function declaration has no verified initializer site",
            })?
    } else {
        let first = instructions
            .first()
            .ok_or(InstallError::AuthorityInvariant {
                message: "global declaration Script has no instruction",
            })?;
        (0, first.decoded().pc())
    };
    let source_span = root
        .metadata()
        .source()
        .mappings()
        .get(site.0)
        .ok_or(InstallError::AuthorityInvariant {
            message: "global function declaration source mapping is missing",
        })?
        .span();
    let name = name
        .description()
        .cloned()
        .ok_or(InstallError::AuthorityInvariant {
            message: "global declaration name is not a string atom",
        })?;
    Ok(InstallError::GlobalDeclarationRejected {
        name,
        function: authority.root_id(),
        pc: site.1,
        source_span,
    })
}

pub(crate) fn global_declaration_error(
    authority: &VerifiedBytecode,
    name: &JsString,
    function: FunctionTemplateId,
    pc: quickjs_bytecode::BytecodePc,
    source_span: quickjs_bytecode::SourceByteSpan,
) -> Result<(JsString, crate::JsStackFrame), crate::ExecutionError> {
    let source = authority
        .function(function)
        .ok_or(crate::EngineFault::InvalidClosureEnvironment { function })?
        .metadata()
        .source();
    let message = JsString::from_utf8("cannot define variable '")?
        .concat(name)?
        .concat(&JsString::from_utf8("'")?)?;
    Ok((
        message,
        crate::JsStackFrame::new(
            function,
            pc,
            source.display_name_arc(),
            source.text_arc(),
            source_span,
        ),
    ))
}

/// One uniquely owned JavaScript runtime.
///
/// Mutable heap state is direct and lock-free. `Arc` backs immutable
/// bytecode/string storage plus runtime-local atom, public-handle, and mailbox
/// identity owners. Their accounting uses `Cell`, so the runtime and every
/// heap-bound handle are deliberately `!Send + !Sync`.
///
/// ```compile_fail
/// use quickjs_runtime::Runtime;
///
/// fn require_sync<T: Sync>() {}
/// require_sync::<Runtime>();
/// ```
pub struct Runtime {
    pub(crate) mailbox: Arc<ReleaseMailbox>,
    atoms: AtomTable,
    realms: Arena<crate::ids::RealmMarker, RealmState>,
    pub(crate) code: Arena<crate::ids::InstalledCodeMarker, InstalledCode>,
    pub(crate) functions: Arena<crate::ids::FunctionMarker, HeapFunction>,
    pub(crate) objects: Arena<crate::ids::ObjectMarker, HeapObject>,
    pub(crate) shape_interner: Rc<RefCell<ShapeInterner>>,
    pub(crate) cells: Arena<crate::ids::BindingCellMarker, BindingCell>,
    pub(crate) global_bindings: Arena<crate::ids::RealmGlobalBindingMarker, RealmGlobalBinding>,
    pub(crate) limits: RuntimeLimits,
    installed_templates: u64,
    installed_atoms: u64,
    installed_constants: u64,
    pub(crate) object_properties: u64,
    pub(crate) array_buffer_bytes: u64,
    pub(crate) for_in_entries: u64,
    pub(crate) collection_entries: u64,
    public_roots: u64,
    pub(crate) collection_pending: bool,
    pub(crate) interrupts: InterruptState,
    pub(crate) promise_rejections: PromiseRejectionState,
    pub(crate) promise_jobs: VecDeque<PromiseJob>,
    pub(crate) finalization_jobs: VecDeque<ObjectId>,
    pub(crate) kept_alive: Vec<StoredValue>,
    pub(crate) generator_states: HashMap<ObjectId, crate::vm::GeneratorRecord>,
    pub(crate) async_function_states: HashMap<ObjectId, crate::vm::AsyncFunctionRecord>,
    pub(crate) async_generator_states: HashMap<ObjectId, crate::vm::AsyncGeneratorRecord>,
    pub(crate) array_from_async_states: HashMap<ObjectId, crate::vm::ArrayFromAsyncRecord>,
    /// Next non-zero seed assigned after a realm transaction commits.
    next_math_random_seed: u64,
}

impl Runtime {
    /// Installs the host interrupt handler, replacing any previous one.
    ///
    /// The handler is polled on a decrementing counter rather than on every
    /// instruction (`INTERRUPT_POLL_INTERVAL`), so cancellation is observed
    /// within that many interpreter steps rather than immediately.
    ///
    /// Requesting cancellation reports [`crate::ExecutionError::Interrupted`], which is
    /// not a catchable JavaScript exception: a script must not be able to
    /// swallow a host cancellation.
    pub fn set_interrupt_handler(&mut self, handler: Arc<dyn crate::InterruptHandler>) {
        self.interrupts.set_handler(handler);
    }

    /// Removes the installed interrupt handler.
    pub fn clear_interrupt_handler(&mut self) {
        self.interrupts.clear_handler();
    }

    /// Returns whether an interrupt handler is installed.
    #[must_use]
    pub fn has_interrupt_handler(&self) -> bool {
        self.interrupts.is_installed()
    }

    /// Returns the predefined atom with the given descriptor.
    pub(crate) fn predefined_atom(&self, predefined: PredefinedAtom) -> Atom {
        self.atoms.predefined(predefined)
    }

    /// Returns a mutable reference to the bytecode function backing a function id.
    pub(crate) fn bytecode_function_mut(
        &mut self,
        id: FunctionId,
    ) -> Option<&mut BytecodeFunction> {
        match self.functions.get_mut(id) {
            Some(HeapFunction {
                implementation: FunctionImplementation::Bytecode(bytecode),
                ..
            }) => Some(bytecode),
            _ => None,
        }
    }

    /// Returns an immutable reference to the bytecode function backing a function id.
    pub(crate) fn bytecode_function(&self, id: FunctionId) -> Option<&BytecodeFunction> {
        match self.functions.get(id) {
            Some(HeapFunction {
                implementation: FunctionImplementation::Bytecode(bytecode),
                ..
            }) => Some(bytecode),
            _ => None,
        }
    }
}

mod arrays;
mod context;
mod for_in;
mod gc;
mod generators;
pub use gc::CollectionReport;
pub(crate) use gc::CollectionRoot;
mod heap;
mod installation;
mod realm;
#[cfg(test)]
mod realm_snapshot;
mod template_objects;

/// An exclusive runtime mutator bound to one active realm.
///
/// ```compile_fail
/// use quickjs_runtime::Context;
///
/// fn require_send<T: Send>(_: T) {}
/// fn context_is_runtime_local(context: Context<'_>) {
///     require_send(context);
/// }
/// ```
pub struct Context<'runtime> {
    pub(crate) runtime: &'runtime mut Runtime,
    pub(crate) realm: RealmId,
}

#[derive(Clone, Copy)]
enum RootPublication {
    Public,
    Internal,
}

impl RootPublication {
    const fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

pub(crate) struct InstalledRoot {
    pub(crate) function: FunctionId,
    pub(crate) code: InstalledCodeId,
    pending_environment: Option<PendingRootEnvironment>,
}

struct PendingRootEnvironment {
    realm: RealmId,
    environment: RootEnvironment,
}

impl InstalledRoot {
    pub(crate) fn commit_environment(&mut self) -> Result<(), crate::EngineFault> {
        self.pending_environment
            .take()
            .map(|_| ())
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "dynamic Script root environment was already committed",
            })
    }
}

struct RootEnvironment {
    bindings: Vec<EnvironmentBinding>,
    inserted_globals: Vec<(Atom, RealmGlobalBindingId)>,
    updated_globals: Vec<(RealmGlobalBindingId, RealmGlobalBindingState)>,
    inserted_cells: Vec<BindingCellId>,
    inserted_global_properties: Vec<PropertyKey>,
    updated_global_properties: Vec<(PropertyKey, OwnProperty)>,
}

fn runtime_string(
    value: &quickjs_bytecode::CompilerString,
) -> Result<JsString, crate::JsStringError> {
    if let Some(units) = value.latin1_units() {
        JsString::from_latin1(units)
    } else {
        JsString::from_code_units(value.code_units())
    }
}

fn predefined_string(atoms: &AtomTable, atom: PredefinedAtom) -> JsString {
    atoms
        .predefined(atom)
        .description()
        .expect("string predefined atom has a description")
        .clone()
}

fn require_root_kind(
    authority: &VerifiedBytecode,
    expected: CompilerExecutableKind,
) -> Result<(), InstallError> {
    let actual = authority.root().metadata().executable_kind();
    if actual == expected
        || (expected == CompilerExecutableKind::OrdinaryFunction
            && matches!(
                actual,
                CompilerExecutableKind::GeneratorFunction
                    | CompilerExecutableKind::AsyncFunction
                    | CompilerExecutableKind::AsyncGeneratorFunction
            ))
    {
        return Ok(());
    }
    let message = match expected {
        CompilerExecutableKind::GlobalScript => {
            "non-Script executable cannot execute as a host-loaded Global Script"
        }
        CompilerExecutableKind::OrdinaryFunction => {
            "non-instantiable executable cannot be instantiated as a source function"
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            "source function cannot execute as a dynamic-function Script"
        }
        CompilerExecutableKind::OrdinaryMethod
        | CompilerExecutableKind::ClassConstructor
        | CompilerExecutableKind::OrdinaryArrow
        | CompilerExecutableKind::GeneratorFunction
        | CompilerExecutableKind::GeneratorMethod
        | CompilerExecutableKind::AsyncFunction
        | CompilerExecutableKind::AsyncMethod
        | CompilerExecutableKind::AsyncGeneratorFunction
        | CompilerExecutableKind::AsyncGeneratorMethod => {
            "unsupported executable-kind admission request"
        }
    };
    Err(InstallError::AuthorityInvariant { message })
}

fn preflight_opcodes(authority: &VerifiedBytecode) -> Result<(), InstallError> {
    for (function_index, function) in authority.functions().enumerate() {
        let function_id = FunctionTemplateId::new(u32::try_from(function_index).map_err(|_| {
            InstallError::AuthorityInvariant {
                message: "function template index is not representable",
            }
        })?);
        let instructions = function.function().control_flow().instructions();
        let mappings = function.metadata().source().mappings();
        if instructions.len() != mappings.len() {
            return Err(InstallError::AuthorityInvariant {
                message: "instruction/source mapping lengths differ",
            });
        }
        for (instruction, mapping) in instructions.iter().zip(mappings) {
            let decoded = instruction.decoded();
            let instruction = decoded.instruction();
            let opcode = instruction.opcode();
            if !is_supported_instruction(instruction) {
                return Err(InstallError::UnsupportedOpcode {
                    function: function_id,
                    pc: decoded.pc(),
                    source_span: mapping.span(),
                    opcode,
                });
            }
        }
    }
    Ok(())
}

const fn is_supported_instruction(instruction: Instruction) -> bool {
    is_supported_opcode(instruction.opcode())
        && (!matches!(instruction.opcode(), FinalOpcode::ThrowError)
            || matches!(instruction.operands(), Operands::AtomU8 { value: 4, .. }))
}

#[allow(
    clippy::too_many_lines,
    reason = "whole-graph capability admission remains one exhaustive opcode audit"
)]
const fn is_supported_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PushI32
            | FinalOpcode::PushConst
            | FinalOpcode::FClosure
            | FinalOpcode::SetName
            | FinalOpcode::SetNameComputed
            | FinalOpcode::SetHomeObject
            | FinalOpcode::PushAtomValue
            | FinalOpcode::PrivateSymbol
            | FinalOpcode::PushBigIntI32
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
            | FinalOpcode::RegExp
            | FinalOpcode::SpecialObject
            | FinalOpcode::Rest
            | FinalOpcode::ArrayFrom
            | FinalOpcode::Append
            | FinalOpcode::Catch
            | FinalOpcode::Gosub
            | FinalOpcode::Ret
            | FinalOpcode::Drop
            | FinalOpcode::Nip
            | FinalOpcode::NipCatch
            | FinalOpcode::Dup
            | FinalOpcode::Dup1
            | FinalOpcode::Dup2
            | FinalOpcode::Dup3
            | FinalOpcode::Insert2
            | FinalOpcode::Insert3
            | FinalOpcode::Insert4
            | FinalOpcode::Swap
            | FinalOpcode::Rot3l
            | FinalOpcode::Rot3r
            | FinalOpcode::Call
            | FinalOpcode::CallMethod
            | FinalOpcode::CallConstructor
            | FinalOpcode::Apply
            | FinalOpcode::CheckCtorReturn
            | FinalOpcode::CheckCtor
            | FinalOpcode::InitCtor
            | FinalOpcode::GetSuper
            | FinalOpcode::GetSuperValue
            | FinalOpcode::PutSuperValue
            | FinalOpcode::Perm3
            | FinalOpcode::Perm4
            | FinalOpcode::Perm5
            | FinalOpcode::Throw
            | FinalOpcode::Return
            | FinalOpcode::ReturnUndef
            | FinalOpcode::ReturnAsync
            | FinalOpcode::Await
            | FinalOpcode::InitialYield
            | FinalOpcode::Yield
            | FinalOpcode::YieldStar
            | FinalOpcode::AsyncYieldStar
            | FinalOpcode::ThrowError
            | FinalOpcode::GetLoc
            | FinalOpcode::PutLoc
            | FinalOpcode::SetLoc
            | FinalOpcode::GetArg
            | FinalOpcode::PutArg
            | FinalOpcode::SetArg
            | FinalOpcode::GetVarUndef
            | FinalOpcode::GetVar
            | FinalOpcode::PutVar
            | FinalOpcode::PutVarInit
            | FinalOpcode::GetVarRef
            | FinalOpcode::PutVarRef
            | FinalOpcode::SetVarRef
            | FinalOpcode::SetLocUninitialized
            | FinalOpcode::GetLocCheck
            | FinalOpcode::PutLocCheck
            | FinalOpcode::SetLocCheck
            | FinalOpcode::GetVarRefCheck
            | FinalOpcode::PutVarRefCheck
            | FinalOpcode::CloseLoc
            | FinalOpcode::ForInStart
            | FinalOpcode::ForInNext
            | FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::IteratorClose
            | FinalOpcode::IteratorNext
            | FinalOpcode::IteratorCall
            | FinalOpcode::IteratorCheckObject
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::GetPrivateField
            | FinalOpcode::PrivateIn
            | FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::PutField
            | FinalOpcode::PutPrivateField
            | FinalOpcode::PutArrayEl
            | FinalOpcode::Delete
            | FinalOpcode::DeleteVar
            | FinalOpcode::SetProto
            | FinalOpcode::ToObject
            | FinalOpcode::ToPropKey
            | FinalOpcode::CopyDataProperties
            | FinalOpcode::DefineField
            | FinalOpcode::DefinePrivateField
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::DefineClass
            | FinalOpcode::DefineMethod
            | FinalOpcode::DefineMethodComputed
            | FinalOpcode::IfFalse
            | FinalOpcode::IfTrue
            | FinalOpcode::Goto
            | FinalOpcode::Neg
            | FinalOpcode::Plus
            | FinalOpcode::Dec
            | FinalOpcode::Inc
            | FinalOpcode::PostDec
            | FinalOpcode::PostInc
            | FinalOpcode::Not
            | FinalOpcode::Lnot
            | FinalOpcode::Typeof
            | FinalOpcode::Mul
            | FinalOpcode::Div
            | FinalOpcode::Mod
            | FinalOpcode::Add
            | FinalOpcode::Sub
            | FinalOpcode::Pow
            | FinalOpcode::Shl
            | FinalOpcode::Sar
            | FinalOpcode::Shr
            | FinalOpcode::Lt
            | FinalOpcode::Lte
            | FinalOpcode::Gt
            | FinalOpcode::Gte
            | FinalOpcode::Eq
            | FinalOpcode::Neq
            | FinalOpcode::StrictEq
            | FinalOpcode::StrictNeq
            | FinalOpcode::InstanceOf
            | FinalOpcode::In
            | FinalOpcode::And
            | FinalOpcode::Xor
            | FinalOpcode::Or
            | FinalOpcode::IsNull
            | FinalOpcode::IsUndefinedOrNull
            | FinalOpcode::Nop
            | FinalOpcode::PushMinus1
            | FinalOpcode::Push0
            | FinalOpcode::Push1
            | FinalOpcode::Push2
            | FinalOpcode::Push3
            | FinalOpcode::Push4
            | FinalOpcode::Push5
            | FinalOpcode::Push6
            | FinalOpcode::Push7
            | FinalOpcode::PushI8
            | FinalOpcode::PushI16
            | FinalOpcode::PushConst8
            | FinalOpcode::FClosure8
            | FinalOpcode::PushEmptyString
            | FinalOpcode::GetLoc8
            | FinalOpcode::PutLoc8
            | FinalOpcode::SetLoc8
            | FinalOpcode::GetLoc0
            | FinalOpcode::GetLoc1
            | FinalOpcode::GetLoc2
            | FinalOpcode::GetLoc3
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
            | FinalOpcode::SetLoc0
            | FinalOpcode::SetLoc1
            | FinalOpcode::SetLoc2
            | FinalOpcode::SetLoc3
            | FinalOpcode::GetArg0
            | FinalOpcode::GetArg1
            | FinalOpcode::GetArg2
            | FinalOpcode::GetArg3
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
            | FinalOpcode::SetArg0
            | FinalOpcode::SetArg1
            | FinalOpcode::SetArg2
            | FinalOpcode::SetArg3
            | FinalOpcode::GetVarRef0
            | FinalOpcode::GetVarRef1
            | FinalOpcode::GetVarRef2
            | FinalOpcode::GetVarRef3
            | FinalOpcode::PutVarRef0
            | FinalOpcode::PutVarRef1
            | FinalOpcode::PutVarRef2
            | FinalOpcode::PutVarRef3
            | FinalOpcode::SetVarRef0
            | FinalOpcode::SetVarRef1
            | FinalOpcode::SetVarRef2
            | FinalOpcode::SetVarRef3
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::IfFalse8
            | FinalOpcode::IfTrue8
            | FinalOpcode::Goto8
            | FinalOpcode::Goto16
    )
}

fn check_limit(resource: RuntimeResource, limit: u64, observed: u64) -> Result<(), RuntimeError> {
    if observed <= limit {
        Ok(())
    } else {
        Err(RuntimeError::LimitExceeded {
            resource,
            limit,
            observed,
        })
    }
}

fn stale_heap_reference(reference: HeapReference) -> crate::EngineFault {
    match reference {
        HeapReference::Function(function) => crate::EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        },
        HeapReference::Object(object) => crate::EngineFault::StaleHeapEdge {
            edge: "object",
            index: object.index(),
            generation: object.generation(),
        },
    }
}

fn check_install_limit(
    resource: RuntimeResource,
    limit: u64,
    observed: u64,
) -> Result<(), InstallError> {
    if observed <= limit {
        Ok(())
    } else {
        Err(InstallError::LimitExceeded {
            resource,
            limit,
            observed,
        })
    }
}

pub(crate) fn check_execution_limit(
    resource: RuntimeResource,
    limit: u64,
    observed: u64,
) -> Result<(), crate::ExecutionError> {
    if observed <= limit {
        Ok(())
    } else {
        Err(crate::ExecutionError::LimitExceeded {
            resource,
            limit,
            observed,
        })
    }
}

pub(crate) fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn for_in_snapshot_work_upper_bound(property_count: usize, string_length: Option<u32>) -> u64 {
    let property_count = usize_to_u64(property_count);
    let candidate_count =
        property_count.saturating_add(u64::from(string_length.unwrap_or_default()));
    property_count
        .saturating_mul(4)
        .saturating_add(candidate_count)
        .saturating_add(conservative_for_in_sort_work(candidate_count))
        .saturating_add(1)
}

fn conservative_for_in_sort_work(entries: u64) -> u64 {
    if entries <= 1 {
        return 0;
    }
    let levels = u64::from(u64::BITS - (entries - 1).leading_zeros());
    entries.saturating_mul(levels).saturating_mul(2)
}

#[cfg(test)]
mod tests;
