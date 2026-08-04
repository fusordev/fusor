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
    ExceptionKind, ExecutionLimits, Function, HandleError, HandleKind, InstallError, JsBigInt,
    JsNumber, JsString, JsValue, OrdinaryDynamicFunctionCompiler, PredefinedAtom, PropertyKey,
    PropertyLayout, PropertyLayoutKind, RuntimeError, RuntimeResource,
    arena::{Arena, RuntimeIdentity},
    ids::{BindingCellId, FunctionId, InstalledCodeId, ObjectId, RealmGlobalBindingId, RealmId},
    interrupt::InterruptState,
    object::{
        ArrayIterator, ArrayIteratorKind, ArrayState, BoxedPrimitive, ForInIterator, ForInSnapshot,
        HeapObject, IntegrityLevel, KeyPhases, ObjectRecord, OwnProperty, PromiseCapability,
        PromiseReaction, PropertyDeletion, StringIterator,
    },
    value::{HeapReference, PrimitiveValue, ReleaseMailbox, RootTarget, SlotValue, StoredValue},
};

mod async_functions;
mod iterators;
mod limits;
mod promises;
mod symbols;
pub(crate) use iterators::PreparedIteratorResultPlan;
pub use limits::{RuntimeLimits, RuntimeUsage};

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
        promise: PromiseIntrinsics,
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

    #[cfg(test)]
    const fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::EvalError => "EvalError",
            Self::RangeError => "RangeError",
            Self::ReferenceError => "ReferenceError",
            Self::SyntaxError => "SyntaxError",
            Self::TypeError => "TypeError",
            Self::UriError => "URIError",
            Self::InternalError => "InternalError",
            Self::AggregateError => "AggregateError",
        }
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
struct PromiseIntrinsics {
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
    iterator_prototype: ObjectId,
    async_iterator_prototype: ObjectId,
    async_from_sync_iterator_prototype: ObjectId,
    async_from_sync_iterator_next: FunctionId,
    array_iterator_prototype: ObjectId,
    string_iterator_prototype: ObjectId,
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

pub(crate) enum ForInAdvance {
    Continue { work: u64 },
    Yield { key: PropertyKey, work: u64 },
    Done { work: u64 },
}

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
    Function(FunctionTemplateId),
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
pub(crate) enum StringHtmlMethod {
    Anchor,
    Big,
    Blink,
    Bold,
    Fixed,
    FontColor,
    FontSize,
    Italics,
    Link,
    Small,
    Strike,
    Sub,
    Sup,
}

impl StringHtmlMethod {
    /// Returns the HTML element name passed to the specification's
    /// `CreateHTML` abstract operation.
    pub(crate) const fn tag_name(self) -> &'static str {
        match self {
            Self::Anchor | Self::Link => "a",
            Self::Big => "big",
            Self::Blink => "blink",
            Self::Bold => "b",
            Self::Fixed => "tt",
            Self::FontColor | Self::FontSize => "font",
            Self::Italics => "i",
            Self::Small => "small",
            Self::Strike => "strike",
            Self::Sub => "sub",
            Self::Sup => "sup",
        }
    }

