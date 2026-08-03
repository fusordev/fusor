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
    collections::{HashMap, HashSet},
    sync::{Arc, Weak},
};

use quickjs_bytecode::{
    CompilerBindingKind, CompilerBindingPolicy, CompilerCapturedBinding, CompilerClosureBinding,
    CompilerConstant, CompilerConstantValue, CompilerExecutableKind, FinalOpcode,
    FunctionTemplateId, Operands, VerifiedBytecode,
};

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
        HeapObject, IntegrityLevel, KeyPhases, ObjectRecord, OwnProperty, PropertyDeletion,
        StringIterator,
    },
    value::{HeapReference, PrimitiveValue, ReleaseMailbox, RootTarget, SlotValue, StoredValue},
};

mod iterators;
mod limits;
mod symbols;
pub(crate) use iterators::PreparedIteratorResultPlan;
pub use limits::{RuntimeLimits, RuntimeUsage};

struct RealmState {
    object_prototype: ObjectId,
    global_object: ObjectId,
    intrinsics: RealmIntrinsics,
    global_bindings: HashMap<Atom, RealmGlobalBindingId>,
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
        function_constructor: FunctionId,
        errors: ErrorIntrinsics,
        boolean: BooleanIntrinsics,
        number: NumberIntrinsics,
        bigint: BigIntIntrinsics,
        string: StringIntrinsics,
        array: ArrayIntrinsics,
        symbol: SymbolIntrinsics,
        iterators: IteratorIntrinsics,
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
    array_iterator_prototype: ObjectId,
    string_iterator_prototype: ObjectId,
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

/// Which by-copy builder a continuation is performing.
///
/// These are the ES2023 change-by-copy methods that answer a fresh dense
/// Array: holes in the receiver become present `undefined` elements, because
/// the pinned oracle reads with `JS_TryGetPropertyInt64`, which reports an
/// absent index as `undefined` (`quickjs.c:9115-9142`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayByCopy {
    With,
    ToReversed,
    ToSpliced,
}

impl ArrayByCopy {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            Self::With | Self::ToSpliced => 2,
            Self::ToReversed => 0,
        }
    }

    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::With => "with",
            Self::ToReversed => "toReversed",
            Self::ToSpliced => "toSpliced",
        }
    }
}

/// Which flattening method a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayFlatten {
    Flat,
    FlatMap,
}

impl ArrayFlatten {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            // `flat` reports 0 even though it accepts a depth argument;
            // `flatMap` reports 1. The pinned oracle confirms both.
            Self::Flat => 0,
            Self::FlatMap => 1,
        }
    }

    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::FlatMap => "flatMap",
        }
    }
}

/// Which sort a continuation is performing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArraySort {
    Sort,
    ToSorted,
}

impl ArraySort {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        // Both report 1, which the pinned oracle confirms.
        match self {
            Self::Sort | Self::ToSorted => 1,
        }
    }

    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Sort => "sort",
            Self::ToSorted => "toSorted",
        }
    }
}

/// Which `Reflect` method a native function implements.
///
/// Each of these takes an object target and reports
/// `TypeError: not an object` for anything else, including a primitive that the
/// matching `Object` static would accept — the `reflect` magic flag in
/// `js_object_isExtensible`, `js_object_preventExtensions`,
/// `js_object_getPrototypeOf`, `js_object_defineProperty`, and
/// `js_object_getOwnPropertyDescriptor` (`quickjs.c:40026-40400`,
/// `quickjs.c:50215-50329`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReflectMethod {
    Get,
    Set,
    Has,
    DeleteProperty,
    OwnKeys,
    GetPrototypeOf,
    SetPrototypeOf,
    IsExtensible,
    PreventExtensions,
    DefineProperty,
    GetOwnPropertyDescriptor,
}