    /// Returns the optional attribute name passed to `CreateHTML`.
    pub(crate) const fn attribute_name(self) -> Option<&'static str> {
        match self {
            Self::Anchor => Some("name"),
            Self::FontColor => Some("color"),
            Self::FontSize => Some("size"),
            Self::Link => Some("href"),
            Self::Big
            | Self::Blink
            | Self::Bold
            | Self::Fixed
            | Self::Italics
            | Self::Small
            | Self::Strike
            | Self::Sub
            | Self::Sup => None,
        }
    }
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
    /// `String.prototype.replace`, whose `@@replace` protocol dispatch must run
    /// before the receiver and fallback arguments are string-coerced.
    Replace,
    Slice,
    StartsWith,
    Substr,
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
    /// One Annex B HTML wrapper implemented through `CreateHTML`.
    Html(StringHtmlMethod),
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
            | Self::Replace
            | Self::FromCharCode
            | Self::FromCodePoint => &[],
            Self::Html(method) => {
                if method.attribute_name().is_some() {
                    &[StringArgument::String]
                } else {
                    &[]
                }
            }
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
            // `substr`'s second argument is a length rather than an end index,
            // but it is coerced identically; the difference is in the body.
            Self::Slice | Self::Substring | Self::Substr => {
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
    ObjectPrototypeProtoGetter,
    ObjectPrototypeProtoSetter,
    ObjectPrototypeDefineGetter,
    ObjectPrototypeDefineSetter,
    ObjectPrototypeLookupGetter,
    ObjectPrototypeLookupSetter,
    /// One method on the ordinary `%Reflect%` object.
    Reflect(ReflectMethod),
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
    SymbolConstructor,
    SymbolPrototypeToString,
    SymbolPrototypeValueOf,
    SymbolPrototypeToPrimitive,
    SymbolPrototypeDescription,
    SymbolFor,
    SymbolKeyFor,
    IteratorPrototypeIterator,
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
    StringPrototypeIterator,
    StringIteratorNext,
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
                | Self::GeneratorFunctionConstructor
                | Self::AsyncFunctionConstructor
                | Self::AsyncGeneratorFunctionConstructor
                | Self::PromiseConstructor
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
        }
    }

    pub(crate) const fn native(&self) -> Option<&NativeFunction> {
        match &self.implementation {
            FunctionImplementation::Bytecode(_)
            | FunctionImplementation::Bound(_)
            | FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::PromiseCombinatorElement(_) => None,
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
            | FunctionImplementation::PromiseCombinatorElement(_) => None,
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
            | FunctionImplementation::PromiseCombinatorElement(_) => None,
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
            | FunctionImplementation::PromiseCombinatorElement(_) => None,
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
            | FunctionImplementation::PromiseCombinatorElement(_) => None,
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
            | FunctionImplementation::PromiseFinally(_) => None,
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
}

#[derive(Clone, Copy)]
enum RealmGlobalRequest {
    Lookup,
    Var,
    Function,
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
            _ => Err(InstallError::AuthorityInvariant {
                message: "unsupported constructor-realm global declaration policy",
            }),
        }
    }

    const fn initial_state(self) -> RealmGlobalBindingState {
        match self {
            Self::Lookup => RealmGlobalBindingState::Unresolved,
            Self::Var | Self::Function => RealmGlobalBindingState::Object,
        }
    }

    const fn upgraded_state(self, current: RealmGlobalBindingState) -> RealmGlobalBindingState {
        match (self, current) {
            (Self::Lookup, current)
            | (Self::Var | Self::Function, current @ RealmGlobalBindingState::Object) => current,
            (Self::Var | Self::Function, RealmGlobalBindingState::Unresolved) => {
                RealmGlobalBindingState::Object
            }
        }
    }

    const fn declares_object_property(self) -> bool {
        !matches!(self, Self::Lookup)
    }
}

const fn dynamic_function_declaration_property_layout() -> PropertyLayout {
    PropertyLayout::data(true, true, true)
}

fn global_function_replacement_layout(existing: PropertyLayout) -> Option<PropertyLayout> {
    if existing.is_configurable() {
        Some(dynamic_function_declaration_property_layout())
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
    pub(crate) cells: Arena<crate::ids::BindingCellMarker, BindingCell>,
    pub(crate) global_bindings: Arena<crate::ids::RealmGlobalBindingMarker, RealmGlobalBinding>,
    pub(crate) limits: RuntimeLimits,
    installed_templates: u64,
    installed_atoms: u64,
    installed_constants: u64,
    pub(crate) object_properties: u64,
    pub(crate) for_in_entries: u64,
    public_roots: u64,
    pub(crate) collection_pending: bool,
    pub(crate) interrupts: InterruptState,
    pub(crate) promise_rejections: PromiseRejectionState,
    pub(crate) promise_jobs: VecDeque<PromiseJob>,
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
        CompilerExecutableKind::OrdinaryFunction => {
            "non-instantiable executable cannot be instantiated as a source function"
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            "source function cannot execute as a dynamic-function Script"
        }
        CompilerExecutableKind::OrdinaryMethod
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
            | FinalOpcode::PushAtomValue
            | FinalOpcode::PushBigIntI32
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
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
            | FinalOpcode::Insert2
            | FinalOpcode::Insert3
            | FinalOpcode::Swap
            | FinalOpcode::Rot3l
            | FinalOpcode::Rot3r
            | FinalOpcode::Call
            | FinalOpcode::CallMethod
            | FinalOpcode::CallConstructor
            | FinalOpcode::Apply
            | FinalOpcode::Perm3
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
            | FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::PutField
            | FinalOpcode::PutArrayEl
            | FinalOpcode::Delete
            | FinalOpcode::SetProto
            | FinalOpcode::ToObject
            | FinalOpcode::ToPropKey
            | FinalOpcode::CopyDataProperties
            | FinalOpcode::DefineField
            | FinalOpcode::DefineArrayEl
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
            | FinalOpcode::And
            | FinalOpcode::Xor
            | FinalOpcode::Or
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