impl ReflectMethod {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            Self::OwnKeys | Self::GetPrototypeOf | Self::IsExtensible | Self::PreventExtensions => {
                1
            }
            Self::Get
            | Self::Has
            | Self::DeleteProperty
            | Self::SetPrototypeOf
            | Self::GetOwnPropertyDescriptor => 2,
            Self::Set | Self::DefineProperty => 3,
        }
    }

    /// Returns the predefined atom this method is installed under.
    ///
    /// Every `Reflect` name is predefined, so none is interned per realm; a
    /// duplicate interning would break the atom table's rollback invariant.
    pub(crate) const fn predefined_atom(self) -> PredefinedAtom {
        match self {
            Self::Get => PredefinedAtom::Get,
            Self::Set => PredefinedAtom::SetProperty,
            Self::Has => PredefinedAtom::Has,
            Self::DeleteProperty => PredefinedAtom::DeleteProperty,
            Self::OwnKeys => PredefinedAtom::OwnKeys,
            Self::GetPrototypeOf => PredefinedAtom::GetPrototypeOf,
            Self::SetPrototypeOf => PredefinedAtom::SetPrototypeOf,
            Self::IsExtensible => PredefinedAtom::IsExtensible,
            Self::PreventExtensions => PredefinedAtom::PreventExtensions,
            Self::DefineProperty => PredefinedAtom::DefineProperty,
            Self::GetOwnPropertyDescriptor => PredefinedAtom::GetOwnPropertyDescriptor,
        }
    }
}

/// Which `Object` own-key listing a native function implements.
///
/// Both walk the target's own enumerable string keys and read each one, so both
/// share one resumable continuation; they differ only in whether an element is
/// the value alone or a `[key, value]` pair
/// (`JS_ITERATOR_KIND_VALUE` versus `KIND_KEY_AND_VALUE`,
/// `quickjs.c:40206-40260`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectListing {
    Values,
    Entries,
}

impl ObjectListing {
    /// Returns the property name this static is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Values => "values",
            Self::Entries => "entries",
        }
    }

    /// Returns whether each element is a `[key, value]` pair.
    pub(crate) const fn is_paired(self) -> bool {
        matches!(self, Self::Entries)
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
}

impl ArrayCopier {
    /// Returns the reported `length` of the installed function.
    pub(crate) const fn arity(self) -> i32 {
        match self {
            Self::Slice => 2,
            // `concat` is variadic and `at` takes one index; both report 1.
            Self::Concat | Self::At => 1,
        }
    }

    /// Returns the property name this method is installed under.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Slice => "slice",
            Self::Concat => "concat",
            Self::At => "at",
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
            Self::Pop | Self::Shift | Self::Reverse => 0,
            // `copyWithin` reports 2, which the pinned oracle confirms.
            Self::CopyWithin => 2,
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
    Slice,
    StartsWith,
    Substr,
    Substring,
    Trim,
    TrimEnd,
    TrimStart,
    IsWellFormed,
    ToWellFormed,
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
            | Self::Concat
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
            // `substr`'s second argument is a length rather than an end index,
            // but it is coerced identically; the difference is in the body.
            Self::Slice | Self::Substring | Self::Substr => {
                &[StringArgument::Integer, StringArgument::OptionalInteger]
            }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeFunctionKind {
    FunctionPrototype,
    FunctionPrototypeApply,
    FunctionPrototypeCall,
    FunctionPrototypeBind,
    FunctionPrototypeHasInstance,
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
    ObjectDefineProperty,
    ObjectGetOwnPropertyDescriptor,
    ObjectCreate,
    /// `Object.is`, which is `SameValue` rather than strict equality.
    ObjectIs,
    /// `Object.hasOwn`, which is `hasOwnProperty` with the target as its first
    /// argument rather than as its receiver.
    ObjectHasOwn,
    /// `Object.getOwnPropertySymbols`, the symbol-only own-key listing.
    ObjectGetOwnPropertySymbols,
    /// One `Object` listing sharing the resumable own-key walk.
    ObjectListing(ObjectListing),
    /// `Object.getOwnPropertyDescriptors`, which reads no value and so never
    /// suspends.
    ObjectGetOwnPropertyDescriptors,
    /// `Object.assign`, which interleaves a source read with a target write per
    /// key across several sources.
    ObjectAssign,
    /// `Object.defineProperties`, which validates every descriptor before
    /// applying any of them.
    ObjectDefineProperties,
    /// `Object.fromEntries`, which drains an iterable of `[key, value]` pairs.
    ObjectFromEntries,
    ObjectPrototypeToString,
    ObjectPrototypeValueOf,
    ObjectPrototypeHasOwnProperty,
    ObjectPrototypeIsPrototypeOf,
    ObjectPrototypePropertyIsEnumerable,
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
    /// One `Number` predicate static.
    NumberPredicateStatic(NumberPredicate),
    /// `Array.isArray`.
    ArrayIsArray,
    /// One `Array.prototype` search sharing the resumable element loop.
    ArrayPrototypeSearch(ArraySearch),
    /// One `Array.prototype` mutator sharing the resumable element driver.
    ArrayPrototypeMutator(ArrayMutator),
    /// One `Array.prototype` copying method sharing the resumable element read.
    ArrayPrototypeCopier(ArrayCopier),
    /// One `Array.prototype` callback method sharing the resumable loop.
    ArrayPrototypeCallback(ArrayCallback),
    /// One `Array.prototype` reduction sharing the resumable fold.
    ArrayPrototypeReduction(ArrayReduction),
    /// `Array.prototype.splice`.
    ArrayPrototypeSplice,
    /// One `Array.prototype` change-by-copy method sharing the resumable
    /// dense snapshot read.
    ArrayPrototypeByCopy(ArrayByCopy),
    /// One `Array.prototype` flattening method sharing the resumable
    /// worklist driver.
    ArrayPrototypeFlatten(ArrayFlatten),
    /// One `Array.prototype` sort sharing the resumable merge driver.
    ArrayPrototypeSort(ArraySort),
    ArrayConstructor,
    SymbolConstructor,
    SymbolPrototypeToString,
    SymbolPrototypeValueOf,
    SymbolPrototypeToPrimitive,
    SymbolPrototypeDescription,
    SymbolFor,
    SymbolKeyFor,
    IteratorPrototypeIterator,
    /// `Reflect.apply`, which shares the `Function.prototype.apply` machinery.
    ReflectApply,
    /// `Reflect.construct`, which validates `newTarget` before the argument
    /// list and its target after it.
    ReflectConstruct,
    /// One `Reflect` method whose target must already be an object and whose
    /// key, when it takes one, converts with a resumable `ToPropertyKey`.
    ReflectMethod(ReflectMethod),
    ArrayPrototypeJoin,
    ArrayPrototypeToString,
    ArrayPrototypeValues,
    ArrayPrototypeKeys,
    ArrayPrototypeEntries,
    ArrayIteratorNext,
    StringPrototypeIterator,
    StringIteratorNext,
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
        }
    }

    pub(crate) const fn native(&self) -> Option<&NativeFunction> {
        match &self.implementation {
            FunctionImplementation::Bytecode(_) | FunctionImplementation::Bound(_) => None,
            FunctionImplementation::Native(function) => Some(function),
        }
    }

    pub(crate) fn bound(&self) -> Option<&BoundFunction> {
        match &self.implementation {
            FunctionImplementation::Bytecode(_) | FunctionImplementation::Native(_) => None,
            FunctionImplementation::Bound(bound) => Some(bound),
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
pub use gc::CollectionReport;
pub(crate) use gc::CollectionRoot;
mod heap;
mod installation;
mod realm;

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
    if actual == expected {
        return Ok(());
    }
    let message = match (expected, actual) {
        (
            CompilerExecutableKind::OrdinaryFunction,
            CompilerExecutableKind::DynamicFunctionScript,
        ) => "dynamic-function Script cannot be instantiated as an ordinary function",
        (CompilerExecutableKind::OrdinaryFunction, CompilerExecutableKind::OrdinaryMethod) => {
            "ordinary method cannot be instantiated as an ordinary function"
        }
        (CompilerExecutableKind::OrdinaryMethod, CompilerExecutableKind::OrdinaryFunction) => {
            "ordinary function cannot be instantiated as an ordinary method"
        }
        (CompilerExecutableKind::OrdinaryMethod, CompilerExecutableKind::DynamicFunctionScript) => {
            "dynamic-function Script cannot be instantiated as an ordinary method"
        }
        (
            CompilerExecutableKind::DynamicFunctionScript,
            CompilerExecutableKind::OrdinaryFunction,
        ) => "ordinary function cannot execute as a dynamic-function Script",
        (CompilerExecutableKind::DynamicFunctionScript, CompilerExecutableKind::OrdinaryMethod) => {
            "ordinary method cannot execute as a dynamic-function Script"
        }
        _ => {
            return Err(InstallError::AuthorityInvariant {
                message: "matching executable kinds reached rejection",
            });
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
            let opcode = decoded.instruction().opcode();
            if !is_supported_opcode(opcode) {
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
            | FinalOpcode::PushAtomValue
            | FinalOpcode::PushBigIntI32
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
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
            | FinalOpcode::ForOfNext
            | FinalOpcode::IteratorClose
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
