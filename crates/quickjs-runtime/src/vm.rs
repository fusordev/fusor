/*
 * JavaScript bytecode execution and closure semantics derived from QuickJS.
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

//! Iterative execution of runtime-installed verified bytecode.

use std::{cell::RefCell, collections::VecDeque, error::Error, fmt, rc::Rc, sync::Arc};

use quickjs_bytecode::{
    BytecodePc, CompilerBindingKind, CompilerClosureBinding, CompilerClosureSource,
    CompilerExecutableKind, FinalOpcode, FunctionKind, FunctionTemplateId, InstructionIndex,
    Operands, SourceByteSpan, VerifiedBytecodeFunction, VerifiedSuccessorKind,
};

#[cfg(test)]
use crate::runtime::ForInAdvance;
use crate::{
    ArrayIndex, BigIntError, Context, DynamicFunctionCompileFailure, DynamicFunctionFamily,
    EngineFault, ExceptionKind, ExecutionError, Function, HandleError, HandleKind, JsBigInt,
    JsException, JsNumber, JsStackFrame, JsString, JsStringError, JsValue, MAX_STRING_CODE_UNITS,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, PredefinedAtom, PropertyKey,
    PropertyLayout, Runtime, RuntimeError, RuntimeResource,
    conversion::{
        MAX_SAFE_INTEGER, number_to_index, number_to_int8, number_to_int16, number_to_int32,
        number_to_integer_or_infinity, number_to_length, number_to_uint8, number_to_uint16,
        number_to_uint32, string_to_number, string_to_parse_float, string_to_parse_int,
    },
    define_property::{
        DefinitionDecision, PropertyDefinition, Requested, validate_and_apply_existing,
        validate_and_apply_new,
    },
    ids::{BindingCellId, FunctionId, InstalledCodeId, ObjectId, RealmGlobalBindingId, RealmId},
    interrupt::InterruptCounter,
    number::decimal::{DecimalDigits, exact_fixed, exact_significant},
    object::{
        DataViewByteLength, DataViewState, ForInSnapshot, IntegrityLevel, KeyPhases, OwnProperty,
        PropertyDeletion, TypedArrayElementType, TypedArrayLength, TypedArrayState,
    },
    runtime::{
        ArrayBufferPrototypeMethod, ArrayCallback, ArrayCopier, ArrayDefineOutcome, ArrayFlatten,
        ArrayLengthWriteOutcome, ArrayMutator, ArrayReduction, ArraySearch, ArraySort, ArrayStatic,
        BindingCell, BoundFunction, BytecodeFunction, CollectionRoot, DataViewElementType,
        DataViewPrototypeMethod, DatePrototypeMethod, DateStaticMethod, EnvironmentBinding,
        FinalizationRegistryMethod, FrameBindingAddress, FunctionImplementation,
        GlobalNumericFunction, HeapFunction, InstalledCode, InstalledConstant, InstalledRoot,
        InstalledTemplate, LocaleStringMethod, MapMethod, MathMethod, NativeFunction,
        NativeFunctionKind, NumberFormat, NumberPredicate, PreparedIteratorResultPlan,
        PromiseCapabilityCapture, PromiseCapabilityExecutor, PromiseCombinatorElementFunction,
        PromiseCombinatorElementKind, PromiseCombinatorKind, PromiseCombinatorShared,
        PromiseFinallyFunction, PromiseFinallyThunkKind, PromiseJob, PromiseResolvingFunction,
        PromiseResolvingKind, PromiseStatic, RealmGlobalBindingState, ReflectMethod, RegExpFlag,
        RegExpSymbolMethod, SetMethod, SetPrototypeOutcome, StringArgument, StringMethod,
        TemporalDurationPrototypeMethod, TemporalDurationStaticMethod,
        TemporalInstantPrototypeMethod, TemporalInstantStaticMethod, TypedArrayElementValue,
        TypedArrayOwnProperty, TypedArrayPropertyKey, TypedArrayPrototypeMethod,
        TypedArrayStoreOutcome, TypedArrayView, UriFunction, WeakMapMethod, WeakSetMethod,
        array_length_from_number, check_execution_limit, global_declaration_error, usize_to_u64,
    },
    value::{HeapReference, SlotValue, StoredValue},
};

mod aggregate_error;
mod array_buffer;
mod array_callbacks;
mod array_copiers;
mod array_flatten;
mod array_from_async;
mod array_join;
mod array_mutators;
mod array_search;
mod array_sort;
mod array_statics;
mod async_from_sync;
mod async_function;
mod async_generator;
mod bigint_intrinsics;
mod bindings;
mod conversions;
mod data_view;
mod date;
mod define_property_intrinsics;
mod dynamic;
mod error_stack;
mod errors;
mod exceptions;
mod execution;
mod for_in;
mod from_entries;
mod generator;
mod group_by;
mod instanceof;
mod iterators;
mod json_parse;
mod json_stringify;
mod locale_string;
mod map;
mod math;
mod math_sum_precise;
mod native;
mod object_intrinsics;
mod promise;
mod promise_combinators;
mod properties;
mod proxy;
mod reflect;
mod regexp;
mod set;
mod stack;
mod string_methods;
mod string_raw;
mod string_replace;
mod string_split;
mod temporal;
mod typed_array;
mod uri;
mod weak_collections;
mod weak_references;

pub(crate) use array_from_async::ArrayFromAsyncRecord;
use async_function::{begin_async_await, suspend_async_function};

#[allow(
    clippy::wildcard_imports,
    reason = "private VM sibling modules share one interpreter implementation namespace"
)]
use {
    aggregate_error::*, array_buffer::*, array_callbacks::*, array_copiers::*, array_flatten::*,
    array_from_async::*, array_join::*, array_mutators::*, array_search::*, array_sort::*,
    array_statics::*, async_from_sync::*, async_generator::*, bigint_intrinsics::*, bindings::*,
    conversions::*, data_view::*, date::*, define_property_intrinsics::*, dynamic::*,
    error_stack::*, errors::*, exceptions::*, execution::*, for_in::*, from_entries::*,
    generator::*, group_by::*, iterators::*, json_parse::*, json_stringify::*, locale_string::*,
    map::*, math::*, math_sum_precise::*, native::*, object_intrinsics::*, promise::*,
    promise_combinators::*, properties::*, proxy::*, reflect::*, regexp::*, set::*, stack::*,
    string_methods::*, string_raw::*, string_replace::*, string_split::*, temporal::*,
    typed_array::*, uri::*, weak_collections::*, weak_references::*,
};

/// Inclusive per-call interpreter limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionLimits {
    instruction_fuel: u64,
    dynamic_compilations: u64,
    dynamic_source_code_units: u64,
}

impl ExecutionLimits {
    /// Replaces the maximum interpreter work units in one execution session.
    ///
    /// Every completed bytecode instruction costs one unit. Bounded internal
    /// scans, such as `for-in` advancement and `Function.prototype.apply`
    /// argument collection, debit additional units from the same budget.
    #[must_use]
    pub const fn with_instruction_fuel(mut self, instruction_fuel: u64) -> Self {
        self.instruction_fuel = instruction_fuel;
        self
    }

    /// Replaces the maximum dynamic-function compilations in one interpreter
    /// session.
    #[must_use]
    pub const fn with_dynamic_compilations(mut self, maximum: u64) -> Self {
        self.dynamic_compilations = maximum;
        self
    }

    /// Replaces the maximum aggregate generated dynamic-Function UTF-16 code
    /// units in one interpreter session.
    #[must_use]
    pub const fn with_dynamic_source_code_units(mut self, maximum: u64) -> Self {
        self.dynamic_source_code_units = maximum;
        self
    }
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            instruction_fuel: 10_000_000,
            dynamic_compilations: 1_024,
            dynamic_source_code_units: 16_777_216,
        }
    }
}

struct ExecutionBudget {
    /// The decrementing interrupt poll counter for this session.
    interrupt_counter: InterruptCounter,
    instruction_limit: u64,
    executed_instructions: u64,
    compilation_limit: u64,
    source_code_unit_limit: u64,
    compilations: u64,
    source_code_units: u64,
    native_root_completion: Option<StoredValue>,
}

impl ExecutionBudget {
    const fn new(limits: ExecutionLimits) -> Self {
        Self {
            interrupt_counter: InterruptCounter::new(),
            instruction_limit: limits.instruction_fuel,
            executed_instructions: 0,
            compilation_limit: limits.dynamic_compilations,
            source_code_unit_limit: limits.dynamic_source_code_units,
            compilations: 0,
            source_code_units: 0,
            native_root_completion: None,
        }
    }

    fn charge_instructions(&mut self, additional: u64) -> Result<(), ExecutionError> {
        if additional
            > self
                .instruction_limit
                .saturating_sub(self.executed_instructions)
        {
            self.executed_instructions = self.instruction_limit;
            return Err(ExecutionError::InstructionLimitExceeded {
                limit: self.instruction_limit,
                executed: self.executed_instructions,
            });
        }
        self.executed_instructions = self.executed_instructions.saturating_add(additional);
        Ok(())
    }

    const fn remaining_instructions(&self) -> u64 {
        self.instruction_limit
            .saturating_sub(self.executed_instructions)
    }

    fn charge_dynamic_compilation(
        &mut self,
        source: &OrdinaryDynamicFunctionSource,
    ) -> Result<(), ExecutionError> {
        let compilations = self.compilations.saturating_add(1);
        if compilations > self.compilation_limit {
            return Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::DynamicCompilations,
                limit: self.compilation_limit,
                observed: compilations,
            });
        }
        let source_code_units = self
            .source_code_units
            .saturating_add(dynamic_function_source_code_units(source));
        if source_code_units > self.source_code_unit_limit {
            return Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::DynamicSourceCodeUnits,
                limit: self.source_code_unit_limit,
                observed: source_code_units,
            });
        }
        self.compilations = compilations;
        self.source_code_units = source_code_units;
        Ok(())
    }
}

enum FrameBinding {
    Direct(SlotValue),
    Captured(BindingCellId),
}

enum OperandStackEntry {
    JavaScript(StoredValue),
    Catch { handler: InstructionIndex },
    ForOfCatch { active: bool },
    FinallyReturn { continuation: InstructionIndex },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConstructorState {
    NonConstructor,
    Ordinary,
    DerivedUninitialized,
    DerivedInitialized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConstructorProfile {
    OrdinaryOrNone,
    Derived,
    DerivedDefault,
}

impl ConstructorProfile {
    const fn is_derived(self) -> bool {
        matches!(self, Self::Derived | Self::DerivedDefault)
    }

    const fn is_derived_default(self) -> bool {
        matches!(self, Self::DerivedDefault)
    }
}

pub(crate) struct Frame {
    function: FunctionId,
    code: InstalledCodeId,
    template: FunctionTemplateId,
    strict: bool,
    receiver: StoredValue,
    new_target: Option<FunctionId>,
    instruction: InstructionIndex,
    return_to: Option<CallReturn>,
    dynamic_return: Option<DynamicFunctionReturn>,
    native_returns: Vec<NativeContinuation>,
    transient_cleanup_pending: bool,
    constructor_state: ConstructorState,
    native_caller: Option<SyntheticNativeFrame>,
    generator_resume: Option<ObjectId>,
    generator_result: Option<ObjectId>,
    resume_abrupt: Option<PendingException>,
    reserved_values: u64,
    arguments_snapshot_use: ArgumentsSnapshotUse,
    arguments_snapshot: Option<Vec<StoredValue>>,
    constructor_arguments: Option<Vec<StoredValue>>,
    arguments: Vec<FrameBinding>,
    locals: Vec<FrameBinding>,
    own_cells: Vec<Option<BindingCellId>>,
    own_cell_bindings: Vec<FrameBindingAddress>,
    environment: Vec<EnvironmentBinding>,
    stack: Vec<OperandStackEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeneratorLifecycle {
    SuspendedStart,
    SuspendedYield,
    SuspendedYieldStar,
    Executing,
    Completed,
}

pub(crate) struct GeneratorRecord {
    pub(crate) state: GeneratorLifecycle,
    pub(crate) frame: Option<Frame>,
}

pub(crate) struct AsyncFunctionRecord {
    pub(crate) frame: Frame,
    pub(crate) awaiting: ObjectId,
    pub(crate) origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AsyncGeneratorLifecycle {
    SuspendedStart,
    SuspendedYield,
    SuspendedYieldStar,
    Executing,
    DrainingQueue,
    Completed,
}

pub(crate) struct AsyncGeneratorRequest {
    pub(crate) mode: GeneratorResumeMode,
    pub(crate) value: StoredValue,
    pub(crate) capability: crate::object::PromiseCapability,
    pub(crate) realm: RealmId,
    pub(crate) origin: JsStackFrame,
}

pub(crate) struct AsyncGeneratorAwait {
    pub(crate) promise: ObjectId,
    pub(crate) origin: JsStackFrame,
    pub(crate) kind: AsyncGeneratorAwaitKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AsyncGeneratorAwaitKind {
    Body,
    ReturnResume,
    ReturnResumeYieldStar,
    ReturnComplete,
}

pub(crate) struct AsyncGeneratorRecord {
    pub(crate) state: AsyncGeneratorLifecycle,
    pub(crate) frame: Option<Frame>,
    pub(crate) queue: VecDeque<AsyncGeneratorRequest>,
    pub(crate) awaiting: Option<AsyncGeneratorAwait>,
}

/// The pinned `call (native)` / `apply (native)` entry `QuickJS` places
/// between the target function and its caller when a bytecode function is
/// reached through `Function.prototype.call` or `Function.prototype.apply`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SyntheticNativeFrame {
    Call,
    Apply,
}

impl SyntheticNativeFrame {
    const fn label(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Apply => "apply",
        }
    }
}

struct DynamicFunctionReturn {
    root: InstalledRoot,
    realm: RealmId,
    family: DynamicFunctionFamily,
    construction: Option<FunctionId>,
    origin: Option<JsStackFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyGetStage {
    TrapLookup,
    TrapCall,
}

struct ProxyGetContinuation {
    proxy: HeapReference,
    target: HeapReference,
    handler: HeapReference,
    key: PropertyKey,
    receiver: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ProxyGetStage,
}

impl ProxyGetContinuation {
    #[allow(
        clippy::unused_self,
        reason = "all Proxy continuations expose one uniform retained-value accounting method"
    )]
    const fn retained_values(&self) -> u64 {
        4
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyCallStage {
    TrapLookup,
    TrapCall,
}

struct ProxyCallContinuation {
    proxy: FunctionId,
    target: FunctionId,
    handler: HeapReference,
    receiver: StoredValue,
    arguments: CallArguments,
    new_target: Option<FunctionId>,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ProxyCallStage,
}

impl ProxyCallContinuation {
    fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.remaining().len())
            .saturating_add(3)
            .saturating_add(u64::from(self.new_target.is_some()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyBooleanKind {
    Has,
    Delete,
    Set,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyBooleanStage {
    TrapLookup,
    TrapCall,
    TargetDescriptor,
    TargetExtensible,
}

enum ProxyBooleanCompletion {
    Boolean,
    Write { strict: bool },
    Delete { strict: bool },
}

struct ProxyBooleanContinuation {
    proxy: HeapReference,
    target: HeapReference,
    handler: HeapReference,
    key: PropertyKey,
    value: Option<StoredValue>,
    receiver: Option<StoredValue>,
    realm: RealmId,
    origin: JsStackFrame,
    kind: ProxyBooleanKind,
    stage: ProxyBooleanStage,
    completion: ProxyBooleanCompletion,
    trap_result: Option<bool>,
}

impl ProxyBooleanContinuation {
    fn retained_values(&self) -> u64 {
        4_u64
            .saturating_add(u64::from(self.value.is_some()))
            .saturating_add(u64::from(self.receiver.is_some()))
            .saturating_add(u64::from(self.trap_result.is_some()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinarySetReceiverStage {
    Descriptor,
    Define,
}

struct OrdinarySetReceiverContinuation {
    target: HeapReference,
    receiver: HeapReference,
    key: PropertyKey,
    name: JsString,
    value: StoredValue,
    strict: bool,
    boolean_result: bool,
    realm: RealmId,
    origin: JsStackFrame,
    stage: OrdinarySetReceiverStage,
}

impl OrdinarySetReceiverContinuation {
    const fn retained_values() -> u64 {
        3
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyMetaKind {
    GetPrototypeOf,
    SetPrototypeOf,
    IsExtensible,
    PreventExtensions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyMetaStage {
    TrapLookup,
    TrapCall,
    ExtensibleCheck,
    TargetPrototypeCheck,
}

#[allow(
    clippy::option_option,
    reason = "the outer option selects [[SetPrototypeOf]] and the inner option represents null"
)]
struct ProxyMetaContinuation {
    proxy: HeapReference,
    target: HeapReference,
    handler: HeapReference,
    requested_prototype: Option<Option<HeapReference>>,
    trap_result: Option<StoredValue>,
    realm: RealmId,
    origin: JsStackFrame,
    kind: ProxyMetaKind,
    stage: ProxyMetaStage,
}

impl ProxyMetaContinuation {
    fn retained_values(&self) -> u64 {
        4_u64.saturating_add(u64::from(self.trap_result.is_some()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyDescriptorStage {
    TrapLookup,
    TrapCall,
    DescriptorRead,
    TargetDescriptor,
    TargetDescriptorRead,
    ExtensibleCheck,
}

#[allow(
    clippy::option_option,
    reason = "validation distinguishes not-yet-read, absent, and present descriptors"
)]
struct ProxyDescriptorContinuation {
    proxy: HeapReference,
    target: HeapReference,
    handler: HeapReference,
    key: PropertyKey,
    reader: Option<DescriptorReadState>,
    trap_descriptor: Option<Option<OwnProperty>>,
    target_descriptor: Option<Option<OwnProperty>>,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ProxyDescriptorStage,
}

impl ProxyDescriptorContinuation {
    fn retained_values(&self) -> u64 {
        4_u64
            .saturating_add(
                self.reader
                    .as_ref()
                    .map_or(0, DescriptorReadState::retained_values),
            )
            .saturating_add(u64::from(self.trap_descriptor.is_some()))
            .saturating_add(u64::from(self.target_descriptor.is_some()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyDefineStage {
    TrapLookup,
    TargetDefine,
    TrapCall,
    TargetDescriptor,
    TargetDescriptorRead,
    ExtensibleCheck,
}

#[allow(
    clippy::option_option,
    reason = "validation distinguishes not-yet-read, absent, and present descriptors"
)]
struct ProxyDefineContinuation {
    proxy: HeapReference,
    target: HeapReference,
    handler: HeapReference,
    key: PropertyKey,
    definition: PropertyDefinition,
    descriptor: ObjectId,
    target_descriptor: Option<Option<OwnProperty>>,
    reader: Option<DescriptorReadState>,
    realm: RealmId,
    origin: JsStackFrame,
    result: DefinePropertyResult,
    stage: ProxyDefineStage,
}

impl ProxyDefineContinuation {
    fn retained_values(&self) -> u64 {
        5_u64
            .saturating_add(u64::from(self.target_descriptor.is_some()))
            .saturating_add(
                self.reader
                    .as_ref()
                    .map_or(0, DescriptorReadState::retained_values),
            )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyOwnKeysStage {
    TrapLookup,
    TrapCall,
    ResultLength,
    ResultIndex,
    TargetKeys,
    TargetExtensible,
    TargetDescriptor,
}

struct ProxyOwnKeysContinuation {
    proxy: HeapReference,
    target: HeapReference,
    handler: HeapReference,
    result: Option<HeapReference>,
    length: usize,
    next_index: usize,
    trap_keys: Vec<PropertyKey>,
    target_keys: Vec<PropertyKey>,
    next_target_key: usize,
    non_configurable_keys: Vec<PropertyKey>,
    target_extensible: Option<bool>,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ProxyOwnKeysStage,
}

#[derive(Clone, Copy)]
enum ObjectMetaFailure {
    NonExtensible,
    ProxyTrap,
}

struct ObjectMetaContinuation {
    completion: StoredValue,
    failure: ObjectMetaFailure,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum OwnDescriptorQuery {
    Present,
    Enumerable,
}

struct OwnDescriptorQueryContinuation {
    query: OwnDescriptorQuery,
}

impl ObjectMetaContinuation {
    const fn retained_values() -> u64 {
        1
    }
}

impl ProxyOwnKeysContinuation {
    fn retained_values(&self) -> u64 {
        4_u64
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(usize_to_u64(self.trap_keys.len()))
            .saturating_add(usize_to_u64(self.target_keys.len()))
            .saturating_add(usize_to_u64(self.non_configurable_keys.len()))
    }
}

enum NativeContinuation {
    FunctionSource(FunctionSourceContinuation),
    FunctionApply(FunctionApplyContinuation),
    FunctionBind(FunctionBindContinuation),
    PropertyKey(PropertyKeyContinuation),
    OperatorPrimitive(OperatorPrimitiveContinuation),
    ArrayBufferConstructor(Box<ArrayBufferConstructorContinuation>),
    ArrayBufferSlice(Box<ArrayBufferSliceContinuation>),
    DataViewConstructor(Box<DataViewConstructorState>),
    TypedArrayConstructorObject(Box<TypedArrayConstructorObjectState>),
    TypedArrayConstructorSequence(Box<TypedArrayConstructorSequenceState>),
    TypedArrayPrototypeSet(Box<TypedArrayPrototypeSetState>),
    TypedArrayPrototypeSubarray(Box<TypedArrayPrototypeSubarrayState>),
    TypedArrayPrototypeSlice(Box<TypedArrayPrototypeSliceState>),
    DateToJson(DateToJsonContinuation),
    TemporalDurationBag(Box<TemporalDurationBagContinuation>),
    TemporalDurationCompareOptions(TemporalDurationCompareOptionsContinuation),
    TemporalDurationRoundOptions(Box<TemporalDurationRoundContinuation>),
    TemporalDurationTotalOptions(Box<TemporalDurationTotalContinuation>),
    TemporalDurationToStringOptions(Box<TemporalDurationToStringContinuation>),
    TemporalInstantRoundOptions(Box<TemporalInstantRoundContinuation>),
    TemporalInstantDifferenceOptions(Box<TemporalInstantDifferenceContinuation>),
    TemporalInstantToStringOptions(Box<TemporalInstantToStringContinuation>),
    IntrinsicGet(IntrinsicGetContinuation),
    AggregateError(AggregateErrorContinuation),
    FromEntries(Box<FromEntriesContinuation>),
    GroupBy(Box<GroupByContinuation>),
    MapConstructor(Box<MapConstructorContinuation>),
    MapForEach(Box<MapForEachContinuation>),
    MapComputed(Box<MapComputedContinuation>),
    SetConstructor(Box<SetConstructorContinuation>),
    SetForEach(Box<SetForEachContinuation>),
    SetOperation(Box<SetOperationContinuation>),
    MathSumPrecise(Box<MathSumPreciseContinuation>),
    JsonParse(Box<JsonParseContinuation>),
    JsonStringify(Box<JsonStringifyContinuation>),
    ErrorConstructor(ErrorConstructorContinuation),
    ErrorToString(ErrorToStringContinuation),
    ArrayIteratorNext(ArrayIteratorNextContinuation),
    ForOfStart(ForOfStartContinuation),
    ForOfNext(ForOfNextContinuation),
    ForOfClose(ForOfCloseContinuation),
    AsyncFromSync(AsyncFromSyncContinuation),
    AsyncFromSyncClose(AsyncFromSyncCloseContinuation),
    YieldStarIteratorCall(YieldStarIteratorCallContinuation),
    IteratorAppend(IteratorAppendContinuation),
    IteratorClose(IteratorCloseContinuation),
    ForIn(Box<ForInContinuation>),
    CopyDataProperties(CopyDataPropertiesContinuation),
    EnumerableOwnProperties(Box<EnumerableOwnPropertiesContinuation>),
    ObjectKeyListing(Box<ObjectKeyListingContinuation>),
    ProxyEnumerable(Box<ProxyEnumerableContinuation>),
    ObjectAssign(Box<ObjectAssignContinuation>),
    GetOwnPropertyDescriptors(Box<GetOwnPropertyDescriptorsContinuation>),
    ArrayJoin(Box<ArrayJoinContinuation>),
    ArraySearch(Box<ArraySearchContinuation>),
    ArrayMutator(Box<ArrayMutatorContinuation>),
    ArrayCopier(Box<ArrayCopierContinuation>),
    ArrayCallback(Box<ArrayCallbackContinuation>),
    ArrayReduction(Box<ArrayReductionContinuation>),
    ArraySplice(Box<ArraySpliceContinuation>),
    ArraySort(Box<ArraySortContinuation>),
    ArrayFlatten(Box<ArrayFlattenContinuation>),
    ArrayStatic(Box<ArrayStaticContinuation>),
    ArrayFromAsync(Box<ArrayFromAsyncRecord>),
    StringRaw(Box<StringRawContinuation>),
    StringReplace(Box<StringReplaceContinuation>),
    StringSplit(Box<StringSplitContinuation>),
    RegExp(Box<RegExpContinuation>),
    LocaleString(Box<LocaleStringContinuation>),
    DefineProperty(Box<DefinePropertyContinuation>),
    DefineProperties(Box<DefinePropertiesContinuation>),
    InstanceOf(InstanceOfContinuation),
    Promise(PromiseContinuation),
    PromiseCombinator(Box<PromiseCombinatorContinuation>),
    ProxyGet(Box<ProxyGetContinuation>),
    ProxyCall(Box<ProxyCallContinuation>),
    ProxyBoolean(Box<ProxyBooleanContinuation>),
    OrdinarySetReceiver(Box<OrdinarySetReceiverContinuation>),
    ProxyMeta(Box<ProxyMetaContinuation>),
    ProxyDescriptor(Box<ProxyDescriptorContinuation>),
    ProxyDefine(Box<ProxyDefineContinuation>),
    ProxyOwnKeys(Box<ProxyOwnKeysContinuation>),
    IntegrityLevel(Box<IntegrityLevelContinuation>),
    IsPrototypeOf(Box<IsPrototypeOfContinuation>),
    ObjectMeta(ObjectMetaContinuation),
    OwnDescriptorQuery(OwnDescriptorQueryContinuation),
    AsyncAwait {
        origin: JsStackFrame,
    },
    AsyncGeneratorReturnAwait {
        generator: ObjectId,
        kind: AsyncGeneratorAwaitKind,
        origin: JsStackFrame,
        completion: StoredValue,
    },
    /// Ignore an accessor setter's return value and complete `Reflect.set`
    /// with the internal-method success Boolean.
    ReflectSet,
    /// Convert an ordinary `[[Set]]` Boolean into a language write completion.
    ProxyWrite,
    FunctionCall,
}

impl NativeContinuation {
    #[allow(
        clippy::too_many_lines,
        reason = "each continuation variant has a single explicit accounting contract"
    )]
    fn retained_values(&self) -> u64 {
        match self {
            Self::FunctionSource(state) => usize_to_u64(state.arguments.len())
                .saturating_add(u64::from(state.construction.is_some())),
            Self::FunctionApply(state) => state.retained_values(),
            Self::FunctionBind(state) => state.retained_values(),
            Self::PropertyKey(state) => state.retained_values(),
            Self::OperatorPrimitive(state) => state.retained_values(),
            Self::ArrayBufferConstructor(_) => {
                ArrayBufferConstructorContinuation::retained_values()
            }
            Self::ArrayBufferSlice(_) => ArrayBufferSliceContinuation::retained_values(),
            Self::DataViewConstructor(_) => DataViewConstructorState::retained_values(),
            Self::TypedArrayConstructorObject(_) => {
                TypedArrayConstructorObjectState::retained_values()
            }
            Self::TypedArrayConstructorSequence(state) => state.retained_values(),
            Self::TypedArrayPrototypeSet(_) => TypedArrayPrototypeSetState::retained_values(),
            Self::TypedArrayPrototypeSubarray(_) => {
                TypedArrayPrototypeSubarrayState::retained_values()
            }
            Self::TypedArrayPrototypeSlice(_) => TypedArrayPrototypeSliceState::retained_values(),
            Self::DateToJson(_) => DateToJsonContinuation::retained_values(),
            Self::TemporalDurationBag(state) => state.retained_values(),
            Self::TemporalDurationCompareOptions(_) => {
                TemporalDurationCompareOptionsContinuation::retained_values()
            }
            Self::TemporalDurationRoundOptions(_) => {
                TemporalDurationRoundContinuation::retained_values()
            }
            Self::TemporalDurationTotalOptions(_) => {
                TemporalDurationTotalContinuation::retained_values()
            }
            Self::TemporalDurationToStringOptions(_) => {
                TemporalDurationToStringContinuation::retained_values()
            }
            Self::TemporalInstantRoundOptions(_) => {
                TemporalInstantRoundContinuation::retained_values()
            }
            Self::TemporalInstantDifferenceOptions(_) => {
                TemporalInstantDifferenceContinuation::retained_values()
            }
            Self::TemporalInstantToStringOptions(_) => {
                TemporalInstantToStringContinuation::retained_values()
            }
            Self::IntrinsicGet(state) => state.retained_values(),
            Self::AggregateError(state) => state.retained_values(),
            Self::FromEntries(state) => state.retained_values(),
            Self::GroupBy(state) => state.retained_values(),
            Self::MapConstructor(state) => state.retained_values(),
            Self::MapForEach(_) => MapForEachContinuation::retained_values(),
            Self::MapComputed(_) => MapComputedContinuation::retained_values(),
            Self::SetConstructor(state) => state.retained_values(),
            Self::SetForEach(_) => SetForEachContinuation::retained_values(),
            Self::SetOperation(state) => state.retained_values(),
            Self::MathSumPrecise(state) => state.retained_values(),
            Self::JsonParse(state) => state.retained_values(),
            Self::JsonStringify(state) => state.retained_values(),
            Self::ErrorConstructor(state) => state.retained_values(),
            Self::ErrorToString(state) => state.retained_values(),
            Self::ArrayIteratorNext(state) => state.retained_values(),
            Self::ForOfStart(state) => state.retained_values(),
            Self::ForOfNext(state) => state.retained_values(),
            Self::ForOfClose(state) => state.retained_values(),
            Self::AsyncFromSync(state) => state.retained_values(),
            Self::AsyncFromSyncClose(state) => state.retained_values(),
            Self::YieldStarIteratorCall(state) => state.retained_values(),
            Self::IteratorAppend(state) => state.retained_values(),
            Self::IteratorClose(state) => state.retained_values(),
            Self::ForIn(state) => state.retained_values(),
            Self::CopyDataProperties(state) => state.retained_values(),
            Self::EnumerableOwnProperties(state) => state.retained_values(),
            Self::ObjectKeyListing(state) => state.retained_values(),
            Self::ProxyEnumerable(state) => state.retained_values(),
            Self::ObjectAssign(state) => state.retained_values(),
            Self::GetOwnPropertyDescriptors(state) => state.retained_values(),
            Self::ArrayJoin(_) => ArrayJoinContinuation::retained_values(),
            Self::ArraySearch(_) => ArraySearchContinuation::retained_values(),
            Self::ArrayMutator(state) => state.retained_values(),
            Self::ArrayCopier(state) => state.retained_values(),
            Self::ArrayCallback(_) => ArrayCallbackContinuation::retained_values(),
            Self::ArrayReduction(_) => ArrayReductionContinuation::retained_values(),
            Self::ArraySplice(state) => state.retained_values(),
            Self::ArraySort(state) => state.retained_values(),
            Self::ArrayFlatten(state) => state.retained_values(),
            Self::ArrayStatic(state) => state.retained_values(),
            Self::ArrayFromAsync(state) => state.retained_values(),
            Self::StringRaw(state) => state.retained_values(),
            Self::StringReplace(state) => state.retained_values(),
            Self::StringSplit(state) => state.retained_values(),
            Self::RegExp(state) => state.retained_values(),
            Self::LocaleString(state) => state.retained_values(),
            Self::DefineProperty(state) => state.retained_values(),
            Self::DefineProperties(state) => state.retained_values(),
            Self::InstanceOf(state) => state.retained_values(),
            Self::Promise(state) => state.retained_values(),
            Self::PromiseCombinator(state) => state.retained_values(),
            Self::ProxyGet(state) => state.retained_values(),
            Self::ProxyCall(state) => state.retained_values(),
            Self::ProxyBoolean(state) => state.retained_values(),
            Self::OrdinarySetReceiver(_) => OrdinarySetReceiverContinuation::retained_values(),
            Self::ProxyMeta(state) => state.retained_values(),
            Self::ProxyDescriptor(state) => state.retained_values(),
            Self::ProxyDefine(state) => state.retained_values(),
            Self::ProxyOwnKeys(state) => state.retained_values(),
            Self::IntegrityLevel(state) => state.retained_values(),
            Self::IsPrototypeOf(_) => IsPrototypeOfContinuation::retained_values(),
            Self::ObjectMeta(_) => ObjectMetaContinuation::retained_values(),
            Self::OwnDescriptorQuery(_)
            | Self::AsyncAwait { .. }
            | Self::ReflectSet
            | Self::ProxyWrite
            | Self::FunctionCall => 0,
            Self::AsyncGeneratorReturnAwait { .. } => 1,
        }
    }

    fn handles_abrupt(&self) -> bool {
        matches!(
            self,
            Self::AggregateError(_)
                | Self::FromEntries(_)
                | Self::GroupBy(_)
                | Self::MapConstructor(_)
                | Self::SetConstructor(_)
                | Self::ArrayStatic(_)
                | Self::ArrayFromAsync(_)
                | Self::PromiseCombinator(_)
                | Self::IteratorAppend(_)
                | Self::IteratorClose(_)
                | Self::AsyncFromSync(_)
                | Self::AsyncFromSyncClose(_)
                | Self::AsyncGeneratorReturnAwait { .. }
        ) || matches!(self, Self::Promise(state) if state.handles_abrupt())
            || matches!(self, Self::RegExp(state) if state.handles_abrupt())
            || matches!(
                self,
                Self::OperatorPrimitive(state)
                    if matches!(
                        &state.target,
                        OperatorPrimitiveTarget::ArrayFromAsyncLength { .. }
                    ) || matches!(
                        &state.target,
                        OperatorPrimitiveTarget::RegExpValue(state) if state.handles_abrupt()
                    )
            )
    }
}

enum PromiseCapabilityPurpose {
    Resolve {
        resolution: StoredValue,
    },
    Reject {
        reason: StoredValue,
    },
    Then {
        promise: ObjectId,
        on_fulfilled: Option<FunctionId>,
        on_rejected: Option<FunctionId>,
    },
    Try {
        callback: StoredValue,
        arguments: Vec<StoredValue>,
    },
    Combinator {
        constructor: FunctionId,
        kind: PromiseCombinatorKind,
        iterable: StoredValue,
    },
    WithResolvers,
}

impl PromiseCapabilityPurpose {
    fn retained_values(&self) -> u64 {
        match self {
            Self::Resolve { .. } | Self::Reject { .. } => 1,
            Self::Then {
                on_fulfilled,
                on_rejected,
                ..
            } => 1_u64
                .saturating_add(u64::from(on_fulfilled.is_some()))
                .saturating_add(u64::from(on_rejected.is_some())),
            Self::Try {
                callback,
                arguments,
            } => usize_to_u64(arguments.len())
                .saturating_add(u64::from(callback.heap_reference().is_some())),
            Self::Combinator { iterable, .. } => {
                1_u64.saturating_add(u64::from(iterable.heap_reference().is_some()))
            }
            Self::WithResolvers => 0,
        }
    }
}

struct PromiseThenState {
    promise: ObjectId,
    realm: RealmId,
    on_fulfilled: Option<FunctionId>,
    on_rejected: Option<FunctionId>,
    origin: JsStackFrame,
}

struct PromiseFinallyState {
    receiver: StoredValue,
    realm: RealmId,
    on_finally: StoredValue,
    origin: JsStackFrame,
}

struct PromiseFinallyThenState {
    receiver: StoredValue,
    then_finally: StoredValue,
    catch_finally: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl PromiseThenState {
    fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(u64::from(self.on_fulfilled.is_some()))
            .saturating_add(u64::from(self.on_rejected.is_some()))
    }
}

enum PromiseContinuation {
    AsyncFunctionSettlement {
        capability: crate::object::PromiseCapability,
    },
    ConstructorExecutor {
        promise: ObjectId,
        reject: FunctionId,
    },
    ResolveThenGet {
        promise: ObjectId,
        realm: RealmId,
        resolution: StoredValue,
        completion: StoredValue,
    },
    ResolveConstructorGet {
        realm: RealmId,
        constructor: FunctionId,
        promise: ObjectId,
        origin: JsStackFrame,
    },
    NewCapabilityConstruct {
        capture: Rc<RefCell<PromiseCapabilityCapture>>,
        realm: RealmId,
        origin: JsStackFrame,
        purpose: PromiseCapabilityPurpose,
    },
    CapabilitySettlement {
        promise: StoredValue,
    },
    ThenConstructorGet(PromiseThenState),
    ThenSpeciesGet(PromiseThenState),
    FinallyConstructorGet(PromiseFinallyState),
    FinallySpeciesGet(PromiseFinallyState),
    FinallyThenGet(PromiseFinallyThenState),
    FinallyCallback {
        realm: RealmId,
        constructor: FunctionId,
        completion: StoredValue,
        kind: PromiseFinallyThunkKind,
        origin: JsStackFrame,
    },
    FinallyResolved {
        realm: RealmId,
        completion: StoredValue,
        kind: PromiseFinallyThunkKind,
        origin: JsStackFrame,
    },
    FinallyResolvedThenGet {
        realm: RealmId,
        promise: StoredValue,
        thunk: FunctionId,
        origin: JsStackFrame,
    },
    CatchThenGet {
        realm: RealmId,
        receiver: StoredValue,
        on_rejected: StoredValue,
        origin: JsStackFrame,
    },
    ReactionHandler {
        capability: crate::object::PromiseCapability,
    },
    ThenableCall {
        promise: ObjectId,
        reject: FunctionId,
    },
    TryCallback {
        capability: crate::object::PromiseCapability,
        origin: JsStackFrame,
    },
}

impl PromiseContinuation {
    fn retained_values(&self) -> u64 {
        match self {
            Self::ConstructorExecutor { .. }
            | Self::ThenableCall { .. }
            | Self::ResolveConstructorGet { .. }
            | Self::CatchThenGet { .. }
            | Self::FinallyConstructorGet(_)
            | Self::FinallySpeciesGet(_)
            | Self::FinallyCallback { .. }
            | Self::FinallyResolvedThenGet { .. } => 2,
            Self::AsyncFunctionSettlement { .. }
            | Self::ResolveThenGet { .. }
            | Self::ReactionHandler { .. }
            | Self::TryCallback { .. }
            | Self::FinallyThenGet(_) => 3,
            Self::NewCapabilityConstruct {
                capture, purpose, ..
            } => {
                let capture = capture.borrow();
                purpose
                    .retained_values()
                    .saturating_add(u64::from(capture.resolve.is_some()))
                    .saturating_add(u64::from(capture.reject.is_some()))
            }
            Self::CapabilitySettlement { .. } | Self::FinallyResolved { .. } => 1,
            Self::ThenConstructorGet(state) | Self::ThenSpeciesGet(state) => {
                state.retained_values()
            }
        }
    }

    const fn handles_abrupt(&self) -> bool {
        !matches!(
            self,
            Self::CatchThenGet { .. }
                | Self::ResolveConstructorGet { .. }
                | Self::NewCapabilityConstruct { .. }
                | Self::CapabilitySettlement { .. }
                | Self::ThenConstructorGet(_)
                | Self::ThenSpeciesGet(_)
                | Self::FinallyConstructorGet(_)
                | Self::FinallySpeciesGet(_)
                | Self::FinallyThenGet(_)
                | Self::FinallyCallback { .. }
                | Self::FinallyResolved { .. }
                | Self::FinallyResolvedThenGet { .. }
        )
    }
}

enum IntrinsicGetContinuation {
    BooleanConstructor {
        new_target: FunctionId,
        value: bool,
    },
    NumberConstructor {
        new_target: FunctionId,
        value: JsNumber,
    },
    DateConstructor {
        new_target: FunctionId,
        value: JsNumber,
    },
    ArrayBufferConstructor {
        new_target: FunctionId,
        byte_length: usize,
        max_byte_length: Option<usize>,
    },
    TypedArrayConstructor {
        new_target: FunctionId,
        element: TypedArrayElementType,
        length: usize,
    },
    TemporalInstantConstructor {
        new_target: FunctionId,
        epoch_nanoseconds: i128,
    },
    TemporalDurationConstructor {
        new_target: FunctionId,
        duration: temporal_rs::Duration,
    },
    StringConstructor {
        new_target: FunctionId,
        value: JsString,
    },
    ArrayConstructor {
        realm: RealmId,
        new_target: FunctionId,
        arguments: Vec<StoredValue>,
        origin: JsStackFrame,
    },
    MapConstructor {
        kind: MapCollectionKind,
        realm: RealmId,
        new_target: FunctionId,
        iterable: StoredValue,
        origin: JsStackFrame,
    },
    SetConstructor {
        kind: SetCollectionKind,
        realm: RealmId,
        new_target: FunctionId,
        iterable: StoredValue,
        origin: JsStackFrame,
    },
    PromiseConstructor {
        realm: RealmId,
        new_target: FunctionId,
        executor: FunctionId,
        origin: JsStackFrame,
    },
    WeakRefConstructor {
        new_target: FunctionId,
        target: StoredValue,
    },
    FinalizationRegistryConstructor {
        realm: RealmId,
        new_target: FunctionId,
        cleanup_callback: FunctionId,
    },
    ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag,
        temporary_receiver: Option<ObjectId>,
    },
}

impl IntrinsicGetContinuation {
    fn retained_values(&self) -> u64 {
        match self {
            Self::BooleanConstructor { .. }
            | Self::NumberConstructor { .. }
            | Self::DateConstructor { .. }
            | Self::ArrayBufferConstructor { .. }
            | Self::TypedArrayConstructor { .. }
            | Self::TemporalInstantConstructor { .. }
            | Self::TemporalDurationConstructor { .. }
            | Self::StringConstructor { .. } => 1,
            Self::ArrayConstructor { arguments, .. } => {
                1_u64.saturating_add(usize_to_u64(arguments.len()))
            }
            Self::MapConstructor { .. }
            | Self::SetConstructor { .. }
            | Self::PromiseConstructor { .. }
            | Self::WeakRefConstructor { .. }
            | Self::FinalizationRegistryConstructor { .. } => 2,
            Self::ObjectPrototypeToString {
                temporary_receiver, ..
            } => u64::from(temporary_receiver.is_some()),
        }
    }
}

#[derive(Clone, Copy)]
enum ObjectPrototypeTag {
    ArrayBuffer,
    Arguments,
    Array,
    BigInt,
    Boolean,
    Date,
    Error,
    Function,
    Number,
    Object,
    Promise,
    String,
    Symbol,
}

impl ObjectPrototypeTag {
    const fn name(self) -> &'static str {
        match self {
            Self::ArrayBuffer => "ArrayBuffer",
            Self::Arguments => "Arguments",
            Self::Array => "Array",
            Self::BigInt => "BigInt",
            Self::Boolean => "Boolean",
            Self::Date => "Date",
            Self::Error => "Error",
            Self::Function => "Function",
            Self::Number => "Number",
            Self::Object => "Object",
            Self::Promise => "Promise",
            Self::String => "String",
            Self::Symbol => "Symbol",
        }
    }
}

#[derive(Clone, Copy)]
enum ArrayIteratorNextStage {
    AwaitLength,
    AwaitValue,
}

struct ArrayIteratorNextContinuation {
    iterator: ObjectId,
    iterated: StoredValue,
    kind: crate::object::ArrayIteratorKind,
    index: u32,
    realm: RealmId,
    stage: ArrayIteratorNextStage,
    prepared_result: Option<PreparedIteratorResultPlan>,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum ForOfStartStage {
    IteratorMethod,
    AsyncIteratorMethod,
    Iterator,
    NextMethod,
}

struct ForOfStartContinuation {
    iterable: StoredValue,
    iterator: Option<StoredValue>,
    async_from_sync: bool,
    realm: RealmId,
    stage: ForOfStartStage,
    origin: JsStackFrame,
}

impl ForOfStartContinuation {
    fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.iterator.is_some()))
    }
}

#[derive(Clone, Copy)]
enum ForOfNextStage {
    Result,
    Done,
    Value,
}

struct ForOfNextContinuation {
    iterator: StoredValue,
    next: StoredValue,
    result: Option<StoredValue>,
    realm: RealmId,
    stage: ForOfNextStage,
    offset: u8,
    origin: JsStackFrame,
}

impl ForOfNextContinuation {
    fn retained_values(&self) -> u64 {
        2_u64.saturating_add(u64::from(self.result.is_some()))
    }
}

#[derive(Clone, Copy)]
enum ForOfCloseStage {
    AwaitReturnProperty,
    AwaitReturnCall,
}

struct ForOfCloseContinuation {
    iterator: StoredValue,
    realm: RealmId,
    stage: ForOfCloseStage,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum YieldStarIteratorCallMode {
    Return,
    Throw,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncFromSyncMode {
    Next,
    Return,
    Throw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncFromSyncStage {
    Method,
    Call,
    Done,
    Value,
    PromiseResolve,
    MissingThrowReturnMethod,
    MissingThrowReturnCall,
}

struct AsyncFromSyncContinuation {
    wrapper: ObjectId,
    iterator: StoredValue,
    next: StoredValue,
    input: Option<StoredValue>,
    result: Option<StoredValue>,
    capability: crate::object::PromiseCapability,
    realm: RealmId,
    mode: AsyncFromSyncMode,
    stage: AsyncFromSyncStage,
    done: bool,
    origin: JsStackFrame,
}

impl AsyncFromSyncContinuation {
    fn retained_values(&self) -> u64 {
        6_u64
            .saturating_add(u64::from(self.input.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncFromSyncCloseStage {
    ReturnMethod,
    ReturnCall,
}

struct AsyncFromSyncCloseContinuation {
    iterator: StoredValue,
    reason: StoredValue,
    target: AsyncFromSyncCloseTarget,
    realm: RealmId,
    stage: AsyncFromSyncCloseStage,
    origin: JsStackFrame,
}

impl AsyncFromSyncCloseContinuation {
    fn retained_values(&self) -> u64 {
        2_u64.saturating_add(match &self.target {
            AsyncFromSyncCloseTarget::RejectedPromise => 0,
            AsyncFromSyncCloseTarget::Capability(_) => 3,
        })
    }
}

enum AsyncFromSyncCloseTarget {
    RejectedPromise,
    Capability(crate::object::PromiseCapability),
}

#[derive(Clone, Copy)]
enum YieldStarIteratorCallStage {
    Method,
    Call,
}

struct YieldStarIteratorCallContinuation {
    iterator: StoredValue,
    input: Option<StoredValue>,
    realm: RealmId,
    mode: YieldStarIteratorCallMode,
    stage: YieldStarIteratorCallStage,
    origin: JsStackFrame,
}

impl YieldStarIteratorCallContinuation {
    fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.input.is_some()))
    }
}

impl ForOfCloseContinuation {
    #[allow(
        clippy::unused_self,
        reason = "ordinary close always retains exactly its iterator receiver"
    )]
    const fn retained_values(&self) -> u64 {
        1
    }
}

impl ArrayIteratorNextContinuation {
    fn retained_values(&self) -> u64 {
        2_u64.saturating_add(
            self.prepared_result
                .as_ref()
                .map_or(0, PreparedIteratorResultPlan::retained_values),
        )
    }
}

#[derive(Clone, Copy)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every resumable protocol boundary explicit"
)]
enum IteratorAppendStage {
    AwaitProbe,
    AwaitMethod,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitValue,
}

struct IteratorAppendContinuation {
    array: ObjectId,
    next_index: u32,
    iterable: StoredValue,
    iterator: Option<StoredValue>,
    next_acquired: bool,
    next_method: Option<FunctionId>,
    result: Option<StoredValue>,
    realm: RealmId,
    stage: IteratorAppendStage,
    origin: JsStackFrame,
}

impl IteratorAppendContinuation {
    fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next_method.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
    }
}

#[derive(Clone, Copy)]
enum CopyDataPropertiesStage {
    AwaitKeys,
    AwaitDescriptor,
    Next,
    ReadValue,
}

struct CopyDataPropertiesContinuation {
    target: StoredValue,
    source: StoredValue,
    excluded: Option<StoredValue>,
    snapshot: Vec<PropertyKey>,
    next: usize,
    current_key: Option<PropertyKey>,
    realm: RealmId,
    stage: CopyDataPropertiesStage,
    origin: JsStackFrame,
}

impl CopyDataPropertiesContinuation {
    fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(usize_to_u64(self.snapshot.len()))
            .saturating_add(u64::from(self.current_key.is_some()))
    }
}

#[derive(Clone, Copy)]
enum IteratorCloseStage {
    AwaitReturnProperty,
    AwaitReturnCall,
}

struct IteratorCloseContinuation {
    iterator: StoredValue,
    original: PendingException,
    stage: IteratorCloseStage,
}

impl IteratorCloseContinuation {
    #[allow(
        clippy::unused_self,
        reason = "close always retains iterator and original completion"
    )]
    const fn retained_values(&self) -> u64 {
        2
    }
}

struct FunctionSourceContinuation {
    native: NativeFunction,
    arguments: Vec<StoredValue>,
    index: usize,
    stage: PrimitiveConversionStage,
    construction: Option<FunctionId>,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum PrimitiveConversionStage {
    Start,
    ToString,
    ValueOf,
    AwaitExoticProperty,
    AwaitToStringProperty,
    AwaitValueOfProperty,
    AwaitExotic,
    AwaitToString,
    AwaitValueOf,
}

#[derive(Clone, Copy)]
enum PrimitiveConversionProperty {
    Exotic,
    ToString,
    ValueOf,
}

enum PrimitiveConversionPropertyAction {
    Continue,
    Call {
        function: FunctionId,
        arguments: Vec<StoredValue>,
    },
}

struct PropertyKeyContinuation {
    receiver: StoredValue,
    realm: RealmId,
    stage: PrimitiveConversionStage,
    target: PropertyKeyTarget,
    origin: JsStackFrame,
}

impl PropertyKeyContinuation {
    fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.target.retained_values())
    }
}

enum PropertyKeyTarget {
    ToKey,
    Read {
        base: StoredValue,
        realm: RealmId,
    },
    Write {
        base: StoredValue,
        value: StoredValue,
        strict: bool,
        realm: RealmId,
    },
    DefineMethod {
        base: StoredValue,
        function: StoredValue,
        kind: DefineMethodKind,
        enumerable: bool,
        realm: RealmId,
    },
    /// `Object.defineProperty`'s key, awaiting `ToPropertyKey`.
    DefineProperty {
        target: StoredValue,
        descriptor: StoredValue,
        realm: RealmId,
    },
    /// `Object.getOwnPropertyDescriptor`'s key, awaiting `ToPropertyKey`.
    OwnPropertyDescriptor {
        target: StoredValue,
        realm: RealmId,
    },
    /// `Object.prototype.hasOwnProperty` or `Object.hasOwn` key, awaiting
    /// `ToPropertyKey` after each entry point's required target validation.
    HasOwnProperty {
        target: StoredValue,
        realm: RealmId,
    },
    /// `Object.prototype.propertyIsEnumerable`'s key, awaiting `ToPropertyKey`.
    PropertyIsEnumerable {
        target: StoredValue,
        realm: RealmId,
    },
    /// The `delete` operator's key, awaiting `ToPropertyKey`.
    Delete {
        base: StoredValue,
        strict: bool,
        realm: RealmId,
    },
    ReflectGet {
        target: StoredValue,
        receiver: StoredValue,
        realm: RealmId,
    },
    ReflectSet {
        target: StoredValue,
        receiver: StoredValue,
        value: StoredValue,
        realm: RealmId,
    },
    ReflectDefineProperty {
        target: StoredValue,
        descriptor: StoredValue,
        realm: RealmId,
    },
    ReflectOwnPropertyDescriptor {
        target: StoredValue,
        realm: RealmId,
    },
    ReflectHas {
        target: StoredValue,
        realm: RealmId,
    },
    In {
        target: StoredValue,
        realm: RealmId,
    },
}

impl PropertyKeyTarget {
    const fn retained_values(&self) -> u64 {
        match self {
            Self::ToKey => 0,
            Self::Read { .. }
            | Self::Delete { .. }
            | Self::OwnPropertyDescriptor { .. }
            | Self::HasOwnProperty { .. }
            | Self::PropertyIsEnumerable { .. }
            | Self::ReflectOwnPropertyDescriptor { .. }
            | Self::ReflectHas { .. }
            | Self::In { .. } => 1,
            Self::Write { .. }
            | Self::DefineMethod { .. }
            | Self::DefineProperty { .. }
            | Self::ReflectGet { .. }
            | Self::ReflectDefineProperty { .. } => 2,
            Self::ReflectSet { .. } => 3,
        }
    }
}

#[derive(Clone, Copy)]
enum OperatorPrimitiveHint {
    Default,
    Number,
    String,
}

impl OperatorPrimitiveHint {
    const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Number => "number",
            Self::String => "string",
        }
    }

    const fn first_ordinary_stage(self) -> OperatorPrimitiveStage {
        match self {
            Self::String => OperatorPrimitiveStage::ToString,
            Self::Default | Self::Number => OperatorPrimitiveStage::ValueOf,
        }
    }
}

#[derive(Clone, Copy)]
enum OperatorPrimitiveStage {
    Start,
    ValueOf,
    ToString,
    AwaitExoticProperty,
    AwaitValueOfProperty,
    AwaitToStringProperty,
    AwaitExotic,
    AwaitValueOf,
    AwaitToString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FunctionApplyStage {
    AwaitLength,
    AwaitIndex,
}

struct FunctionApplyContinuation {
    target: FunctionId,
    receiver: StoredValue,
    array_like: StoredValue,
    realm: RealmId,
    length: Option<u32>,
    next_index: u32,
    arguments: Vec<StoredValue>,
    stage: FunctionApplyStage,
    active_frame_values: u64,
    origin: JsStackFrame,
    new_target: Option<FunctionId>,
    native_caller: Option<SyntheticNativeFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix marks every observable Function bind suspension boundary"
)]
enum FunctionBindStage {
    AwaitLengthDescriptor,
    AwaitLengthValue,
    AwaitNameValue,
}

struct FunctionBindContinuation {
    target: FunctionId,
    bound_this: StoredValue,
    bound_arguments: Vec<StoredValue>,
    length: JsNumber,
    realm: RealmId,
    stage: FunctionBindStage,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstanceOfStage {
    MethodRead,
    MethodCall,
    PrototypeRead,
    PrototypeWalk,
}

struct InstanceOfContinuation {
    value: StoredValue,
    target: StoredValue,
    prototype: Option<HeapReference>,
    current: Option<HeapReference>,
    realm: RealmId,
    stage: InstanceOfStage,
    origin: JsStackFrame,
}

struct ArrayLengthWriteState {
    base: StoredValue,
    name: JsString,
    strict: bool,
    reflect: bool,
    definition: Option<ArrayLengthDefinition>,
    original: Option<StoredValue>,
    first_length: Option<u32>,
}

#[derive(Clone, Copy)]
struct ArrayLengthDefinition {
    writable: Option<bool>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
    result: DefinePropertyResult,
}

impl FunctionApplyContinuation {
    fn retained_values(&self) -> u64 {
        3_u64.saturating_add(usize_to_u64(self.arguments.len()))
    }
}

impl FunctionBindContinuation {
    fn retained_values(&self) -> u64 {
        2_u64.saturating_add(usize_to_u64(self.bound_arguments.len()))
    }
}

impl InstanceOfContinuation {
    fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.prototype.is_some()))
            .saturating_add(u64::from(self.current.is_some()))
    }
}

enum TemporalInstantLikeTarget {
    Allocate,
    CompareFirst {
        second: StoredValue,
    },
    CompareSecond {
        first_epoch_nanoseconds: i128,
    },
    Equals {
        receiver_epoch_nanoseconds: i128,
    },
    Difference {
        receiver: temporal_rs::Instant,
        options: StoredValue,
        since: bool,
    },
}

impl TemporalInstantLikeTarget {
    const fn retained_values(&self) -> u64 {
        match self {
            Self::CompareFirst { .. } | Self::Difference { .. } => 1,
            Self::Allocate | Self::CompareSecond { .. } | Self::Equals { .. } => 0,
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::CompareFirst { second } => trace_stored_value_root(second, mark),
            Self::Difference { options, .. } => trace_stored_value_root(options, mark),
            Self::Allocate | Self::CompareSecond { .. } | Self::Equals { .. } => {}
        }
    }
}

enum OperatorPrimitiveTarget {
    Unary {
        opcode: FinalOpcode,
    },
    BinaryRight {
        opcode: FinalOpcode,
        right: StoredValue,
        hint: OperatorPrimitiveHint,
    },
    BinaryFinish {
        opcode: FinalOpcode,
        left: StoredValue,
    },
    EqualityFinish {
        opcode: FinalOpcode,
        other: StoredValue,
    },
    NumberIntrinsic {
        new_target: Option<FunctionId>,
    },
    /// A one-argument Date construction, after `ToPrimitive(default)`.
    DateConstructor {
        new_target: FunctionId,
    },
    /// `%ArrayBuffer%`'s initial `byteLength`, awaiting `ToIndex`.
    ArrayBufferConstructorLength(Box<ArrayBufferConstructorLengthContinuation>),
    /// `%ArrayBuffer%`'s optional `maxByteLength`, awaiting `ToIndex`.
    ArrayBufferConstructorMax {
        new_target: FunctionId,
        byte_length: usize,
    },
    /// `%DataView%`'s `byteOffset`, after the backing-buffer brand check.
    DataViewConstructorOffset(Box<DataViewConstructorOffsetState>),
    /// `%DataView%`'s optional explicit `byteLength`, after `ToIndex`.
    DataViewConstructorByteLength(Box<DataViewConstructorByteLengthState>),
    /// A concrete typed-array constructor's initial length after `ToIndex`.
    TypedArrayConstructorLength(Box<TypedArrayConstructorLengthState>),
    /// A concrete typed-array `ArrayBuffer` view's byte offset after `ToIndex`.
    TypedArrayConstructorBufferOffset(Box<TypedArrayConstructorBufferOffsetState>),
    /// A concrete typed-array `ArrayBuffer` view's explicit length after `ToIndex`.
    TypedArrayConstructorBufferLength(Box<TypedArrayConstructorBufferLengthState>),
    /// An array-like typed-array initializer's `length`, awaiting `ToLength`.
    TypedArrayConstructorArrayLikeLength(Box<TypedArrayConstructorSequenceState>),
    /// One array-like or collected iterable initializer element awaiting its
    /// destination typed-array numeric conversion.
    TypedArrayConstructorElement(Box<TypedArrayConstructorSequenceState>),
    /// `%TypedArray%.prototype.set`'s target offset after
    /// `ToIntegerOrInfinity`.
    TypedArrayPrototypeSetOffset(Box<TypedArrayPrototypeSetState>),
    /// A non-typed source's `length`, awaiting `ToLength`.
    TypedArrayPrototypeSetSourceLength(Box<TypedArrayPrototypeSetState>),
    /// One array-like source element awaiting destination numeric conversion.
    TypedArrayPrototypeSetElement(Box<TypedArrayPrototypeSetState>),
    /// `%TypedArray%.prototype.subarray`'s initial relative index.
    TypedArrayPrototypeSubarrayBegin(Box<TypedArrayPrototypeSubarrayState>),
    /// `%TypedArray%.prototype.subarray`'s optional end index.
    TypedArrayPrototypeSubarrayEnd(Box<TypedArrayPrototypeSubarrayState>),
    /// `%TypedArray%.prototype.slice`'s initial relative index.
    TypedArrayPrototypeSliceStart(Box<TypedArrayPrototypeSliceState>),
    /// `%TypedArray%.prototype.slice`'s optional end index.
    TypedArrayPrototypeSliceEnd(Box<TypedArrayPrototypeSliceState>),
    /// `%TypedArray%.prototype.at`'s relative index.
    TypedArrayPrototypeAtIndex(Box<TypedArrayPrototypeAtState>),
    /// `%TypedArray%.prototype.includes`'s optional relative start index.
    TypedArrayPrototypeIncludesFromIndex(Box<TypedArrayPrototypeIncludesState>),
    /// `%TypedArray%.prototype.indexOf`'s optional relative start index.
    TypedArrayPrototypeIndexOfFromIndex(Box<TypedArrayPrototypeIndexOfState>),
    /// `%TypedArray%.prototype.lastIndexOf`'s optional relative start index.
    TypedArrayPrototypeLastIndexOfFromIndex(Box<TypedArrayPrototypeLastIndexOfState>),
    /// `%TypedArray%.prototype.fill` across its value and range conversions.
    TypedArrayPrototypeFill(Box<TypedArrayPrototypeFillState>),
    /// `%TypedArray%.prototype.copyWithin` across its relative index conversions.
    TypedArrayPrototypeCopyWithin(Box<TypedArrayPrototypeCopyWithinState>),
    /// `DataView.prototype.get*` after `ToIndex(byteOffset)`.
    DataViewGetIndex(Box<DataViewGetState>),
    /// `DataView.prototype.set*` after `ToIndex(byteOffset)`.
    DataViewSetOffset(Box<DataViewSetOffsetState>),
    /// `DataView.prototype.set*` after `ToNumber` or `ToBigInt(value)`.
    DataViewSetValue(Box<DataViewSetValueState>),
    /// An integer-indexed typed-array write after `ToNumber` or `ToBigInt`.
    TypedArrayElementSet(Box<TypedArrayElementSetState>),
    /// `ArrayBuffer.prototype.resize`'s new length, after the brand checks.
    ArrayBufferResize {
        object: ObjectId,
    },
    /// `ArrayBuffer.prototype.transfer` or `transferToFixedLength` after
    /// `ToIndex(newLength)`.
    ArrayBufferTransfer {
        object: ObjectId,
        preserve_resizability: bool,
    },
    /// `ArrayBuffer.prototype.slice`'s start argument, awaiting
    /// `ToClampedIndex`'s `ToNumber` component.
    ArrayBufferSliceStart(Box<ArrayBufferSliceContinuation>),
    /// `ArrayBuffer.prototype.slice`'s end argument, awaiting
    /// `ToClampedIndex`'s `ToNumber` component.
    ArrayBufferSliceEnd(Box<ArrayBufferSliceContinuation>),
    /// `Date.parse`, after `ToString`.
    DateParse,
    /// One supplied `Date.UTC` component, after `ToNumber`.
    DateUtc(Box<DateUtcContinuation>),
    /// One supplied multi-argument Date constructor component, after `ToNumber`.
    DateConstructorComponents(Box<DateConstructorContinuation>),
    /// `Date.prototype.setTime`, after its branded receiver check and `ToNumber`.
    DateSetTime {
        object: ObjectId,
    },
    /// One supplied local or UTC Date setter component, after `ToNumber`.
    DateSetter(Box<DateSetterContinuation>),
    /// The final value of `Date.prototype[@@toPrimitive]`.
    DateToPrimitive,
    /// `Date.prototype.toJSON`, after generic `ToPrimitive(number)`.
    DateToJson(Box<DateToJsonContinuation>),
    TemporalInstantNanoseconds {
        new_target: Option<FunctionId>,
    },
    TemporalInstantMilliseconds,
    TemporalInstantString(Box<TemporalInstantLikeTarget>),
    TemporalDurationConstructor(Box<TemporalDurationConstructorContinuation>),
    TemporalDurationBag(Box<TemporalDurationBagContinuation>),
    TemporalDurationRoundLargestUnit(Box<TemporalDurationRoundContinuation>),
    TemporalDurationRoundRoundingIncrement(Box<TemporalDurationRoundContinuation>),
    TemporalDurationRoundRoundingMode(Box<TemporalDurationRoundContinuation>),
    TemporalDurationRoundSmallestUnit(Box<TemporalDurationRoundContinuation>),
    TemporalDurationTotalUnit(Box<TemporalDurationTotalContinuation>),
    TemporalDurationToStringFractionalSecondDigits(Box<TemporalDurationToStringContinuation>),
    TemporalDurationToStringRoundingMode(Box<TemporalDurationToStringContinuation>),
    TemporalDurationToStringSmallestUnit(Box<TemporalDurationToStringContinuation>),
    TemporalInstantRoundRoundingIncrement(Box<TemporalInstantRoundContinuation>),
    TemporalInstantRoundRoundingMode(Box<TemporalInstantRoundContinuation>),
    TemporalInstantRoundSmallestUnit(Box<TemporalInstantRoundContinuation>),
    TemporalInstantDifferenceLargestUnit(Box<TemporalInstantDifferenceContinuation>),
    TemporalInstantDifferenceRoundingIncrement(Box<TemporalInstantDifferenceContinuation>),
    TemporalInstantDifferenceRoundingMode(Box<TemporalInstantDifferenceContinuation>),
    TemporalInstantDifferenceSmallestUnit(Box<TemporalInstantDifferenceContinuation>),
    TemporalInstantToStringFractionalSecondDigits(Box<TemporalInstantToStringContinuation>),
    TemporalInstantToStringRoundingMode(Box<TemporalInstantToStringContinuation>),
    TemporalInstantToStringSmallestUnit(Box<TemporalInstantToStringContinuation>),
    NumberToString {
        number: JsNumber,
    },
    /// A `Number.prototype` decimal rendering's digit count, awaiting
    /// `ToNumber`.
    NumberFormatDigits {
        number: JsNumber,
        format: NumberFormat,
    },
    /// One coercing global numeric function's first argument, awaiting the
    /// conversion selected by that function.
    GlobalNumeric(GlobalNumericFunction),
    /// One unary `%Math%` argument, awaiting `ToNumber`.
    MathUnary(MathMethod),
    /// A binary `%Math%` method's left argument, retaining the right argument.
    MathBinaryRight {
        method: MathMethod,
        right: StoredValue,
    },
    /// A binary `%Math%` method's right argument, retaining the converted left.
    MathBinaryFinish {
        method: MathMethod,
        left: JsNumber,
    },
    /// One variadic `%Math.min%` or `%Math.max%` conversion.
    MathExtrema(Box<MathExtremaContinuation>),
    /// One variadic `%Math.hypot%` conversion.
    MathHypot(Box<MathHypotContinuation>),
    /// One URI function's argument, awaiting `ToString`.
    GlobalUri(UriFunction),
    /// `parseInt`'s input, awaiting `ToString` while retaining its radix.
    GlobalParseIntString {
        radix: StoredValue,
    },
    /// `parseInt`'s radix, awaiting `ToNumber` after the input conversion.
    GlobalParseIntRadix {
        text: JsString,
    },
    StringIntrinsic {
        new_target: Option<FunctionId>,
    },
    SymbolIntrinsic {
        global_registry: bool,
    },
    StringIteratorIntrinsic,
    /// `JSON.parse`'s source text, awaiting `ToString`.
    JsonParseText(JsonParseTextContinuation),
    JsonParseArrayLength(Box<JsonParseContinuation>),
    /// `JSON.rawJSON`'s source text, awaiting `ToString`.
    JsonRawJsonText,
    JsonStringifyReplacerItem(Box<JsonStringifyContinuation>),
    JsonStringifyReplacerLength(Box<JsonStringifyContinuation>),
    JsonStringifySpaceNumber(Box<JsonStringifyContinuation>),
    JsonStringifySpaceString(Box<JsonStringifyContinuation>),
    JsonStringifyContainerLength(Box<JsonStringifyContinuation>),
    JsonStringifyBoxedNumber(Box<JsonStringifyContinuation>),
    JsonStringifyBoxedString(Box<JsonStringifyContinuation>),
    ErrorConstructorMessage(ErrorConstructorContinuation),
    ErrorToStringName(ErrorToStringContinuation),
    ErrorToStringMessage(ErrorToStringContinuation),
    ArrayIteratorLength(ArrayIteratorNextContinuation),
    FunctionApplyLength(FunctionApplyContinuation),
    ProxyOwnKeysLength(Box<ProxyOwnKeysContinuation>),
    /// `BigInt.prototype.toString`'s radix, awaiting `ToNumber`.
    BigIntToString {
        value: Arc<JsBigInt>,
    },
    /// `BigInt.asIntN`/`asUintN`'s bit count, awaiting `ToNumber`.
    BigIntTruncationBits {
        value: StoredValue,
        truncation: BigIntTruncation,
    },
    /// `BigInt.asIntN`/`asUintN`'s value, awaiting `ToPrimitive`.
    BigIntTruncationValue {
        bits: u64,
        truncation: BigIntTruncation,
    },
    ArrayJoinSeparator(Box<ArrayJoinContinuation>),
    ArrayJoinElement(Box<ArrayJoinContinuation>),
    /// An `Array.prototype` search's position argument, awaiting `ToNumber`.
    ArraySearchPosition(Box<ArraySearchContinuation>),
    /// An `Array.prototype` mutator's argument, awaiting `ToNumber`.
    ArrayMutatorArgument(Box<ArrayMutatorContinuation>),
    /// An `Array.prototype` copier's argument, awaiting `ToNumber`.
    ArrayCopierArgument(Box<ArrayCopierContinuation>),
    /// `Array.prototype.splice`'s argument, awaiting `ToNumber`.
    ArraySpliceArgument(Box<ArraySpliceContinuation>),
    /// A sorting method's length, comparator result, or default string value.
    ArraySortValue(Box<ArraySortContinuation>),
    /// A flattening method's depth or nested array length, awaiting `ToNumber`.
    ArrayFlattenValue(Box<ArrayFlattenContinuation>),
    /// An array-like `Array.from` length awaiting `ToNumber`.
    ArrayStaticLength(Box<ArrayStaticContinuation>),
    /// An array-like `Array.fromAsync` length awaiting `ToNumber`.
    ArrayFromAsyncLength {
        operation: ObjectId,
    },
    /// A Set-like record's `size`, awaiting the normative `ToNumber` step.
    SetRecordSize(Box<SetOperationContinuation>),
    /// One `String.raw` length, literal, or substitution conversion.
    StringRawValue(Box<StringRawContinuation>),
    /// One `String.prototype.replace` fallback operand or callback result,
    /// awaiting `ToString`.
    StringReplaceValue(Box<StringReplaceContinuation>),
    /// A `String.prototype.split` fallback operand awaiting primitive
    /// conversion to String or Number.
    StringSplitValue(Box<StringSplitContinuation>),
    /// A `RegExp` protocol operand awaiting primitive conversion.
    RegExpValue(Box<RegExpContinuation>),
    /// A locale-string length or invocation result awaiting primitive conversion.
    LocaleStringValue(Box<LocaleStringContinuation>),
    ArrayLengthWrite(ArrayLengthWriteState),
    /// A `String.prototype` method's receiver, awaiting `ToString`.
    StringMethodSubject(Box<StringMethodContinuation>),
    /// A `String.prototype` method's argument, awaiting its own coercion.
    StringMethodArgument(Box<StringMethodContinuation>),
}

impl OperatorPrimitiveTarget {
    #[allow(
        clippy::too_many_lines,
        reason = "each resumable primitive target owns an explicit retained-value contract"
    )]
    fn retained_values(&self) -> u64 {
        match self {
            // A `BigInt` payload is not a frame value, so the BigInt targets
            // retain nothing beyond the operand the caller already counted.
            Self::Unary { .. }
            | Self::NumberIntrinsic { new_target: None }
            | Self::DateParse
            | Self::ArrayBufferResize { .. }
            | Self::DateSetTime { .. }
            | Self::DateToPrimitive
            | Self::TemporalInstantNanoseconds { new_target: None }
            | Self::TemporalInstantMilliseconds
            | Self::StringIntrinsic { new_target: None }
            | Self::SymbolIntrinsic { .. }
            | Self::StringIteratorIntrinsic
            | Self::JsonRawJsonText
            | Self::GlobalNumeric(_)
            | Self::MathUnary(_)
            | Self::GlobalUri(_)
            | Self::BigIntToString { .. }
            | Self::BigIntTruncationBits { .. }
            | Self::BigIntTruncationValue { .. } => 0,
            Self::BinaryRight { .. }
            | Self::BinaryFinish { .. }
            | Self::EqualityFinish { .. }
            | Self::NumberToString { .. }
            | Self::NumberFormatDigits { .. }
            | Self::GlobalParseIntString { .. }
            | Self::GlobalParseIntRadix { .. }
            | Self::NumberIntrinsic {
                new_target: Some(_),
            }
            | Self::DateConstructor { .. }
            | Self::ArrayBufferConstructorMax { .. }
            | Self::ArrayBufferTransfer { .. }
            | Self::TemporalInstantNanoseconds {
                new_target: Some(_),
            }
            | Self::StringIntrinsic {
                new_target: Some(_),
            }
            | Self::MathBinaryRight { .. }
            | Self::MathBinaryFinish { .. }
            | Self::ArrayFromAsyncLength { .. } => 1,
            Self::DateUtc(state) => state.retained_values(),
            Self::DateConstructorComponents(state) => state.retained_values(),
            Self::ArrayBufferConstructorLength(_) => {
                ArrayBufferConstructorLengthContinuation::retained_values()
            }
            Self::ArrayBufferSliceStart(_) | Self::ArrayBufferSliceEnd(_) => {
                ArrayBufferSliceContinuation::retained_values()
            }
            Self::DataViewConstructorOffset(_) => DataViewConstructorOffsetState::retained_values(),
            Self::DataViewConstructorByteLength(_) => {
                DataViewConstructorByteLengthState::retained_values()
            }
            Self::TypedArrayConstructorLength(_) => {
                TypedArrayConstructorLengthState::retained_values()
            }
            Self::TypedArrayConstructorBufferOffset(_) => {
                TypedArrayConstructorBufferOffsetState::retained_values()
            }
            Self::TypedArrayConstructorBufferLength(_) => {
                TypedArrayConstructorBufferLengthState::retained_values()
            }
            Self::TypedArrayConstructorArrayLikeLength(state)
            | Self::TypedArrayConstructorElement(state) => state.retained_values(),
            Self::TypedArrayPrototypeSetOffset(_state)
            | Self::TypedArrayPrototypeSetSourceLength(_state)
            | Self::TypedArrayPrototypeSetElement(_state) => {
                TypedArrayPrototypeSetState::retained_values()
            }
            Self::TypedArrayPrototypeSubarrayBegin(_state)
            | Self::TypedArrayPrototypeSubarrayEnd(_state) => {
                TypedArrayPrototypeSubarrayState::retained_values()
            }
            Self::TypedArrayPrototypeSliceStart(_state)
            | Self::TypedArrayPrototypeSliceEnd(_state) => {
                TypedArrayPrototypeSliceState::retained_values()
            }
            Self::TypedArrayPrototypeAtIndex(_state) => {
                TypedArrayPrototypeAtState::retained_values()
            }
            Self::TypedArrayPrototypeIncludesFromIndex(_state) => {
                TypedArrayPrototypeIncludesState::retained_values()
            }
            Self::TypedArrayPrototypeIndexOfFromIndex(_state) => {
                TypedArrayPrototypeIndexOfState::retained_values()
            }
            Self::TypedArrayPrototypeLastIndexOfFromIndex(_state) => {
                TypedArrayPrototypeLastIndexOfState::retained_values()
            }
            Self::TypedArrayPrototypeFill(_state) => {
                TypedArrayPrototypeFillState::retained_values()
            }
            Self::TypedArrayPrototypeCopyWithin(_state) => {
                TypedArrayPrototypeCopyWithinState::retained_values()
            }
            Self::DataViewGetIndex(_) => DataViewGetState::retained_values(),
            Self::DataViewSetOffset(_) => DataViewSetOffsetState::retained_values(),
            Self::DataViewSetValue(_) => DataViewSetValueState::retained_values(),
            Self::TypedArrayElementSet(_) => TypedArrayElementSetState::retained_values(),
            Self::TemporalDurationConstructor(state) => state.retained_values(),
            Self::TemporalDurationBag(state) => state.retained_values(),
            Self::TemporalDurationRoundLargestUnit(_state)
            | Self::TemporalDurationRoundRoundingIncrement(_state)
            | Self::TemporalDurationRoundRoundingMode(_state)
            | Self::TemporalDurationRoundSmallestUnit(_state) => {
                TemporalDurationRoundContinuation::retained_values()
            }
            Self::TemporalDurationTotalUnit(_) => {
                TemporalDurationTotalContinuation::retained_values()
            }
            Self::TemporalDurationToStringFractionalSecondDigits(_state)
            | Self::TemporalDurationToStringRoundingMode(_state)
            | Self::TemporalDurationToStringSmallestUnit(_state) => {
                TemporalDurationToStringContinuation::retained_values()
            }
            Self::TemporalInstantRoundRoundingIncrement(_state)
            | Self::TemporalInstantRoundRoundingMode(_state)
            | Self::TemporalInstantRoundSmallestUnit(_state) => {
                TemporalInstantRoundContinuation::retained_values()
            }
            Self::TemporalInstantDifferenceLargestUnit(_state)
            | Self::TemporalInstantDifferenceRoundingIncrement(_state)
            | Self::TemporalInstantDifferenceRoundingMode(_state)
            | Self::TemporalInstantDifferenceSmallestUnit(_state) => {
                TemporalInstantDifferenceContinuation::retained_values()
            }
            Self::TemporalInstantToStringFractionalSecondDigits(_state)
            | Self::TemporalInstantToStringRoundingMode(_state)
            | Self::TemporalInstantToStringSmallestUnit(_state) => {
                TemporalInstantToStringContinuation::retained_values()
            }
            Self::DateSetter(state) => state.retained_values(),
            Self::DateToJson(_) => DateToJsonContinuation::retained_values(),
            Self::TemporalInstantString(state) => state.retained_values(),
            Self::SetRecordSize(state) => state.retained_values(),
            Self::ErrorConstructorMessage(state) => state.retained_values(),
            Self::JsonParseText(state) => state.retained_values(),
            Self::JsonParseArrayLength(state) => state.retained_values(),
            Self::JsonStringifyReplacerItem(state)
            | Self::JsonStringifyReplacerLength(state)
            | Self::JsonStringifySpaceNumber(state)
            | Self::JsonStringifySpaceString(state)
            | Self::JsonStringifyContainerLength(state)
            | Self::JsonStringifyBoxedNumber(state)
            | Self::JsonStringifyBoxedString(state) => state.retained_values(),
            Self::ErrorToStringName(state) | Self::ErrorToStringMessage(state) => {
                state.retained_values()
            }
            Self::ArrayIteratorLength(state) => state.retained_values(),
            Self::FunctionApplyLength(state) => state.retained_values(),
            Self::ProxyOwnKeysLength(state) => state.retained_values(),
            Self::ArrayJoinSeparator(_) | Self::ArrayJoinElement(_) => {
                ArrayJoinContinuation::retained_values()
            }
            Self::ArrayLengthWrite(state) => {
                1_u64.saturating_add(u64::from(state.original.is_some()))
            }
            Self::StringMethodSubject(state) | Self::StringMethodArgument(state) => {
                state.retained_values()
            }
            Self::ArraySearchPosition(_) => ArraySearchContinuation::retained_values(),
            Self::ArrayMutatorArgument(state) => state.retained_values(),
            Self::ArrayCopierArgument(state) => state.retained_values(),
            Self::ArraySpliceArgument(state) => state.retained_values(),
            Self::ArraySortValue(state) => state.retained_values(),
            Self::ArrayFlattenValue(state) => state.retained_values(),
            Self::ArrayStaticLength(state) => state.retained_values(),
            Self::StringRawValue(state) => state.retained_values(),
            Self::StringReplaceValue(state) => state.retained_values(),
            Self::StringSplitValue(state) => state.retained_values(),
            Self::RegExpValue(state) => state.retained_values(),
            Self::LocaleStringValue(state) => state.retained_values(),
            Self::MathExtrema(state) => state.retained_values(),
            Self::MathHypot(state) => state.retained_values(),
        }
    }
}

struct OperatorPrimitiveContinuation {
    receiver: StoredValue,
    realm: RealmId,
    hint: OperatorPrimitiveHint,
    stage: OperatorPrimitiveStage,
    target: OperatorPrimitiveTarget,
    origin: JsStackFrame,
}

impl OperatorPrimitiveContinuation {
    fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.target.retained_values())
    }
}

struct NativeCall {
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    continuations: Vec<NativeContinuation>,
    pre_call: Option<NativePreCall>,
    new_target: Option<FunctionId>,
    native_caller: Option<SyntheticNativeFrame>,
}

enum NativePreCall {
    AdvanceArrayIterator(ObjectId),
}

struct CallArguments {
    values: Vec<StoredValue>,
    next: usize,
}

impl CallArguments {
    const fn empty() -> Self {
        Self {
            values: Vec::new(),
            next: 0,
        }
    }

    const fn from_values(values: Vec<StoredValue>) -> Self {
        Self { values, next: 0 }
    }

    fn take_first(&mut self) -> Option<StoredValue> {
        let value = self.values.get_mut(self.next)?;
        self.next = self.next.saturating_add(1);
        Some(std::mem::replace(value, StoredValue::Undefined))
    }

    fn take_first_or_undefined(&mut self) -> StoredValue {
        self.take_first().unwrap_or(StoredValue::Undefined)
    }

    fn into_remaining_iter(self) -> impl Iterator<Item = StoredValue> {
        self.values.into_iter().skip(self.next)
    }

    fn into_remaining_values(mut self) -> Vec<StoredValue> {
        if self.next != 0 {
            self.values.drain(..self.next);
        }
        self.values
    }

    fn remaining(&self) -> &[StoredValue] {
        self.values.get(self.next..).unwrap_or_default()
    }
}

fn trace_stored_value_root(value: &StoredValue, mark: &mut dyn FnMut(CollectionRoot)) {
    if let Some(reference) = value.heap_reference() {
        mark(CollectionRoot::Heap(reference));
    }
}

fn trace_slot_value_root(value: &SlotValue, mark: &mut dyn FnMut(CollectionRoot)) {
    if let SlotValue::Value(value) = value {
        trace_stored_value_root(value, mark);
    }
}

fn trace_frame_binding_root(binding: &FrameBinding, mark: &mut dyn FnMut(CollectionRoot)) {
    match binding {
        FrameBinding::Direct(value) => trace_slot_value_root(value, mark),
        FrameBinding::Captured(cell) => mark(CollectionRoot::BindingCell(*cell)),
    }
}

fn trace_property_key_target_roots(
    target: &PropertyKeyTarget,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    match target {
        PropertyKeyTarget::ToKey => {}
        PropertyKeyTarget::Read { base, .. }
        | PropertyKeyTarget::Delete { base, .. }
        | PropertyKeyTarget::OwnPropertyDescriptor { target: base, .. }
        | PropertyKeyTarget::HasOwnProperty { target: base, .. }
        | PropertyKeyTarget::PropertyIsEnumerable { target: base, .. }
        | PropertyKeyTarget::ReflectOwnPropertyDescriptor { target: base, .. }
        | PropertyKeyTarget::ReflectHas { target: base, .. }
        | PropertyKeyTarget::In { target: base, .. } => {
            trace_stored_value_root(base, mark);
        }
        PropertyKeyTarget::DefineProperty {
            target, descriptor, ..
        }
        | PropertyKeyTarget::ReflectDefineProperty {
            target, descriptor, ..
        } => {
            trace_stored_value_root(target, mark);
            trace_stored_value_root(descriptor, mark);
        }
        PropertyKeyTarget::Write { base, value, .. } => {
            trace_stored_value_root(base, mark);
            trace_stored_value_root(value, mark);
        }
        PropertyKeyTarget::ReflectGet {
            target, receiver, ..
        } => {
            trace_stored_value_root(target, mark);
            trace_stored_value_root(receiver, mark);
        }
        PropertyKeyTarget::ReflectSet {
            target,
            receiver,
            value,
            ..
        } => {
            trace_stored_value_root(target, mark);
            trace_stored_value_root(receiver, mark);
            trace_stored_value_root(value, mark);
        }
        PropertyKeyTarget::DefineMethod { base, function, .. } => {
            trace_stored_value_root(base, mark);
            trace_stored_value_root(function, mark);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive tracer keeps every primitive-conversion target's heap roots explicit"
)]
fn trace_operator_primitive_target_roots(
    target: &OperatorPrimitiveTarget,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    match target {
        // Primitive continuation payloads carry no heap roots; in particular,
        // a `BigInt` payload is not a heap node.
        OperatorPrimitiveTarget::Unary { .. }
        | OperatorPrimitiveTarget::NumberToString { .. }
        | OperatorPrimitiveTarget::NumberFormatDigits { .. }
        | OperatorPrimitiveTarget::DateParse
        | OperatorPrimitiveTarget::DateToPrimitive
        | OperatorPrimitiveTarget::TemporalInstantNanoseconds { new_target: None }
        | OperatorPrimitiveTarget::TemporalInstantMilliseconds
        | OperatorPrimitiveTarget::GlobalNumeric(_)
        | OperatorPrimitiveTarget::MathUnary(_)
        | OperatorPrimitiveTarget::GlobalUri(_)
        | OperatorPrimitiveTarget::GlobalParseIntRadix { .. }
        | OperatorPrimitiveTarget::SymbolIntrinsic { .. }
        | OperatorPrimitiveTarget::StringIteratorIntrinsic
        | OperatorPrimitiveTarget::JsonRawJsonText
        | OperatorPrimitiveTarget::BigIntToString { .. }
        | OperatorPrimitiveTarget::BigIntTruncationValue { .. }
        // The converted left Number carries no heap edge.
        | OperatorPrimitiveTarget::MathBinaryFinish { .. } => {}
        OperatorPrimitiveTarget::DateSetTime { object }
        | OperatorPrimitiveTarget::ArrayBufferResize { object }
        | OperatorPrimitiveTarget::ArrayBufferTransfer { object, .. } => {
            mark(CollectionRoot::Heap(HeapReference::Object(*object)));
        }
        OperatorPrimitiveTarget::DateConstructor { new_target }
        | OperatorPrimitiveTarget::ArrayBufferConstructorMax { new_target, .. } => {
            mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
        }
        OperatorPrimitiveTarget::TemporalInstantNanoseconds {
            new_target: Some(new_target),
        } => mark(CollectionRoot::Heap(HeapReference::Function(*new_target))),
        OperatorPrimitiveTarget::DateUtc(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DateConstructorComponents(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TemporalDurationConstructor(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TemporalDurationBag(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TemporalDurationRoundLargestUnit(state)
        | OperatorPrimitiveTarget::TemporalDurationRoundRoundingIncrement(state)
        | OperatorPrimitiveTarget::TemporalDurationRoundRoundingMode(state)
        | OperatorPrimitiveTarget::TemporalDurationRoundSmallestUnit(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TemporalDurationTotalUnit(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TemporalDurationToStringFractionalSecondDigits(state)
        | OperatorPrimitiveTarget::TemporalDurationToStringRoundingMode(state)
        | OperatorPrimitiveTarget::TemporalDurationToStringSmallestUnit(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TemporalInstantRoundRoundingIncrement(state)
        | OperatorPrimitiveTarget::TemporalInstantRoundRoundingMode(state)
        | OperatorPrimitiveTarget::TemporalInstantRoundSmallestUnit(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TemporalInstantDifferenceLargestUnit(state)
        | OperatorPrimitiveTarget::TemporalInstantDifferenceRoundingIncrement(state)
        | OperatorPrimitiveTarget::TemporalInstantDifferenceRoundingMode(state)
        | OperatorPrimitiveTarget::TemporalInstantDifferenceSmallestUnit(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TemporalInstantToStringFractionalSecondDigits(state)
        | OperatorPrimitiveTarget::TemporalInstantToStringRoundingMode(state)
        | OperatorPrimitiveTarget::TemporalInstantToStringSmallestUnit(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::DateSetter(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DateToJson(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TemporalInstantString(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::JsonParseText(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::JsonParseArrayLength(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::JsonStringifyReplacerItem(state)
        | OperatorPrimitiveTarget::JsonStringifyReplacerLength(state)
        | OperatorPrimitiveTarget::JsonStringifySpaceNumber(state)
        | OperatorPrimitiveTarget::JsonStringifySpaceString(state)
        | OperatorPrimitiveTarget::JsonStringifyContainerLength(state)
        | OperatorPrimitiveTarget::JsonStringifyBoxedNumber(state)
        | OperatorPrimitiveTarget::JsonStringifyBoxedString(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::BinaryRight { right, .. }
        | OperatorPrimitiveTarget::MathBinaryRight { right, .. } => {
            trace_stored_value_root(right, mark);
        }
        OperatorPrimitiveTarget::BinaryFinish { left, .. } => {
            trace_stored_value_root(left, mark);
        }
        OperatorPrimitiveTarget::EqualityFinish { other, .. } => {
            trace_stored_value_root(other, mark);
        }
        OperatorPrimitiveTarget::GlobalParseIntString { radix } => {
            trace_stored_value_root(radix, mark);
        }
        OperatorPrimitiveTarget::NumberIntrinsic { new_target }
        | OperatorPrimitiveTarget::StringIntrinsic { new_target } => {
            if let Some(new_target) = new_target {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
            }
        }
        OperatorPrimitiveTarget::ArrayBufferConstructorLength(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayBufferSliceStart(state)
        | OperatorPrimitiveTarget::ArrayBufferSliceEnd(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DataViewConstructorOffset(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DataViewConstructorByteLength(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayConstructorLength(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayConstructorBufferOffset(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayConstructorBufferLength(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayConstructorArrayLikeLength(state)
        | OperatorPrimitiveTarget::TypedArrayConstructorElement(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayPrototypeSetOffset(state)
        | OperatorPrimitiveTarget::TypedArrayPrototypeSetSourceLength(state)
        | OperatorPrimitiveTarget::TypedArrayPrototypeSetElement(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayPrototypeSubarrayBegin(state)
        | OperatorPrimitiveTarget::TypedArrayPrototypeSubarrayEnd(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayPrototypeSliceStart(state)
        | OperatorPrimitiveTarget::TypedArrayPrototypeSliceEnd(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayPrototypeAtIndex(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayPrototypeIncludesFromIndex(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TypedArrayPrototypeIndexOfFromIndex(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TypedArrayPrototypeLastIndexOfFromIndex(state) => {
            state.trace_roots(mark);
        }
        OperatorPrimitiveTarget::TypedArrayPrototypeFill(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayPrototypeCopyWithin(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DataViewGetIndex(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DataViewSetOffset(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::DataViewSetValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::TypedArrayElementSet(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ErrorConstructorMessage(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ErrorToStringName(state)
        | OperatorPrimitiveTarget::ErrorToStringMessage(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayIteratorLength(state) => {
            mark(CollectionRoot::Heap(HeapReference::Object(state.iterator)));
            trace_stored_value_root(&state.iterated, mark);
        }
        OperatorPrimitiveTarget::FunctionApplyLength(state) => {
            trace_function_apply_roots(state, mark);
        }
        OperatorPrimitiveTarget::ProxyOwnKeysLength(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            if let Some(result) = state.result {
                mark(CollectionRoot::Heap(result));
            }
        }
        OperatorPrimitiveTarget::StringMethodSubject(state)
        | OperatorPrimitiveTarget::StringMethodArgument(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArraySearchPosition(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayMutatorArgument(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayCopierArgument(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArraySpliceArgument(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArraySortValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayFlattenValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayStaticLength(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayFromAsyncLength { operation } => {
            mark(CollectionRoot::Heap(HeapReference::Object(*operation)));
        }
        OperatorPrimitiveTarget::SetRecordSize(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::StringRawValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::StringReplaceValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::StringSplitValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::RegExpValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::LocaleStringValue(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::MathExtrema(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::MathHypot(state) => state.trace_roots(mark),
        OperatorPrimitiveTarget::ArrayJoinSeparator(state)
        | OperatorPrimitiveTarget::ArrayJoinElement(state) => {
            trace_stored_value_root(state.target(), mark);
        }
        // The pending truncation operand is a real value and must be traced.
        OperatorPrimitiveTarget::BigIntTruncationBits { value, .. } => {
            trace_stored_value_root(value, mark);
        }
        OperatorPrimitiveTarget::ArrayLengthWrite(state) => {
            trace_stored_value_root(&state.base, mark);
            if let Some(original) = &state.original {
                trace_stored_value_root(original, mark);
            }
        }
    }
}

fn trace_function_apply_roots(
    state: &FunctionApplyContinuation,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    mark(CollectionRoot::Heap(HeapReference::Function(state.target)));
    if let Some(new_target) = state.new_target {
        mark(CollectionRoot::Heap(HeapReference::Function(new_target)));
    }
    trace_stored_value_root(&state.receiver, mark);
    trace_stored_value_root(&state.array_like, mark);
    for argument in &state.arguments {
        trace_stored_value_root(argument, mark);
    }
}

fn trace_function_bind_roots(
    state: &FunctionBindContinuation,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    mark(CollectionRoot::Heap(HeapReference::Function(state.target)));
    trace_stored_value_root(&state.bound_this, mark);
    for argument in &state.bound_arguments {
        trace_stored_value_root(argument, mark);
    }
}

fn trace_instance_of_roots(state: &InstanceOfContinuation, mark: &mut dyn FnMut(CollectionRoot)) {
    trace_stored_value_root(&state.value, mark);
    trace_stored_value_root(&state.target, mark);
    if let Some(prototype) = state.prototype {
        mark(CollectionRoot::Heap(prototype));
    }
    if let Some(current) = state.current {
        mark(CollectionRoot::Heap(current));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "each native continuation traces its own rooted operand set; the copy-data-properties arm adds target, source, excluded, and the pending key"
)]
fn trace_native_continuation_roots(
    continuation: &NativeContinuation,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    match continuation {
        NativeContinuation::FunctionSource(state) => {
            for argument in &state.arguments {
                trace_stored_value_root(argument, mark);
            }
            if let Some(construction) = state.construction {
                mark(CollectionRoot::Heap(HeapReference::Function(construction)));
            }
        }
        NativeContinuation::FunctionApply(state) => {
            trace_function_apply_roots(state, mark);
        }
        NativeContinuation::ArrayJoin(state) => {
            trace_stored_value_root(state.target(), mark);
        }
        NativeContinuation::ArraySearch(state) => state.trace_roots(mark),
        NativeContinuation::ArrayMutator(state) => state.trace_roots(mark),
        NativeContinuation::ArrayCopier(state) => state.trace_roots(mark),
        NativeContinuation::ArrayCallback(state) => state.trace_roots(mark),
        NativeContinuation::ArrayReduction(state) => state.trace_roots(mark),
        NativeContinuation::ArraySplice(state) => state.trace_roots(mark),
        NativeContinuation::ArraySort(state) => state.trace_roots(mark),
        NativeContinuation::ArrayFlatten(state) => state.trace_roots(mark),
        NativeContinuation::ArrayStatic(state) => state.trace_roots(mark),
        NativeContinuation::ArrayFromAsync(state) => state.trace_roots(mark),
        NativeContinuation::StringRaw(state) => state.trace_roots(mark),
        NativeContinuation::StringReplace(state) => state.trace_roots(mark),
        NativeContinuation::StringSplit(state) => state.trace_roots(mark),
        NativeContinuation::RegExp(state) => state.trace_roots(mark),
        NativeContinuation::LocaleString(state) => state.trace_roots(mark),
        NativeContinuation::DefineProperty(state) => state.trace_roots(mark),
        NativeContinuation::FunctionBind(state) => {
            trace_function_bind_roots(state, mark);
        }
        NativeContinuation::PropertyKey(state) => {
            trace_stored_value_root(&state.receiver, mark);
            trace_property_key_target_roots(&state.target, mark);
        }
        NativeContinuation::OperatorPrimitive(state) => {
            trace_stored_value_root(&state.receiver, mark);
            trace_operator_primitive_target_roots(&state.target, mark);
        }
        NativeContinuation::ArrayBufferConstructor(state) => state.trace_roots(mark),
        NativeContinuation::ArrayBufferSlice(state) => state.trace_roots(mark),
        NativeContinuation::DataViewConstructor(state) => state.trace_roots(mark),
        NativeContinuation::TypedArrayConstructorObject(state) => state.trace_roots(mark),
        NativeContinuation::TypedArrayConstructorSequence(state) => state.trace_roots(mark),
        NativeContinuation::TypedArrayPrototypeSet(state) => state.trace_roots(mark),
        NativeContinuation::TypedArrayPrototypeSubarray(state) => state.trace_roots(mark),
        NativeContinuation::TypedArrayPrototypeSlice(state) => state.trace_roots(mark),
        NativeContinuation::DateToJson(state) => state.trace_roots(mark),
        NativeContinuation::TemporalDurationBag(state) => state.trace_roots(mark),
        NativeContinuation::TemporalDurationCompareOptions(state) => state.trace_roots(mark),
        NativeContinuation::TemporalDurationRoundOptions(state) => state.trace_roots(mark),
        NativeContinuation::TemporalDurationTotalOptions(state) => state.trace_roots(mark),
        NativeContinuation::TemporalDurationToStringOptions(state) => state.trace_roots(mark),
        NativeContinuation::TemporalInstantRoundOptions(state) => state.trace_roots(mark),
        NativeContinuation::TemporalInstantDifferenceOptions(state) => state.trace_roots(mark),
        NativeContinuation::TemporalInstantToStringOptions(state) => state.trace_roots(mark),
        NativeContinuation::IntrinsicGet(state) => match state {
            IntrinsicGetContinuation::BooleanConstructor { new_target, .. }
            | IntrinsicGetContinuation::NumberConstructor { new_target, .. }
            | IntrinsicGetContinuation::DateConstructor { new_target, .. }
            | IntrinsicGetContinuation::ArrayBufferConstructor { new_target, .. }
            | IntrinsicGetContinuation::TypedArrayConstructor { new_target, .. }
            | IntrinsicGetContinuation::TemporalInstantConstructor { new_target, .. }
            | IntrinsicGetContinuation::TemporalDurationConstructor { new_target, .. }
            | IntrinsicGetContinuation::StringConstructor { new_target, .. } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
            }
            IntrinsicGetContinuation::ArrayConstructor {
                new_target,
                arguments,
                ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
                for argument in arguments {
                    trace_stored_value_root(argument, mark);
                }
            }
            IntrinsicGetContinuation::MapConstructor {
                new_target,
                iterable,
                ..
            }
            | IntrinsicGetContinuation::SetConstructor {
                new_target,
                iterable,
                ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
                trace_stored_value_root(iterable, mark);
            }
            IntrinsicGetContinuation::PromiseConstructor {
                new_target,
                executor,
                ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
                mark(CollectionRoot::Heap(HeapReference::Function(*executor)));
            }
            IntrinsicGetContinuation::WeakRefConstructor {
                new_target, target, ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
                trace_stored_value_root(target, mark);
            }
            IntrinsicGetContinuation::FinalizationRegistryConstructor {
                new_target,
                cleanup_callback,
                ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
                mark(CollectionRoot::Heap(HeapReference::Function(
                    *cleanup_callback,
                )));
            }
            IntrinsicGetContinuation::ObjectPrototypeToString {
                temporary_receiver, ..
            } => {
                if let Some(receiver) = temporary_receiver {
                    mark(CollectionRoot::Heap(HeapReference::Object(*receiver)));
                }
            }
        },
        NativeContinuation::AggregateError(state) => state.trace_roots(mark),
        NativeContinuation::FromEntries(state) => state.trace_roots(mark),
        NativeContinuation::GroupBy(state) => state.trace_roots(mark),
        NativeContinuation::MapConstructor(state) => state.trace_roots(mark),
        NativeContinuation::MapForEach(state) => state.trace_roots(mark),
        NativeContinuation::MapComputed(state) => state.trace_roots(mark),
        NativeContinuation::SetConstructor(state) => state.trace_roots(mark),
        NativeContinuation::SetForEach(state) => state.trace_roots(mark),
        NativeContinuation::SetOperation(state) => state.trace_roots(mark),
        NativeContinuation::MathSumPrecise(state) => state.trace_roots(mark),
        NativeContinuation::JsonParse(state) => state.trace_roots(mark),
        NativeContinuation::JsonStringify(state) => state.trace_roots(mark),
        NativeContinuation::ErrorConstructor(state) => state.trace_roots(mark),
        NativeContinuation::ErrorToString(state) => state.trace_roots(mark),
        NativeContinuation::ArrayIteratorNext(state) => {
            mark(CollectionRoot::Heap(HeapReference::Object(state.iterator)));
            trace_stored_value_root(&state.iterated, mark);
        }
        NativeContinuation::ForOfStart(state) => {
            trace_stored_value_root(&state.iterable, mark);
            if let Some(iterator) = &state.iterator {
                trace_stored_value_root(iterator, mark);
            }
        }
        NativeContinuation::ForOfNext(state) => {
            trace_stored_value_root(&state.iterator, mark);
            trace_stored_value_root(&state.next, mark);
            if let Some(result) = &state.result {
                trace_stored_value_root(result, mark);
            }
        }
        NativeContinuation::ForOfClose(state) => {
            trace_stored_value_root(&state.iterator, mark);
        }
        NativeContinuation::AsyncFromSync(state) => {
            mark(CollectionRoot::Heap(HeapReference::Object(state.wrapper)));
            trace_stored_value_root(&state.iterator, mark);
            trace_stored_value_root(&state.next, mark);
            if let Some(input) = &state.input {
                trace_stored_value_root(input, mark);
            }
            if let Some(result) = &state.result {
                trace_stored_value_root(result, mark);
            }
            trace_stored_value_root(&state.capability.promise, mark);
            mark(CollectionRoot::Heap(HeapReference::Function(
                state.capability.resolve,
            )));
            mark(CollectionRoot::Heap(HeapReference::Function(
                state.capability.reject,
            )));
        }
        NativeContinuation::AsyncFromSyncClose(state) => {
            trace_stored_value_root(&state.iterator, mark);
            trace_stored_value_root(&state.reason, mark);
            if let AsyncFromSyncCloseTarget::Capability(capability) = &state.target {
                trace_stored_value_root(&capability.promise, mark);
                mark(CollectionRoot::Heap(HeapReference::Function(
                    capability.resolve,
                )));
                mark(CollectionRoot::Heap(HeapReference::Function(
                    capability.reject,
                )));
            }
        }
        NativeContinuation::YieldStarIteratorCall(state) => {
            trace_stored_value_root(&state.iterator, mark);
            if let Some(input) = &state.input {
                trace_stored_value_root(input, mark);
            }
        }
        NativeContinuation::IteratorAppend(state) => {
            mark(CollectionRoot::Heap(HeapReference::Object(state.array)));
            trace_stored_value_root(&state.iterable, mark);
            if let Some(iterator) = &state.iterator {
                trace_stored_value_root(iterator, mark);
            }
            if let Some(method) = state.next_method {
                mark(CollectionRoot::Heap(HeapReference::Function(method)));
            }
            if let Some(result) = &state.result {
                trace_stored_value_root(result, mark);
            }
        }
        NativeContinuation::IteratorClose(state) => {
            trace_stored_value_root(&state.iterator, mark);
            if let PendingExceptionPayload::ThrownValue(value) = &state.original.payload {
                trace_stored_value_root(value, mark);
            }
        }
        NativeContinuation::ForIn(state) => state.trace_roots(mark),
        NativeContinuation::CopyDataProperties(state) => {
            trace_stored_value_root(&state.target, mark);
            trace_stored_value_root(&state.source, mark);
            if let Some(excluded) = &state.excluded {
                trace_stored_value_root(excluded, mark);
            }
        }
        NativeContinuation::EnumerableOwnProperties(state) => state.trace_roots(mark),
        NativeContinuation::ObjectKeyListing(state) => state.trace_roots(mark),
        NativeContinuation::ProxyEnumerable(state) => state.trace_roots(mark),
        NativeContinuation::ObjectAssign(state) => state.trace_roots(mark),
        NativeContinuation::GetOwnPropertyDescriptors(state) => state.trace_roots(mark),
        NativeContinuation::DefineProperties(state) => state.trace_roots(mark),
        NativeContinuation::InstanceOf(state) => {
            trace_instance_of_roots(state, mark);
        }
        NativeContinuation::Promise(state) => state.trace_roots(mark),
        NativeContinuation::PromiseCombinator(state) => state.trace_roots(mark),
        NativeContinuation::ProxyGet(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            trace_stored_value_root(&state.receiver, mark);
        }
        NativeContinuation::ProxyCall(state) => {
            mark(CollectionRoot::Heap(HeapReference::Function(state.proxy)));
            mark(CollectionRoot::Heap(HeapReference::Function(state.target)));
            mark(CollectionRoot::Heap(state.handler));
            trace_stored_value_root(&state.receiver, mark);
            for value in state.arguments.remaining() {
                trace_stored_value_root(value, mark);
            }
            if let Some(new_target) = state.new_target {
                mark(CollectionRoot::Heap(HeapReference::Function(new_target)));
            }
        }
        NativeContinuation::ProxyBoolean(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            if let Some(value) = &state.value {
                trace_stored_value_root(value, mark);
            }
            if let Some(receiver) = &state.receiver {
                trace_stored_value_root(receiver, mark);
            }
        }
        NativeContinuation::OrdinarySetReceiver(state) => {
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.receiver));
            trace_stored_value_root(&state.value, mark);
        }
        NativeContinuation::ProxyMeta(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            if let Some(Some(prototype)) = state.requested_prototype {
                mark(CollectionRoot::Heap(prototype));
            }
            if let Some(result) = &state.trap_result {
                trace_stored_value_root(result, mark);
            }
        }
        NativeContinuation::ProxyDescriptor(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            if let Some(reader) = &state.reader {
                reader.trace_roots(mark);
            }
            for property in [
                state.trap_descriptor.as_ref().and_then(Option::as_ref),
                state.target_descriptor.as_ref().and_then(Option::as_ref),
            ]
            .into_iter()
            .flatten()
            {
                match property {
                    OwnProperty::Data { value, .. } => trace_stored_value_root(value, mark),
                    OwnProperty::Accessor { getter, setter, .. } => {
                        if let Some(getter) = getter {
                            mark(CollectionRoot::Heap(HeapReference::Function(*getter)));
                        }
                        if let Some(setter) = setter {
                            mark(CollectionRoot::Heap(HeapReference::Function(*setter)));
                        }
                    }
                }
            }
        }
        NativeContinuation::ProxyDefine(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            mark(CollectionRoot::Heap(HeapReference::Object(
                state.descriptor,
            )));
            if let Some(value) = state.definition.requested_value() {
                trace_stored_value_root(value, mark);
            }
            for function in [
                state.definition.requested_getter().flatten(),
                state.definition.requested_setter().flatten(),
            ]
            .into_iter()
            .flatten()
            {
                mark(CollectionRoot::Heap(HeapReference::Function(function)));
            }
            if let Some(reader) = &state.reader {
                reader.trace_roots(mark);
            }
            if let Some(OwnProperty::Data { value, .. }) =
                state.target_descriptor.as_ref().and_then(Option::as_ref)
            {
                trace_stored_value_root(value, mark);
            }
            if let Some(OwnProperty::Accessor { getter, setter, .. }) =
                state.target_descriptor.as_ref().and_then(Option::as_ref)
            {
                if let Some(getter) = getter {
                    mark(CollectionRoot::Heap(HeapReference::Function(*getter)));
                }
                if let Some(setter) = setter {
                    mark(CollectionRoot::Heap(HeapReference::Function(*setter)));
                }
            }
        }
        NativeContinuation::ProxyOwnKeys(state) => {
            mark(CollectionRoot::Heap(state.proxy));
            mark(CollectionRoot::Heap(state.target));
            mark(CollectionRoot::Heap(state.handler));
            if let Some(result) = state.result {
                mark(CollectionRoot::Heap(result));
            }
        }
        NativeContinuation::IntegrityLevel(state) => state.trace_roots(mark),
        NativeContinuation::IsPrototypeOf(state) => state.trace_roots(mark),
        NativeContinuation::ObjectMeta(state) => trace_stored_value_root(&state.completion, mark),
        NativeContinuation::AsyncGeneratorReturnAwait { completion, .. } => {
            trace_stored_value_root(completion, mark);
        }
        NativeContinuation::OwnDescriptorQuery(_)
        | NativeContinuation::AsyncAwait { .. }
        | NativeContinuation::ReflectSet
        | NativeContinuation::ProxyWrite
        | NativeContinuation::FunctionCall => {}
    }
}

pub(crate) fn trace_frame_roots(frame: &Frame, mark: &mut dyn FnMut(CollectionRoot)) {
    mark(CollectionRoot::Heap(HeapReference::Function(
        frame.function,
    )));
    trace_stored_value_root(&frame.receiver, mark);
    if let Some(new_target) = frame.new_target {
        mark(CollectionRoot::Heap(HeapReference::Function(new_target)));
    }
    if let Some(arguments) = &frame.arguments_snapshot {
        for value in arguments {
            trace_stored_value_root(value, mark);
        }
    }
    for binding in frame.arguments.iter().chain(&frame.locals) {
        trace_frame_binding_root(binding, mark);
    }
    for cell in frame.own_cells.iter().flatten() {
        mark(CollectionRoot::BindingCell(*cell));
    }
    for binding in &frame.environment {
        if let EnvironmentBinding::Captured(cell) = binding {
            mark(CollectionRoot::BindingCell(*cell));
        }
    }
    for entry in &frame.stack {
        if let OperandStackEntry::JavaScript(value) = entry {
            trace_stored_value_root(value, mark);
        }
    }
    for continuation in &frame.native_returns {
        trace_native_continuation_roots(continuation, mark);
    }
    if let Some(generator) = frame.generator_resume {
        mark(CollectionRoot::Heap(HeapReference::Object(generator)));
    }
    if let Some(result) = frame.generator_result {
        mark(CollectionRoot::Heap(HeapReference::Object(result)));
    }
    if let Some(pending) = &frame.resume_abrupt
        && let PendingExceptionPayload::ThrownValue(value) = &pending.payload
    {
        trace_stored_value_root(value, mark);
    }
    if let Some(dynamic) = &frame.dynamic_return {
        mark(CollectionRoot::Heap(HeapReference::Function(
            dynamic.root.function,
        )));
        if let Some(construction) = dynamic.construction {
            mark(CollectionRoot::Heap(HeapReference::Function(construction)));
        }
    }
}

fn collect_cycles_with_execution_roots(
    runtime: &mut Runtime,
    frames: &[Frame],
    continuations: &[NativeContinuation],
    values: &[StoredValue],
) -> Result<(), ExecutionError> {
    let has_ephemeral_heap_roots = !frames.is_empty()
        || !continuations.is_empty()
        || values
            .iter()
            .any(|value| matches!(value, StoredValue::Function(_) | StoredValue::Object(_)));
    runtime
        .collect_cycles_with_roots(|mark| {
            for frame in frames {
                trace_frame_roots(frame, mark);
            }
            for continuation in continuations {
                trace_native_continuation_roots(continuation, mark);
            }
            for value in values {
                trace_stored_value_root(value, mark);
            }
        })
        .map_err(runtime_collection_execution_error)?;

    // Active VM roots are ephemeral: a later pop, catch unwind, or frame
    // return can make an object unreachable without mutating the heap graph.
    // Keep the collector dirty until a root-free execution safe point observes
    // those removals.
    if has_ephemeral_heap_roots {
        runtime.collection_pending = true;
    }
    Ok(())
}

fn runtime_collection_execution_error(error: RuntimeError) -> ExecutionError {
    match error {
        RuntimeError::LimitExceeded {
            resource,
            limit,
            observed,
        } => ExecutionError::LimitExceeded {
            resource,
            limit,
            observed,
        },
        RuntimeError::AllocationFailed {
            resource,
            additional,
        } => ExecutionError::AllocationFailed {
            resource,
            additional,
        },
        RuntimeError::Atom(source) => ExecutionError::Atom(source),
    }
}

fn native_continuations_have_temporary_receiver(continuations: &[NativeContinuation]) -> bool {
    continuations.iter().any(|continuation| {
        matches!(
            continuation,
            NativeContinuation::IntrinsicGet(IntrinsicGetContinuation::ObjectPrototypeToString {
                temporary_receiver: Some(_),
                ..
            })
        )
    })
}

fn frames_have_temporary_receiver(frames: &[Frame]) -> bool {
    frames.iter().any(|frame| {
        frame.transient_cleanup_pending
            || native_continuations_have_temporary_receiver(&frame.native_returns)
    })
}

fn native_dispatch_has_temporary_receiver(dispatch: &NativeDispatch) -> bool {
    match dispatch {
        NativeDispatch::Immediate(_)
        | NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => false,
        NativeDispatch::Frame(frame) => {
            native_continuations_have_temporary_receiver(&frame.native_returns)
        }
        NativeDispatch::Call(call) => {
            native_continuations_have_temporary_receiver(&call.continuations)
        }
    }
}

#[derive(Clone, Copy)]
struct FramePlan {
    function: FunctionId,
    code: InstalledCodeId,
    template: FunctionTemplateId,
    argument_count: usize,
    local_count: usize,
    stack_capacity: usize,
    reserved_values: u64,
    arguments_snapshot_use: ArgumentsSnapshotUse,
    construction: bool,
    constructor_profile: ConstructorProfile,
    strict: bool,
    receiver_access: ReceiverAccess,
    asynchronous: bool,
    instruction: InstructionIndex,
}

#[derive(Clone, Copy)]
enum ArgumentsSnapshotUse {
    None,
    ArgumentsObject,
    RestParameter,
    ArgumentsObjectAndRestParameter,
}

impl ArgumentsSnapshotUse {
    const fn is_needed(self) -> bool {
        !matches!(self, Self::None)
    }

    const fn has_rest_parameter(self) -> bool {
        matches!(
            self,
            Self::RestParameter | Self::ArgumentsObjectAndRestParameter
        )
    }
}

#[derive(Clone, Copy)]
enum ReceiverAccess {
    Direct,
    Lexical,
    NormalizeSloppy,
}

impl ReceiverAccess {
    const fn for_function(strict: bool, executable_kind: CompilerExecutableKind) -> Self {
        if matches!(executable_kind, CompilerExecutableKind::OrdinaryArrow) {
            Self::Lexical
        } else if strict
            || matches!(
                executable_kind,
                CompilerExecutableKind::DynamicFunctionScript
            )
        {
            Self::Direct
        } else {
            Self::NormalizeSloppy
        }
    }
}

fn receiver_profile(function: &VerifiedBytecodeFunction<'_>) -> (bool, ReceiverAccess) {
    let strict = function
        .function()
        .control_flow()
        .function_header()
        .mode()
        .is_strict();
    (
        strict,
        ReceiverAccess::for_function(strict, function.metadata().executable_kind()),
    )
}

fn normalize_receiver(
    runtime: &mut Runtime,
    realm: RealmId,
    access: ReceiverAccess,
    receiver: StoredValue,
) -> Result<StoredValue, ExecutionError> {
    if matches!(access, ReceiverAccess::Direct) {
        return Ok(receiver);
    }
    if matches!(access, ReceiverAccess::Lexical) {
        return Err(EngineFault::RuntimeInvariant {
            message: "lexical receiver reached dynamic receiver normalization",
        }
        .into());
    }
    match receiver {
        StoredValue::Undefined | StoredValue::Null => runtime
            .realm_global_object(realm)
            .map(StoredValue::Object)
            .map_err(ExecutionError::from),
        StoredValue::Function(_) | StoredValue::Object(_) => Ok(receiver),
        StoredValue::Boolean(value) => runtime
            .allocate_boxed_boolean(realm, value)
            .map(StoredValue::Object),
        StoredValue::Number(value) => runtime
            .allocate_boxed_number(realm, value)
            .map(StoredValue::Object),
        StoredValue::BigInt(value) => runtime
            .allocate_boxed_bigint(realm, value)
            .map(StoredValue::Object),
        StoredValue::String(value) => runtime
            .allocate_boxed_string(realm, value)
            .map(StoredValue::Object),
        StoredValue::Symbol(_) => Err(EngineFault::RuntimeInvariant {
            message: "primitive sloppy receiver reached the pre-wrapper object profile",
        }
        .into()),
    }
}

#[derive(Clone, Copy)]
enum CallKind {
    Direct,
    Method,
    Constructor,
}

enum CallInputSource {
    Frame {
        argument_count: usize,
        kind: CallKind,
    },
    Prepared(CallInputs),
}

impl CallInputSource {
    const fn is_construction(&self) -> bool {
        match self {
            Self::Frame { kind, .. } => matches!(kind, CallKind::Constructor),
            Self::Prepared(inputs) => inputs.new_target.is_some(),
        }
    }

    fn argument_count(&self) -> usize {
        match self {
            Self::Frame { argument_count, .. } => *argument_count,
            Self::Prepared(inputs) => inputs.arguments.remaining().len(),
        }
    }
}

#[derive(Clone, Copy)]
enum ReturnDisposition {
    Push,
    Discard,
    InitializeDerivedThis,
}

#[derive(Clone, Copy)]
struct CallReturn {
    instruction: InstructionIndex,
    disposition: ReturnDisposition,
}

impl CallReturn {
    const fn push(instruction: InstructionIndex) -> Self {
        Self {
            instruction,
            disposition: ReturnDisposition::Push,
        }
    }

    const fn discard(instruction: InstructionIndex) -> Self {
        Self {
            instruction,
            disposition: ReturnDisposition::Discard,
        }
    }

    const fn initialize_derived_this(instruction: InstructionIndex) -> Self {
        Self {
            instruction,
            disposition: ReturnDisposition::InitializeDerivedThis,
        }
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "boxing a native dispatch would add an unaccounted infallible allocation to the interpreter loop"
)]
enum Step {
    Continue,
    Call {
        function: FunctionId,
        inputs: CallInputSource,
        return_to: CallReturn,
        source_pc: BytecodePc,
    },
    Apply {
        function: FunctionId,
        receiver: StoredValue,
        array_like: StoredValue,
        magic: u16,
        return_to: CallReturn,
        source_pc: BytecodePc,
    },
    Native {
        dispatch: NativeDispatch,
        return_to: CallReturn,
    },
    Abrupt(PendingException),
    Await {
        value: StoredValue,
        source_pc: BytecodePc,
    },
    InitialYield,
    Yield(StoredValue),
    YieldStar(StoredValue),
    AsyncYieldStar(StoredValue),
    Return {
        value: StoredValue,
        source_pc: BytecodePc,
    },
}

enum PendingExceptionPayload {
    EngineError {
        kind: ExceptionKind,
        message: JsString,
    },
    FrozenEngineError {
        kind: ExceptionKind,
        message: JsString,
        stack: JsString,
    },
    ThrownValue(StoredValue),
}

struct PendingException {
    realm: RealmId,
    payload: PendingExceptionPayload,
    origin: JsStackFrame,
}

enum FrameArguments<'a> {
    Public(&'a [JsValue]),
    Owned(CallArguments),
}

#[derive(Clone, Copy)]
enum BindingName {
    Local(u32),
    Closure(u32),
}

enum ClosureCapturePlan {
    Existing(EnvironmentBinding),
    New(usize),
}

struct PendingOwnCell {
    own_index: usize,
    address: FrameBindingAddress,
    value: SlotValue,
}

impl Context<'_> {
    /// Invokes one runtime-installed ordinary bytecode function.
    ///
    /// Execution starts only at verified instruction zero and advances only
    /// through verified successor identities. Ordinary direct and method calls
    /// push runtime frames onto one explicit vector; Rust stack recursion is
    /// never used. Constructors remain outside this profile.
    ///
    /// # Errors
    ///
    /// Rejects orphaned, foreign, or stale handles before frame mutation.
    /// During execution it returns the admitted exact TDZ `ReferenceError`,
    /// non-callable `TypeError`, or arbitrary value from an explicit
    /// JavaScript `throw`, with verified origin and caller provenance, plus
    /// instruction interruption, resource/allocation failures, or internal
    /// engine faults.
    pub fn call(
        &mut self,
        function: &Function,
        arguments: &[JsValue],
        limits: ExecutionLimits,
    ) -> Result<JsValue, ExecutionError> {
        self.call_with_optional_dynamic_function_compiler(function, arguments, limits, None)
    }

    /// Invokes a runtime function with an immutable dynamic-function compiler
    /// available to nested `%Function%` and `%GeneratorFunction%` calls.
    ///
    /// The compiler receives only owned source strings and returns only a
    /// complete [`quickjs_bytecode::VerifiedBytecode`] authority. It never
    /// receives this context, the runtime, or a caller lexical environment.
    ///
    /// # Errors
    ///
    /// Returns the same execution failures as [`Self::call`], plus a typed
    /// dynamic-compilation failure or JavaScript `SyntaxError`.
    pub fn call_with_dynamic_function_compiler(
        &mut self,
        function: &Function,
        arguments: &[JsValue],
        limits: ExecutionLimits,
        compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    ) -> Result<JsValue, ExecutionError> {
        self.call_with_optional_dynamic_function_compiler(
            function,
            arguments,
            limits,
            Some(compiler),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "host-call admission keeps native, bound, and bytecode dispatch plus their failure paths in one audited boundary"
    )]
    fn call_with_optional_dynamic_function_compiler(
        &mut self,
        function: &Function,
        arguments: &[JsValue],
        limits: ExecutionLimits,
        compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    ) -> Result<JsValue, ExecutionError> {
        self.runtime.prepare_execution_safe_point()?;
        let owner = function.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Function)?;
        let function_id = function.id()?;
        if !self.runtime.functions.contains(function_id) {
            return Err(HandleError::Stale {
                kind: HandleKind::Function,
                index: function_id.index(),
                generation: function_id.generation(),
            }
            .into());
        }

        for argument in arguments {
            let owner = argument.owner()?;
            self.runtime.validate_owner(&owner, HandleKind::Value)?;
            if let Some(reference) = argument.stored()?.heap_reference()
                && !self.runtime.heap_reference_is_live(reference)
            {
                let (index, generation) = match reference {
                    HeapReference::Function(id) => (id.index(), id.generation()),
                    HeapReference::Object(id) => (id.index(), id.generation()),
                };
                return Err(HandleError::Stale {
                    kind: HandleKind::Value,
                    index,
                    generation,
                }
                .into());
            }
        }

        let mut execution_budget = ExecutionBudget::new(limits);
        let mut function_id = function_id;
        let mut receiver = StoredValue::Undefined;
        let mut owned_arguments: Option<Vec<StoredValue>> = None;
        loop {
            let node =
                self.runtime
                    .functions
                    .get(function_id)
                    .ok_or(EngineFault::StaleHeapEdge {
                        edge: "function",
                        index: function_id.index(),
                        generation: function_id.generation(),
                    })?;
            if let Some(native) = node.native().copied() {
                let materialized = if let Some(arguments) = owned_arguments {
                    arguments
                } else {
                    let mut stored: Vec<StoredValue> = Vec::new();
                    stored.try_reserve_exact(arguments.len()).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: arguments.len(),
                        }
                    })?;
                    for argument in arguments {
                        stored.push(argument.stored()?.duplicate());
                    }
                    stored
                };
                let completion = execute_native_entry_with_budget(
                    self.runtime,
                    function_id,
                    native,
                    receiver,
                    materialized,
                    compiler,
                    &mut execution_budget,
                )
                .and_then(|value| self.runtime.public_value(value));
                return complete_host_turn(
                    self.runtime,
                    compiler,
                    &mut execution_budget,
                    completion,
                );
            }
            if let Some(resolving) = node.promise_resolving().cloned() {
                let materialized = if let Some(arguments) = owned_arguments {
                    arguments
                } else {
                    let mut stored: Vec<StoredValue> = Vec::new();
                    stored.try_reserve_exact(arguments.len()).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: arguments.len(),
                        }
                    })?;
                    for argument in arguments {
                        stored.push(argument.stored()?.duplicate());
                    }
                    stored
                };
                let prepared_frames = Vec::new();
                let dispatch = dispatch_promise_resolving(
                    self.runtime,
                    &resolving,
                    CallArguments::from_values(materialized),
                    None,
                    native_function_host_origin(),
                    &mut execution_budget,
                );
                let dispatch = match dispatch {
                    Ok(dispatch) => resolve_native_dispatch(
                        self.runtime,
                        dispatch,
                        &prepared_frames,
                        0,
                        0,
                        compiler,
                        &mut execution_budget,
                    ),
                    Err(error) => Err(error),
                };
                let completion = execute_root_dispatch_with_budget(
                    self.runtime,
                    dispatch,
                    prepared_frames,
                    compiler,
                    &mut execution_budget,
                )
                .and_then(|value| self.runtime.public_value(value));
                return complete_host_turn(
                    self.runtime,
                    compiler,
                    &mut execution_budget,
                    completion,
                );
            }
            if let Some(executor) = node.promise_capability_executor().cloned() {
                let materialized = if let Some(arguments) = owned_arguments {
                    arguments
                } else {
                    let mut stored: Vec<StoredValue> = Vec::new();
                    stored.try_reserve_exact(arguments.len()).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: arguments.len(),
                        }
                    })?;
                    for argument in arguments {
                        stored.push(argument.stored()?.duplicate());
                    }
                    stored
                };
                let prepared_frames = Vec::new();
                let dispatch = dispatch_promise_capability_executor(
                    &executor,
                    CallArguments::from_values(materialized),
                    native_function_host_origin(),
                );
                let dispatch = match dispatch {
                    Ok(dispatch) => resolve_native_dispatch(
                        self.runtime,
                        dispatch,
                        &prepared_frames,
                        0,
                        0,
                        compiler,
                        &mut execution_budget,
                    ),
                    Err(error) => Err(error),
                };
                let completion = execute_root_dispatch_with_budget(
                    self.runtime,
                    dispatch,
                    prepared_frames,
                    compiler,
                    &mut execution_budget,
                )
                .and_then(|value| self.runtime.public_value(value));
                return complete_host_turn(
                    self.runtime,
                    compiler,
                    &mut execution_budget,
                    completion,
                );
            }
            if let Some(promise_finally) = node.promise_finally().cloned() {
                let materialized = if let Some(arguments) = owned_arguments {
                    arguments
                } else {
                    let mut stored: Vec<StoredValue> = Vec::new();
                    stored.try_reserve_exact(arguments.len()).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: arguments.len(),
                        }
                    })?;
                    for argument in arguments {
                        stored.push(argument.stored()?.duplicate());
                    }
                    stored
                };
                let prepared_frames = Vec::new();
                let dispatch = dispatch_promise_finally_function(
                    &promise_finally,
                    CallArguments::from_values(materialized),
                    None,
                    native_function_host_origin(),
                );
                let dispatch = match dispatch {
                    Ok(dispatch) => resolve_native_dispatch(
                        self.runtime,
                        dispatch,
                        &prepared_frames,
                        0,
                        0,
                        compiler,
                        &mut execution_budget,
                    ),
                    Err(error) => Err(error),
                };
                let completion = execute_root_dispatch_with_budget(
                    self.runtime,
                    dispatch,
                    prepared_frames,
                    compiler,
                    &mut execution_budget,
                )
                .and_then(|value| self.runtime.public_value(value));
                return complete_host_turn(
                    self.runtime,
                    compiler,
                    &mut execution_budget,
                    completion,
                );
            }
            if let Some(element) = node.promise_combinator_element().cloned() {
                let materialized = if let Some(arguments) = owned_arguments {
                    arguments
                } else {
                    let mut stored: Vec<StoredValue> = Vec::new();
                    stored.try_reserve_exact(arguments.len()).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: arguments.len(),
                        }
                    })?;
                    for argument in arguments {
                        stored.push(argument.stored()?.duplicate());
                    }
                    stored
                };
                let prepared_frames = Vec::new();
                let dispatch = dispatch_promise_combinator_element(
                    self.runtime,
                    &element,
                    CallArguments::from_values(materialized),
                    None,
                    native_function_host_origin(),
                );
                let dispatch = match dispatch {
                    Ok(dispatch) => resolve_native_dispatch(
                        self.runtime,
                        dispatch,
                        &prepared_frames,
                        0,
                        0,
                        compiler,
                        &mut execution_budget,
                    ),
                    Err(error) => Err(error),
                };
                let completion = execute_root_dispatch_with_budget(
                    self.runtime,
                    dispatch,
                    prepared_frames,
                    compiler,
                    &mut execution_budget,
                )
                .and_then(|value| self.runtime.public_value(value));
                return complete_host_turn(
                    self.runtime,
                    compiler,
                    &mut execution_budget,
                    completion,
                );
            }
            if node.proxy().is_some() || node.proxy_revoker().is_some() {
                let materialized = if let Some(arguments) = owned_arguments {
                    arguments
                } else {
                    let mut stored = Vec::new();
                    stored.try_reserve_exact(arguments.len()).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: arguments.len(),
                        }
                    })?;
                    for argument in arguments {
                        stored.push(argument.stored()?.duplicate());
                    }
                    stored
                };
                let prepared_frames = Vec::new();
                let dispatch = resolve_native_dispatch(
                    self.runtime,
                    NativeDispatch::Call(NativeCall {
                        function: function_id,
                        receiver,
                        arguments: CallArguments::from_values(materialized),
                        return_to: None,
                        origin: native_function_host_origin(),
                        continuations: Vec::new(),
                        pre_call: None,
                        new_target: None,
                        native_caller: None,
                    }),
                    &prepared_frames,
                    0,
                    0,
                    compiler,
                    &mut execution_budget,
                );
                let completion = execute_root_dispatch_with_budget(
                    self.runtime,
                    dispatch,
                    prepared_frames,
                    compiler,
                    &mut execution_budget,
                )
                .and_then(|value| self.runtime.public_value(value));
                return complete_host_turn(
                    self.runtime,
                    compiler,
                    &mut execution_budget,
                    completion,
                );
            }
            let Some(bound) = node.bound() else {
                break;
            };
            let accumulated = owned_arguments.take();
            let accumulated_len = accumulated.as_ref().map_or(arguments.len(), Vec::len);
            let mut merged = Vec::new();
            merged
                .try_reserve_exact(bound.bound_arguments.len().saturating_add(accumulated_len))
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: bound.bound_arguments.len().saturating_add(accumulated_len),
                })?;
            for argument in &bound.bound_arguments {
                merged.push(argument.duplicate());
            }
            if let Some(accumulated) = accumulated {
                merged.extend(accumulated);
            } else {
                for argument in arguments {
                    merged.push(argument.stored()?.duplicate());
                }
            }
            owned_arguments = Some(merged);
            receiver = bound.bound_this.duplicate();
            function_id = bound.target;
        }

        let supplied_argument_count = owned_arguments.as_ref().map_or(arguments.len(), Vec::len);
        let plan = plan_frame(
            self.runtime,
            function_id,
            0,
            0,
            supplied_argument_count,
            false,
        )?;
        let frame = create_frame(
            self.runtime,
            plan,
            receiver,
            None,
            match owned_arguments {
                Some(arguments) => FrameArguments::Owned(CallArguments::from_values(arguments)),
                None => FrameArguments::Public(arguments),
            },
            None,
            None,
        )?;
        let completion =
            execute_frames_with_budget(self.runtime, frame, compiler, None, &mut execution_budget)
                .and_then(|value| self.runtime.public_value(value));
        complete_host_turn(self.runtime, compiler, &mut execution_budget, completion)
    }

    pub(crate) fn execute_internal_root(
        &mut self,
        root: &mut InstalledRoot,
        receiver: StoredValue,
        limits: ExecutionLimits,
    ) -> Result<StoredValue, ExecutionError> {
        self.execute_internal_root_with_optional_dynamic_function_compiler(
            root, receiver, limits, None,
        )
    }

    pub(crate) fn execute_internal_root_with_dynamic_function_compiler(
        &mut self,
        root: &mut InstalledRoot,
        receiver: StoredValue,
        limits: ExecutionLimits,
        compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    ) -> Result<StoredValue, ExecutionError> {
        self.execute_internal_root_with_optional_dynamic_function_compiler(
            root,
            receiver,
            limits,
            Some(compiler),
        )
    }

    fn execute_internal_root_with_optional_dynamic_function_compiler(
        &mut self,
        root: &mut InstalledRoot,
        receiver: StoredValue,
        limits: ExecutionLimits,
        compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    ) -> Result<StoredValue, ExecutionError> {
        let plan = plan_frame(self.runtime, root.function, 0, 0, 0, false)?;
        let frame = create_frame(
            self.runtime,
            plan,
            receiver,
            None,
            FrameArguments::Owned(CallArguments::empty()),
            None,
            None,
        )?;
        execute_frames(self.runtime, frame, limits, compiler, Some(root))
    }
}

fn execute_frames(
    runtime: &mut Runtime,
    initial: Frame,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    unstarted_dynamic_root: Option<&mut InstalledRoot>,
) -> Result<StoredValue, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(limits);
    execute_frames_with_budget(
        runtime,
        initial,
        compiler,
        unstarted_dynamic_root,
        &mut execution_budget,
    )
}

fn execute_frames_with_budget(
    runtime: &mut Runtime,
    initial: Frame,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    unstarted_dynamic_root: Option<&mut InstalledRoot>,
    execution_budget: &mut ExecutionBudget,
) -> Result<StoredValue, ExecutionError> {
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    frames.push(initial);
    execute_prepared_frames_with_budget(
        runtime,
        frames,
        compiler,
        unstarted_dynamic_root,
        execution_budget,
    )
}

fn execute_prepared_frames_with_budget(
    runtime: &mut Runtime,
    mut frames: Vec<Frame>,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    unstarted_dynamic_root: Option<&mut InstalledRoot>,
    execution_budget: &mut ExecutionBudget,
) -> Result<StoredValue, ExecutionError> {
    let mut active_frame_values = frames.iter().fold(0_u64, |total, frame| {
        total.saturating_add(frame.reserved_values)
    });
    if let Some(root) = unstarted_dynamic_root {
        root.commit_environment()?;
    }

    let result = execute_frame_loop(
        runtime,
        &mut frames,
        &mut active_frame_values,
        compiler,
        execution_budget,
    );
    let generator_cleanup = if result.is_err() {
        complete_active_generator_resumes(runtime, &frames)
    } else {
        Ok(())
    };
    let reclaim_temporary_receivers = frames_have_temporary_receiver(&frames);
    let cleanup = retire_active_dynamic_roots(runtime, &mut frames);
    if let Err(fault) = cleanup {
        if reclaim_temporary_receivers {
            frames.clear();
            if runtime.collection_pending {
                let _ = runtime.collect_cycles();
            }
        }
        return Err(fault.into());
    }
    if let Err(error) = generator_cleanup {
        if reclaim_temporary_receivers {
            frames.clear();
            if runtime.collection_pending {
                let _ = runtime.collect_cycles();
            }
        }
        return Err(error);
    }
    if result.is_err() {
        if reclaim_temporary_receivers {
            frames.clear();
            if runtime.collection_pending {
                let _ = runtime.collect_cycles();
            }
        }
        return result;
    }
    if reclaim_temporary_receivers {
        let collection = runtime
            .collect_cycles_with_roots(|mark| {
                if let Ok(value) = &result {
                    trace_stored_value_root(value, mark);
                }
            })
            .map_err(runtime_collection_execution_error);
        collection?;
    }
    result
}

#[allow(
    clippy::too_many_lines,
    reason = "the iterative bytecode/native transition loop remains centralized so every abrupt path shares one cleanup boundary"
)]
fn execute_frame_loop(
    runtime: &mut Runtime,
    frames: &mut Vec<Frame>,
    active_frame_values: &mut u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<StoredValue, ExecutionError> {
    'frames: loop {
        if let Some(completion) = execution_budget.native_root_completion.take() {
            debug_assert!(frames.is_empty());
            return Ok(completion);
        }
        execution_budget.charge_instructions(1)?;
        // The interrupt poll is separate from the fuel charge: fuel is a
        // pre-committed budget, while an interrupt is a decision the host makes
        // during execution.
        if execution_budget.interrupt_counter.charge_step() && runtime.interrupts.should_interrupt()
        {
            return Err(ExecutionError::Interrupted {
                executed: execution_budget.executed_instructions,
            });
        }
        let frame = frames.last_mut().ok_or(EngineFault::MissingInstruction {
            function: FunctionTemplateId::new(0),
            instruction: 0,
        })?;
        if let Some(pending) = frame.resume_abrupt.take() {
            dispatch_pending_exception(
                runtime,
                frames,
                active_frame_values,
                pending,
                compiler,
                execution_budget,
            )?;
            continue;
        }
        let frame = frames.last_mut().ok_or(EngineFault::MissingInstruction {
            function: FunctionTemplateId::new(0),
            instruction: 0,
        })?;
        let step = execute_one(runtime, frame, execution_budget)?;
        match step {
            Step::Continue => {}
            Step::Call {
                function,
                inputs,
                return_to,
                source_pc,
            } => {
                let mut function = function;
                let mut inputs = inputs;
                let construction = inputs.is_construction();
                let mut native = None;
                let mut resolving = None;
                let mut capability_executor = None;
                let mut promise_finally = None;
                let mut promise_combinator_element = None;
                let mut proxy_function = false;
                loop {
                    let node =
                        runtime
                            .functions
                            .get(function)
                            .ok_or(EngineFault::StaleHeapEdge {
                                edge: "function",
                                index: function.index(),
                                generation: function.generation(),
                            })?;
                    if let Some(value) = node.native().copied() {
                        native = Some(value);
                        break;
                    }
                    if let Some(value) = node.promise_resolving().cloned() {
                        resolving = Some(value);
                        break;
                    }
                    if let Some(value) = node.promise_capability_executor().cloned() {
                        capability_executor = Some(value);
                        break;
                    }
                    if let Some(value) = node.promise_finally().cloned() {
                        promise_finally = Some(value);
                        break;
                    }
                    if let Some(value) = node.promise_combinator_element().cloned() {
                        promise_combinator_element = Some(value);
                        break;
                    }
                    if node.proxy().is_some() || node.proxy_revoker().is_some() {
                        proxy_function = true;
                        break;
                    }
                    let Some(bound) = node.bound() else {
                        break;
                    };
                    if construction && !function_is_constructor(runtime, function)? {
                        let caller = frames.last().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        let origin = instruction_location(runtime, caller, source_pc)?;
                        let operation_realm = code(runtime, caller.code)?.realm;
                        let pending = PendingException {
                            realm: operation_realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: function_not_constructor_message(runtime, function)?,
                            },
                            origin,
                        };
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue 'frames;
                    }
                    let target = bound.target;
                    let materialized = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let mut arguments = Vec::new();
                    arguments
                        .try_reserve_exact(
                            bound
                                .bound_arguments
                                .len()
                                .saturating_add(materialized.arguments.values.len()),
                        )
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: bound
                                .bound_arguments
                                .len()
                                .saturating_add(materialized.arguments.values.len()),
                        })?;
                    for argument in &bound.bound_arguments {
                        arguments.push(argument.duplicate());
                    }
                    arguments.extend(materialized.arguments.into_remaining_values());
                    let new_target = match materialized.new_target {
                        Some(current) if current == function => Some(target),
                        other => other,
                    };
                    let receiver = if new_target.is_some() {
                        materialized.receiver
                    } else {
                        bound.bound_this.duplicate()
                    };
                    inputs = CallInputSource::Prepared(CallInputs {
                        receiver,
                        arguments: CallArguments::from_values(arguments),
                        new_target,
                    });
                    function = target;
                }
                let caller = frames.last().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                let origin = instruction_location(runtime, caller, source_pc)?;
                let operation_realm = code(runtime, caller.code)?.realm;
                if proxy_function {
                    let active_frames = active_execution_frames(frames);
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let inputs = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let dispatch = resolve_native_dispatch(
                        runtime,
                        NativeDispatch::Call(NativeCall {
                            function,
                            receiver: inputs.receiver,
                            arguments: inputs.arguments,
                            return_to: Some(return_to),
                            origin,
                            continuations: Vec::new(),
                            pre_call: None,
                            new_target: inputs.new_target,
                            native_caller: None,
                        }),
                        frames,
                        active_frames,
                        *active_frame_values,
                        compiler,
                        execution_budget,
                    );
                    match dispatch {
                        Ok(NativeDispatch::Immediate(value)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::MissingInstruction {
                                    function: FunctionTemplateId::new(0),
                                    instruction: 0,
                                })?;
                            push_call_result(parent, value, return_to)?;
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Proxy dispatch remained unresolved",
                            }
                            .into());
                        }
                        Ok(
                            NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. },
                        ) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Proxy function produced a structured result",
                            }
                            .into());
                        }
                        Err(
                            NativeFailure::Abrupt(pending)
                            | NativeFailure::AbruptAfterTransient(pending),
                        ) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if let Some(native) = native {
                    if construction && !native.kind.is_constructor() {
                        let pending = PendingException {
                            realm: operation_realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: function_not_constructor_message(runtime, function)?,
                            },
                            origin,
                        };
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue;
                    }
                    let active_frames = active_execution_frames(frames);
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let inputs = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let dispatch = dispatch_native_call_with_frames(
                        runtime,
                        function,
                        native,
                        inputs,
                        Some(return_to),
                        Some(origin),
                        frames,
                        active_frames,
                        *active_frame_values,
                        compiler,
                        execution_budget,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(value)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::MissingInstruction {
                                    function: FunctionTemplateId::new(0),
                                    instruction: 0,
                                })?;
                            push_call_result(parent, value, return_to)?;
                        }
                        Ok(
                            NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. },
                        ) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message:
                                    "native function call returned a structured continuation result",
                            }
                            .into());
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "native dispatch resolver returned an unresolved call",
                            }
                            .into());
                        }
                        Err(NativeFailure::Abrupt(pending)) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::AbruptAfterTransient(pending)) => {
                            let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                message: "transient native throw has no executing frame",
                            })?;
                            frame.transient_cleanup_pending = true;
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if let Some(resolving) = resolving {
                    if construction {
                        let pending = PendingException {
                            realm: operation_realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: function_not_constructor_message(runtime, function)?,
                            },
                            origin,
                        };
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue;
                    }
                    let active_frames = active_execution_frames(frames);
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let inputs = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let dispatch = dispatch_promise_resolving(
                        runtime,
                        &resolving,
                        inputs.arguments,
                        Some(return_to),
                        origin,
                        execution_budget,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(value)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::MissingInstruction {
                                    function: FunctionTemplateId::new(0),
                                    instruction: 0,
                                })?;
                            push_call_result(parent, value, return_to)?;
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise resolving dispatch remained unresolved",
                            }
                            .into());
                        }
                        Ok(
                            NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. },
                        ) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise resolving function produced a structured result",
                            }
                            .into());
                        }
                        Err(
                            NativeFailure::Abrupt(pending)
                            | NativeFailure::AbruptAfterTransient(pending),
                        ) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if let Some(executor) = capability_executor {
                    if construction {
                        let pending = PendingException {
                            realm: operation_realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: function_not_constructor_message(runtime, function)?,
                            },
                            origin,
                        };
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue;
                    }
                    let active_frames = active_execution_frames(frames);
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let inputs = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let dispatch =
                        dispatch_promise_capability_executor(&executor, inputs.arguments, origin);
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(value)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::MissingInstruction {
                                    function: FunctionTemplateId::new(0),
                                    instruction: 0,
                                })?;
                            push_call_result(parent, value, return_to)?;
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise capability executor dispatch remained unresolved",
                            }
                            .into());
                        }
                        Ok(
                            NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. },
                        ) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise capability executor produced a structured result",
                            }
                            .into());
                        }
                        Err(
                            NativeFailure::Abrupt(pending)
                            | NativeFailure::AbruptAfterTransient(pending),
                        ) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if let Some(promise_finally) = promise_finally {
                    if construction {
                        let pending = PendingException {
                            realm: operation_realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: function_not_constructor_message(runtime, function)?,
                            },
                            origin,
                        };
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue;
                    }
                    let active_frames = active_execution_frames(frames);
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let inputs = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let dispatch = dispatch_promise_finally_function(
                        &promise_finally,
                        inputs.arguments,
                        Some(return_to),
                        origin,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(value)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::MissingInstruction {
                                    function: FunctionTemplateId::new(0),
                                    instruction: 0,
                                })?;
                            push_call_result(parent, value, return_to)?;
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise finally dispatch remained unresolved",
                            }
                            .into());
                        }
                        Ok(
                            NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. },
                        ) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise finally function produced a structured result",
                            }
                            .into());
                        }
                        Err(
                            NativeFailure::Abrupt(pending)
                            | NativeFailure::AbruptAfterTransient(pending),
                        ) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if let Some(element) = promise_combinator_element {
                    if construction {
                        let pending = PendingException {
                            realm: operation_realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: function_not_constructor_message(runtime, function)?,
                            },
                            origin,
                        };
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue;
                    }
                    let active_frames = active_execution_frames(frames);
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let inputs = take_call_inputs(
                        frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?,
                        function,
                        inputs,
                    )?;
                    let dispatch = dispatch_promise_combinator_element(
                        runtime,
                        &element,
                        inputs.arguments,
                        Some(return_to),
                        origin,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(value)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::MissingInstruction {
                                    function: FunctionTemplateId::new(0),
                                    instruction: 0,
                                })?;
                            push_call_result(parent, value, return_to)?;
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise combinator element dispatch remained unresolved",
                            }
                            .into());
                        }
                        Ok(
                            NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. },
                        ) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "Promise combinator element produced a structured result",
                            }
                            .into());
                        }
                        Err(
                            NativeFailure::Abrupt(pending)
                            | NativeFailure::AbruptAfterTransient(pending),
                        ) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if construction && !function_is_constructor(runtime, function)? {
                    let pending = PendingException {
                        realm: operation_realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::TypeError,
                            message: function_not_constructor_message(runtime, function)?,
                        },
                        origin,
                    };
                    dispatch_pending_exception(
                        runtime,
                        frames,
                        active_frame_values,
                        pending,
                        compiler,
                        execution_budget,
                    )?;
                    continue;
                }
                if !construction && bytecode_function_is_class_constructor(runtime, function)? {
                    let pending = PendingException {
                        realm: operation_realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::TypeError,
                            message: class_constructor_call_message(runtime, function)?,
                        },
                        origin,
                    };
                    dispatch_pending_exception(
                        runtime,
                        frames,
                        active_frame_values,
                        pending,
                        compiler,
                        execution_budget,
                    )?;
                    continue;
                }
                let supplied_argument_count = inputs.argument_count();
                let plan = plan_frame(
                    runtime,
                    function,
                    active_execution_frames(frames),
                    *active_frame_values,
                    supplied_argument_count,
                    construction,
                )?;
                frames
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    })?;
                let inputs = take_call_inputs(
                    frames.last_mut().ok_or(EngineFault::MissingInstruction {
                        function: FunctionTemplateId::new(0),
                        instruction: 0,
                    })?,
                    function,
                    inputs,
                )?;
                let construction = inputs.new_target;
                let mut child = create_frame(
                    runtime,
                    plan,
                    if construction.is_some() {
                        StoredValue::Undefined
                    } else {
                        inputs.receiver
                    },
                    construction,
                    FrameArguments::Owned(inputs.arguments),
                    Some(return_to),
                    None,
                )?;
                if let Some(new_target) = construction
                    && child.constructor_state != ConstructorState::DerivedUninitialized
                {
                    child.receiver = StoredValue::Object(create_ordinary_constructor_receiver(
                        runtime, new_target,
                    )?);
                    child.constructor_state = ConstructorState::Ordinary;
                }
                *active_frame_values = active_frame_values.saturating_add(child.reserved_values);
                frames.push(child);
            }
            Step::Apply {
                function,
                receiver,
                array_like,
                magic,
                return_to,
                source_pc,
            } => {
                let caller = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                let origin = instruction_location(runtime, caller, source_pc)?;
                let operation_realm = code(runtime, caller.code)?.realm;
                let active_frames = active_execution_frames(frames);
                if frames.try_reserve(1).is_err() {
                    return Err(ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    });
                }
                let dispatch = if magic == 2 {
                    let StoredValue::Function(new_target) = receiver else {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "verified super spread apply did not receive the active new.target",
                        }
                        .into());
                    };
                    begin_array_like_call(
                        runtime,
                        operation_realm,
                        function,
                        StoredValue::Undefined,
                        array_like,
                        Some(return_to),
                        origin,
                        active_frames,
                        *active_frame_values,
                        execution_budget,
                        Some(new_target),
                        None,
                        false,
                    )
                } else {
                    let new_target = (magic & 1 != 0).then_some(function);
                    begin_function_apply(
                        runtime,
                        operation_realm,
                        CallInputs {
                            receiver: StoredValue::Function(function),
                            arguments: CallArguments::from_values(vec![receiver, array_like]),
                            new_target,
                        },
                        Some(return_to),
                        origin,
                        active_frames,
                        *active_frame_values,
                        execution_budget,
                        new_target,
                        None,
                    )
                };
                let dispatch = match dispatch {
                    Ok(dispatch) => dispatch,
                    Err(NativeFailure::Abrupt(pending)) => {
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                        continue;
                    }
                    Err(NativeFailure::AbruptAfterTransient(_)) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "bytecode apply raised a resolver-only transient throw",
                        }
                        .into());
                    }
                    Err(NativeFailure::Execution(error)) => return Err(error),
                };
                let dispatch = resolve_native_dispatch(
                    runtime,
                    dispatch,
                    frames,
                    active_frames,
                    *active_frame_values,
                    compiler,
                    execution_budget,
                );
                match dispatch {
                    Ok(NativeDispatch::Immediate(value)) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        push_call_result(parent, value, return_to)?;
                    }
                    Ok(NativeDispatch::Frame(child)) => {
                        *active_frame_values =
                            active_frame_values.saturating_add(child.reserved_values);
                        frames.push(child);
                    }
                    Ok(NativeDispatch::Call(_)) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "bytecode apply resolver returned an unresolved call",
                        }
                        .into());
                    }
                    Ok(
                        NativeDispatch::Pair(_, _)
                        | NativeDispatch::ForOfRecord { .. }
                        | NativeDispatch::ForOfStep { .. }
                        | NativeDispatch::ForOfClosed
                        | NativeDispatch::CopyDataPropertiesDone
                        | NativeDispatch::AsyncAwait { .. },
                    ) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "bytecode apply produced a structured continuation result",
                        }
                        .into());
                    }
                    Err(NativeFailure::Abrupt(pending)) => {
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                    }
                    Err(NativeFailure::AbruptAfterTransient(pending)) => {
                        let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                            message: "transient apply throw has no executing frame",
                        })?;
                        frame.transient_cleanup_pending = true;
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                    }
                    Err(NativeFailure::Execution(error)) => return Err(error),
                }
            }
            Step::Native {
                dispatch,
                return_to,
            } => {
                let active_frames = active_execution_frames(frames);
                if frames.try_reserve(1).is_err() {
                    if native_dispatch_has_temporary_receiver(&dispatch) {
                        let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                            message: "transient native dispatch has no executing frame",
                        })?;
                        frame.transient_cleanup_pending = true;
                    }
                    return Err(ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    });
                }
                let dispatch = resolve_native_dispatch(
                    runtime,
                    dispatch,
                    frames,
                    active_frames,
                    *active_frame_values,
                    compiler,
                    execution_budget,
                );
                match dispatch {
                    Ok(NativeDispatch::Immediate(value)) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        push_call_result(parent, value, return_to)?;
                    }
                    Ok(NativeDispatch::Pair(original, updated)) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        push_operator_pair(parent, original, updated, return_to)?;
                    }
                    Ok(NativeDispatch::ForOfRecord { iterator, next }) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        push_for_of_record(parent, iterator, next, return_to)?;
                    }
                    Ok(NativeDispatch::ForOfStep {
                        value,
                        done,
                        offset,
                    }) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        finish_for_of_step(parent, value, done, return_to, offset)?;
                    }
                    Ok(NativeDispatch::ForOfClosed) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        finish_for_of_close(parent, return_to)?;
                    }
                    Ok(NativeDispatch::CopyDataPropertiesDone) => {
                        let parent = frames.last_mut().ok_or(EngineFault::MissingInstruction {
                            function: FunctionTemplateId::new(0),
                            instruction: 0,
                        })?;
                        if !matches!(return_to.disposition, ReturnDisposition::Discard) {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "copy-data-properties reached a value-producing continuation",
                            }
                            .into());
                        }
                        parent.instruction = return_to.instruction;
                    }
                    Ok(NativeDispatch::AsyncAwait { promise, origin }) => {
                        match finish_async_suspension(
                            runtime,
                            frames,
                            active_frame_values,
                            promise,
                            origin,
                            compiler,
                            execution_budget,
                        )? {
                            AsyncSuspension::Continued => {}
                            AsyncSuspension::Root(value) => return Ok(value),
                        }
                    }
                    Ok(NativeDispatch::Frame(child)) => {
                        *active_frame_values =
                            active_frame_values.saturating_add(child.reserved_values);
                        frames.push(child);
                    }
                    Ok(NativeDispatch::Call(_)) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "abstract operation resolver returned an unresolved call",
                        }
                        .into());
                    }
                    Err(NativeFailure::Abrupt(pending)) => {
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                    }
                    Err(NativeFailure::AbruptAfterTransient(pending)) => {
                        let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                            message: "transient native throw has no executing frame",
                        })?;
                        frame.transient_cleanup_pending = true;
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                    }
                    Err(NativeFailure::Execution(error)) => return Err(error),
                }
            }
            Step::Abrupt(pending) => {
                // The pending transport exclusively owns a popped thrown
                // value while active frames own the remaining heap edges.
                // Allocate provenance, then immediately publish the escaping
                // root; no collection safe point may be inserted between
                // these operations.
                dispatch_pending_exception(
                    runtime,
                    frames,
                    active_frame_values,
                    pending,
                    compiler,
                    execution_budget,
                )?;
            }
            Step::Await { value, source_pc } => {
                let frame = frames.last().ok_or(EngineFault::RuntimeInvariant {
                    message: "await has no executing async frame",
                })?;
                let origin = instruction_location(runtime, frame, source_pc)?;
                let realm = code(runtime, frame.code)?.realm;
                let return_to = CallReturn::discard(frame.instruction);
                let active_frames = active_execution_frames(frames);
                frames
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    })?;
                let dispatch = begin_async_await(
                    runtime,
                    realm,
                    value,
                    Some(return_to),
                    origin,
                    execution_budget,
                );
                let dispatch = match dispatch {
                    Ok(dispatch) => resolve_native_dispatch(
                        runtime,
                        dispatch,
                        frames,
                        active_frames,
                        *active_frame_values,
                        compiler,
                        execution_budget,
                    ),
                    Err(error) => Err(error),
                };
                match dispatch {
                    Ok(NativeDispatch::AsyncAwait { promise, origin }) => {
                        match finish_async_suspension(
                            runtime,
                            frames,
                            active_frame_values,
                            promise,
                            origin,
                            compiler,
                            execution_budget,
                        )? {
                            AsyncSuspension::Continued => {}
                            AsyncSuspension::Root(value) => return Ok(value),
                        }
                    }
                    Ok(NativeDispatch::Frame(child)) => {
                        *active_frame_values =
                            active_frame_values.saturating_add(child.reserved_values);
                        frames.push(child);
                    }
                    Ok(NativeDispatch::Call(_)) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "await PromiseResolve remained an unresolved call",
                        }
                        .into());
                    }
                    Ok(
                        NativeDispatch::Immediate(_)
                        | NativeDispatch::Pair(_, _)
                        | NativeDispatch::ForOfRecord { .. }
                        | NativeDispatch::ForOfStep { .. }
                        | NativeDispatch::ForOfClosed
                        | NativeDispatch::CopyDataPropertiesDone,
                    ) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "await PromiseResolve produced a non-suspension result",
                        }
                        .into());
                    }
                    Err(NativeFailure::Abrupt(pending)) => {
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                    }
                    Err(NativeFailure::AbruptAfterTransient(pending)) => {
                        let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                            message: "transient await throw has no executing frame",
                        })?;
                        frame.transient_cleanup_pending = true;
                        dispatch_pending_exception(
                            runtime,
                            frames,
                            active_frame_values,
                            pending,
                            compiler,
                            execution_budget,
                        )?;
                    }
                    Err(NativeFailure::Execution(error)) => return Err(error),
                }
            }
            Step::InitialYield => {
                let mut initial = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values = active_frame_values.saturating_sub(initial.reserved_values);
                let return_to = initial.return_to;
                let native_returns = std::mem::take(&mut initial.native_returns);
                initial.reserved_values = initial
                    .reserved_values
                    .saturating_sub(native_continuation_values(&native_returns));
                let function_kind = code(runtime, initial.code)?
                    .authority
                    .function(initial.template)
                    .ok_or(EngineFault::InvalidClosureEnvironment {
                        function: initial.template,
                    })?
                    .function()
                    .control_flow()
                    .function_header()
                    .kind();
                let mut result = if function_kind == FunctionKind::AsyncGenerator {
                    create_async_generator(runtime, initial)?
                } else {
                    create_generator(runtime, initial)?
                };
                if !native_returns.is_empty() {
                    match resume_suspended_native_returns(
                        runtime,
                        frames,
                        active_frame_values,
                        native_returns,
                        result,
                        return_to,
                        compiler,
                        execution_budget,
                    )? {
                        SuspendedNativeReturn::Value(value) => result = value,
                        SuspendedNativeReturn::Continued => continue,
                    }
                }
                if let Some(parent) = frames.last_mut() {
                    push_call_result(
                        parent,
                        result,
                        return_to.ok_or(EngineFault::RuntimeInvariant {
                            message: "nested generator creation has no caller continuation",
                        })?,
                    )?;
                    continue;
                }
                if return_to.is_some() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "host generator creation has a caller continuation",
                    }
                    .into());
                }
                return Ok(result);
            }
            Step::Yield(value) => {
                let mut suspended = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values =
                    active_frame_values.saturating_sub(suspended.reserved_values);
                let generator =
                    suspended
                        .generator_resume
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "yielding frame has no generator resume identity",
                        })?;
                if runtime.async_generator_states.contains_key(&generator) {
                    if suspended.generator_result.is_some() {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "async-generator yield retained a synchronous result object",
                        }
                        .into());
                    }
                    if !suspended.native_returns.is_empty() {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "async-generator yield retained native continuations",
                        }
                        .into());
                    }
                    match finish_async_generator_yield(
                        runtime,
                        generator,
                        suspended,
                        value,
                        AsyncGeneratorLifecycle::SuspendedYield,
                        execution_budget,
                    )
                    .map_err(native_failure_to_execution)?
                    {
                        AsyncGeneratorYieldOutcome::Frame(next) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(next.reserved_values);
                            frames.push(next);
                            continue;
                        }
                        AsyncGeneratorYieldOutcome::Dispatch(dispatch) => {
                            let active_frames = active_execution_frames(frames);
                            let dispatch = resolve_native_dispatch(
                                runtime,
                                dispatch,
                                frames,
                                active_frames,
                                *active_frame_values,
                                compiler,
                                execution_budget,
                            )
                            .map_err(native_failure_to_execution)?;
                            match dispatch {
                                NativeDispatch::Frame(next) => {
                                    *active_frame_values =
                                        active_frame_values.saturating_add(next.reserved_values);
                                    frames.push(next);
                                    continue;
                                }
                                NativeDispatch::Immediate(result) if frames.is_empty() => {
                                    return Ok(result);
                                }
                                NativeDispatch::Immediate(_)
                                | NativeDispatch::Pair(_, _)
                                | NativeDispatch::ForOfRecord { .. }
                                | NativeDispatch::ForOfStep { .. }
                                | NativeDispatch::ForOfClosed
                                | NativeDispatch::CopyDataPropertiesDone
                                | NativeDispatch::AsyncAwait { .. }
                                | NativeDispatch::Call(_) => {
                                    return Err(EngineFault::RuntimeInvariant {
                                        message: "queued async-generator return produced an invalid dispatch",
                                    }
                                    .into());
                                }
                            }
                        }
                        AsyncGeneratorYieldOutcome::Suspended if frames.is_empty() => {
                            return Ok(StoredValue::Undefined);
                        }
                        AsyncGeneratorYieldOutcome::Suspended => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "async-generator yield retained an execution parent",
                            }
                            .into());
                        }
                    }
                }
                let result =
                    suspended
                        .generator_result
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "yielding frame has no reserved iterator result",
                        })?;
                suspended.reserved_values = suspended.reserved_values.saturating_sub(1);
                let return_to = suspended.return_to;
                let native_returns = std::mem::take(&mut suspended.native_returns);
                suspended.reserved_values = suspended
                    .reserved_values
                    .saturating_sub(native_continuation_values(&native_returns));
                runtime.finish_iterator_result(result, value, false)?;
                suspend_generator_frame(
                    runtime,
                    generator,
                    suspended,
                    GeneratorLifecycle::SuspendedYield,
                )?;
                let mut result = StoredValue::Object(result);
                if !native_returns.is_empty() {
                    match resume_suspended_native_returns(
                        runtime,
                        frames,
                        active_frame_values,
                        native_returns,
                        result,
                        return_to,
                        compiler,
                        execution_budget,
                    )? {
                        SuspendedNativeReturn::Value(value) => result = value,
                        SuspendedNativeReturn::Continued => continue,
                    }
                }
                if let Some(parent) = frames.last_mut() {
                    push_call_result(
                        parent,
                        result,
                        return_to.ok_or(EngineFault::RuntimeInvariant {
                            message: "nested generator resume has no caller continuation",
                        })?,
                    )?;
                    continue;
                }
                if return_to.is_some() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "host generator resume has a caller continuation",
                    }
                    .into());
                }
                return Ok(result);
            }
            Step::AsyncYieldStar(value) => {
                let suspended = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values =
                    active_frame_values.saturating_sub(suspended.reserved_values);
                let generator =
                    suspended
                        .generator_resume
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "delegated async-yield frame has no generator resume identity",
                        })?;
                if !runtime.async_generator_states.contains_key(&generator)
                    || suspended.generator_result.is_some()
                    || !suspended.native_returns.is_empty()
                {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "delegated async-yield frame has invalid suspension state",
                    }
                    .into());
                }
                match finish_async_generator_yield(
                    runtime,
                    generator,
                    suspended,
                    value,
                    AsyncGeneratorLifecycle::SuspendedYieldStar,
                    execution_budget,
                )
                .map_err(native_failure_to_execution)?
                {
                    AsyncGeneratorYieldOutcome::Frame(next) => {
                        *active_frame_values =
                            active_frame_values.saturating_add(next.reserved_values);
                        frames.push(next);
                    }
                    AsyncGeneratorYieldOutcome::Dispatch(dispatch) => {
                        let active_frames = active_execution_frames(frames);
                        let dispatch = resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        )
                        .map_err(native_failure_to_execution)?;
                        match dispatch {
                            NativeDispatch::Frame(next) => {
                                *active_frame_values =
                                    active_frame_values.saturating_add(next.reserved_values);
                                frames.push(next);
                            }
                            NativeDispatch::Immediate(result) if frames.is_empty() => {
                                return Ok(result);
                            }
                            NativeDispatch::Immediate(_)
                            | NativeDispatch::Pair(_, _)
                            | NativeDispatch::ForOfRecord { .. }
                            | NativeDispatch::ForOfStep { .. }
                            | NativeDispatch::ForOfClosed
                            | NativeDispatch::CopyDataPropertiesDone
                            | NativeDispatch::AsyncAwait { .. }
                            | NativeDispatch::Call(_) => {
                                return Err(EngineFault::RuntimeInvariant {
                                    message: "queued delegated async-generator return produced an invalid dispatch",
                                }
                                .into());
                            }
                        }
                    }
                    AsyncGeneratorYieldOutcome::Suspended if frames.is_empty() => {
                        return Ok(StoredValue::Undefined);
                    }
                    AsyncGeneratorYieldOutcome::Suspended => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "delegated async-generator yield retained an execution parent",
                        }
                        .into());
                    }
                }
            }
            Step::YieldStar(value) => {
                if !matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "verified delegated yield produced a non-object iterator result",
                    }
                    .into());
                }
                let mut suspended = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values =
                    active_frame_values.saturating_sub(suspended.reserved_values);
                let generator =
                    suspended
                        .generator_resume
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "delegated-yield frame has no generator resume identity",
                        })?;
                let reserved =
                    suspended
                        .generator_result
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "delegated-yield frame has no reserved iterator result",
                        })?;
                runtime.discard_reserved_iterator_result(reserved)?;
                suspended.reserved_values = suspended.reserved_values.saturating_sub(1);
                let return_to = suspended.return_to;
                let native_returns = std::mem::take(&mut suspended.native_returns);
                suspended.reserved_values = suspended
                    .reserved_values
                    .saturating_sub(native_continuation_values(&native_returns));
                suspend_generator_frame(
                    runtime,
                    generator,
                    suspended,
                    GeneratorLifecycle::SuspendedYieldStar,
                )?;
                let mut result = value;
                if !native_returns.is_empty() {
                    match resume_suspended_native_returns(
                        runtime,
                        frames,
                        active_frame_values,
                        native_returns,
                        result,
                        return_to,
                        compiler,
                        execution_budget,
                    )? {
                        SuspendedNativeReturn::Value(value) => result = value,
                        SuspendedNativeReturn::Continued => continue,
                    }
                }
                if let Some(parent) = frames.last_mut() {
                    push_call_result(
                        parent,
                        result,
                        return_to.ok_or(EngineFault::RuntimeInvariant {
                            message: "nested delegated generator resume has no caller continuation",
                        })?,
                    )?;
                    continue;
                }
                if return_to.is_some() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "host delegated generator resume has a caller continuation",
                    }
                    .into());
                }
                return Ok(result);
            }
            Step::Return { value, source_pc } => {
                if let Some(frame) = frames.last()
                    && matches!(
                        frame.constructor_state,
                        ConstructorState::DerivedUninitialized
                            | ConstructorState::DerivedInitialized
                    )
                    && value.heap_reference().is_none()
                    && !(frame.constructor_state == ConstructorState::DerivedInitialized
                        && matches!(value, StoredValue::Undefined))
                {
                    let frame = frames.last().ok_or(EngineFault::MissingInstruction {
                        function: FunctionTemplateId::new(0),
                        instruction: 0,
                    })?;
                    let pending = if matches!(value, StoredValue::Undefined) {
                        derived_this_uninitialized_exception(runtime, frame, source_pc)?
                    } else {
                        derived_constructor_primitive_return_exception(runtime, frame, source_pc)?
                    };
                    dispatch_pending_exception(
                        runtime,
                        frames,
                        active_frame_values,
                        pending,
                        compiler,
                        execution_budget,
                    )?;
                    continue;
                }
                let mut finished = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values = active_frame_values.saturating_sub(finished.reserved_values);
                let return_to = finished.return_to;
                let mut value = if let Some(generator) = finished.generator_resume
                    && runtime.async_generator_states.contains_key(&generator)
                {
                    if finished.generator_result.is_some() {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "async-generator return retained a synchronous result object",
                        }
                        .into());
                    }
                    let dispatch =
                        finish_async_generator_return(runtime, generator, value, execution_budget)
                            .map_err(native_failure_to_execution)?;
                    let active_frames = active_execution_frames(frames);
                    match resolve_native_dispatch(
                        runtime,
                        dispatch,
                        frames,
                        active_frames,
                        *active_frame_values,
                        compiler,
                        execution_budget,
                    )
                    .map_err(native_failure_to_execution)?
                    {
                        NativeDispatch::Immediate(value) => value,
                        NativeDispatch::Frame(next) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(next.reserved_values);
                            frames.push(next);
                            continue;
                        }
                        NativeDispatch::Pair(_, _)
                        | NativeDispatch::ForOfRecord { .. }
                        | NativeDispatch::ForOfStep { .. }
                        | NativeDispatch::ForOfClosed
                        | NativeDispatch::CopyDataPropertiesDone
                        | NativeDispatch::AsyncAwait { .. }
                        | NativeDispatch::Call(_) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "async-generator completion produced an invalid dispatch",
                            }
                            .into());
                        }
                    }
                } else if let Some(generator) = finished.generator_resume {
                    let result = finished.generator_result.take().ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "returning generator frame has no reserved iterator result",
                        },
                    )?;
                    runtime.finish_iterator_result(result, value, true)?;
                    complete_generator_resume(runtime, generator)?;
                    StoredValue::Object(result)
                } else if let Some(dynamic) = finished.dynamic_return.take() {
                    match finish_dynamic_function_return(runtime, dynamic, value)? {
                        DynamicFunctionCompletion::Value(value) => value,
                        DynamicFunctionCompletion::Abrupt(pending) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                            continue;
                        }
                    }
                } else if finished.constructor_state == ConstructorState::Ordinary {
                    match value {
                        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
                        StoredValue::Undefined
                        | StoredValue::Null
                        | StoredValue::Boolean(_)
                        | StoredValue::Number(_)
                        | StoredValue::BigInt(_)
                        | StoredValue::String(_)
                        | StoredValue::Symbol(_) => finished.receiver,
                    }
                } else if finished.constructor_state == ConstructorState::DerivedInitialized {
                    match value {
                        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
                        StoredValue::Undefined => finished.receiver,
                        StoredValue::Null
                        | StoredValue::Boolean(_)
                        | StoredValue::Number(_)
                        | StoredValue::BigInt(_)
                        | StoredValue::String(_)
                        | StoredValue::Symbol(_) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "validated derived constructor returned an invalid primitive",
                            }
                            .into());
                        }
                    }
                } else {
                    value
                };
                let native_returns = std::mem::take(&mut finished.native_returns);
                if !native_returns.is_empty() {
                    if frames.try_reserve(1).is_err() {
                        if native_continuations_have_temporary_receiver(&native_returns) {
                            if let Some(frame) = frames.last_mut() {
                                frame.transient_cleanup_pending = true;
                            } else if runtime.collection_pending {
                                let _ = runtime.collect_cycles();
                            }
                        }
                        return Err(ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        });
                    }
                    let active_frames = active_execution_frames(frames);
                    let dispatch = resume_native_continuations(
                        runtime,
                        native_returns,
                        value,
                        return_to,
                        frames,
                        active_frames,
                        *active_frame_values,
                        compiler,
                        execution_budget,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            frames,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            execution_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(completion)) => value = completion,
                        Ok(NativeDispatch::Pair(original, updated)) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                    message: "operator continuation has no executing frame",
                                })?;
                            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                                message: "operator continuation has no caller continuation",
                            })?;
                            push_operator_pair(parent, original, updated, return_to)?;
                            continue;
                        }
                        Ok(NativeDispatch::ForOfRecord { iterator, next }) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                    message: "for-of start continuation has no executing frame",
                                })?;
                            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                                message: "for-of start continuation has no caller continuation",
                            })?;
                            push_for_of_record(parent, iterator, next, return_to)?;
                            continue;
                        }
                        Ok(NativeDispatch::ForOfStep {
                            value,
                            done,
                            offset,
                        }) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                    message: "for-of step continuation has no executing frame",
                                })?;
                            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                                message: "for-of step continuation has no caller continuation",
                            })?;
                            finish_for_of_step(parent, value, done, return_to, offset)?;
                            continue;
                        }
                        Ok(NativeDispatch::ForOfClosed) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                    message: "for-of close continuation has no executing frame",
                                })?;
                            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                                message: "for-of close continuation has no caller continuation",
                            })?;
                            finish_for_of_close(parent, return_to)?;
                            continue;
                        }
                        Ok(NativeDispatch::CopyDataPropertiesDone) => {
                            let parent =
                                frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                    message: "copy-data-properties continuation has no executing frame",
                                })?;
                            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                                message: "copy-data-properties continuation has no caller continuation",
                            })?;
                            if !matches!(return_to.disposition, ReturnDisposition::Discard) {
                                return Err(EngineFault::RuntimeInvariant {
                                    message: "copy-data-properties reached a value-producing continuation",
                                }
                                .into());
                            }
                            parent.instruction = return_to.instruction;
                            continue;
                        }
                        Ok(NativeDispatch::AsyncAwait { promise, origin }) => {
                            match finish_async_suspension(
                                runtime,
                                frames,
                                active_frame_values,
                                promise,
                                origin,
                                compiler,
                                execution_budget,
                            )? {
                                AsyncSuspension::Continued => continue,
                                AsyncSuspension::Root(value) => return Ok(value),
                            }
                        }
                        Ok(NativeDispatch::Frame(child)) => {
                            *active_frame_values =
                                active_frame_values.saturating_add(child.reserved_values);
                            frames.push(child);
                            continue;
                        }
                        Ok(NativeDispatch::Call(_)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "native continuation resolver returned an unresolved call",
                            }
                            .into());
                        }
                        Err(NativeFailure::Abrupt(pending)) => {
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                            continue;
                        }
                        Err(NativeFailure::AbruptAfterTransient(pending)) => {
                            let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                message: "transient native throw has no executing frame",
                            })?;
                            frame.transient_cleanup_pending = true;
                            dispatch_pending_exception(
                                runtime,
                                frames,
                                active_frame_values,
                                pending,
                                compiler,
                                execution_budget,
                            )?;
                            continue;
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                }
                if let Some(parent) = frames.last_mut() {
                    let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                        message: "nested frame has no caller continuation",
                    })?;
                    push_call_result(parent, value, return_to)?;
                    continue;
                }
                if return_to.is_some() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "host frame has a caller continuation",
                    }
                    .into());
                }
                return Ok(value);
            }
        }
    }
}

enum SuspendedNativeReturn {
    Value(StoredValue),
    Continued,
}

enum AsyncSuspension {
    Continued,
    Root(StoredValue),
}

fn native_failure_to_execution(failure: NativeFailure) -> ExecutionError {
    match failure {
        NativeFailure::Execution(error) => error,
        NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
            EngineFault::RuntimeInvariant {
                message: "internal async-generator settlement threw JavaScript",
            }
            .into()
        }
    }
}

fn finish_async_suspension(
    runtime: &mut Runtime,
    frames: &mut Vec<Frame>,
    active_frame_values: &mut u64,
    promise: ObjectId,
    origin: JsStackFrame,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<AsyncSuspension, ExecutionError> {
    let frame = frames.pop().ok_or(EngineFault::RuntimeInvariant {
        message: "async suspension lost its executing frame",
    })?;
    *active_frame_values = active_frame_values.saturating_sub(frame.reserved_values);
    if let Some(generator) = frame.generator_resume
        && runtime.async_generator_states.contains_key(&generator)
    {
        let (result, return_to) =
            suspend_async_generator_await(runtime, generator, frame, promise, origin)?;
        if let Some(parent) = frames.last_mut() {
            push_call_result(
                parent,
                result,
                return_to.ok_or(EngineFault::RuntimeInvariant {
                    message: "nested async-generator request has no caller continuation",
                })?,
            )?;
            return Ok(AsyncSuspension::Continued);
        }
        if return_to.is_some() {
            return Err(EngineFault::RuntimeInvariant {
                message: "host async-generator request retained a caller continuation",
            }
            .into());
        }
        return Ok(AsyncSuspension::Root(result));
    }
    let (mut result, outer, return_to) = suspend_async_function(runtime, frame, promise, origin)?;
    if !outer.is_empty() {
        match resume_suspended_native_returns(
            runtime,
            frames,
            active_frame_values,
            outer,
            result,
            return_to,
            compiler,
            execution_budget,
        )? {
            SuspendedNativeReturn::Value(value) => result = value,
            SuspendedNativeReturn::Continued => return Ok(AsyncSuspension::Continued),
        }
    }
    if let Some(parent) = frames.last_mut() {
        push_call_result(
            parent,
            result,
            return_to.ok_or(EngineFault::RuntimeInvariant {
                message: "nested async call has no caller continuation",
            })?,
        )?;
        return Ok(AsyncSuspension::Continued);
    }
    if return_to.is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "host async call retained a caller continuation",
        }
        .into());
    }
    Ok(AsyncSuspension::Root(result))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "a yielded generator can resume the same native continuation graph as a returning bytecode frame"
)]
fn resume_suspended_native_returns(
    runtime: &mut Runtime,
    frames: &mut Vec<Frame>,
    active_frame_values: &mut u64,
    native_returns: Vec<NativeContinuation>,
    value: StoredValue,
    return_to: Option<CallReturn>,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<SuspendedNativeReturn, ExecutionError> {
    if frames.try_reserve(1).is_err() {
        if native_continuations_have_temporary_receiver(&native_returns) {
            if let Some(frame) = frames.last_mut() {
                frame.transient_cleanup_pending = true;
            } else if runtime.collection_pending {
                let _ = runtime.collect_cycles();
            }
        }
        return Err(ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        });
    }
    let active_frames = active_execution_frames(frames);
    let dispatch = resume_native_continuations(
        runtime,
        native_returns,
        value,
        return_to,
        frames,
        active_frames,
        *active_frame_values,
        compiler,
        execution_budget,
    );
    let dispatch = match dispatch {
        Ok(dispatch) => resolve_native_dispatch(
            runtime,
            dispatch,
            frames,
            active_frames,
            *active_frame_values,
            compiler,
            execution_budget,
        ),
        Err(error) => Err(error),
    };
    match dispatch {
        Ok(NativeDispatch::Immediate(value)) => Ok(SuspendedNativeReturn::Value(value)),
        Ok(NativeDispatch::Pair(original, updated)) => {
            let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                message: "operator continuation has no executing frame",
            })?;
            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                message: "operator continuation has no caller continuation",
            })?;
            push_operator_pair(parent, original, updated, return_to)?;
            Ok(SuspendedNativeReturn::Continued)
        }
        Ok(NativeDispatch::ForOfRecord { iterator, next }) => {
            let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                message: "for-of start continuation has no executing frame",
            })?;
            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                message: "for-of start continuation has no caller continuation",
            })?;
            push_for_of_record(parent, iterator, next, return_to)?;
            Ok(SuspendedNativeReturn::Continued)
        }
        Ok(NativeDispatch::ForOfStep {
            value,
            done,
            offset,
        }) => {
            let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                message: "for-of step continuation has no executing frame",
            })?;
            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                message: "for-of step continuation has no caller continuation",
            })?;
            finish_for_of_step(parent, value, done, return_to, offset)?;
            Ok(SuspendedNativeReturn::Continued)
        }
        Ok(NativeDispatch::ForOfClosed) => {
            let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                message: "for-of close continuation has no executing frame",
            })?;
            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                message: "for-of close continuation has no caller continuation",
            })?;
            finish_for_of_close(parent, return_to)?;
            Ok(SuspendedNativeReturn::Continued)
        }
        Ok(NativeDispatch::CopyDataPropertiesDone) => {
            let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                message: "copy-data-properties continuation has no executing frame",
            })?;
            let return_to = return_to.ok_or(EngineFault::RuntimeInvariant {
                message: "copy-data-properties continuation has no caller continuation",
            })?;
            if !matches!(return_to.disposition, ReturnDisposition::Discard) {
                return Err(EngineFault::RuntimeInvariant {
                    message: "copy-data-properties reached a value-producing continuation",
                }
                .into());
            }
            parent.instruction = return_to.instruction;
            Ok(SuspendedNativeReturn::Continued)
        }
        Ok(NativeDispatch::AsyncAwait { promise, origin }) => Ok(
            match finish_async_suspension(
                runtime,
                frames,
                active_frame_values,
                promise,
                origin,
                compiler,
                execution_budget,
            )? {
                AsyncSuspension::Continued => SuspendedNativeReturn::Continued,
                AsyncSuspension::Root(value) => SuspendedNativeReturn::Value(value),
            },
        ),
        Ok(NativeDispatch::Frame(child)) => {
            *active_frame_values = active_frame_values.saturating_add(child.reserved_values);
            frames.push(child);
            Ok(SuspendedNativeReturn::Continued)
        }
        Ok(NativeDispatch::Call(_)) => Err(EngineFault::RuntimeInvariant {
            message: "native continuation resolver returned an unresolved call",
        }
        .into()),
        Err(NativeFailure::Abrupt(pending)) => {
            dispatch_pending_exception(
                runtime,
                frames,
                active_frame_values,
                pending,
                compiler,
                execution_budget,
            )?;
            Ok(SuspendedNativeReturn::Continued)
        }
        Err(NativeFailure::AbruptAfterTransient(pending)) => {
            let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                message: "transient native throw has no executing frame",
            })?;
            frame.transient_cleanup_pending = true;
            dispatch_pending_exception(
                runtime,
                frames,
                active_frame_values,
                pending,
                compiler,
                execution_budget,
            )?;
            Ok(SuspendedNativeReturn::Continued)
        }
        Err(NativeFailure::Execution(error)) => Err(error),
    }
}

fn push_call_result(
    parent: &mut Frame,
    value: StoredValue,
    return_to: CallReturn,
) -> Result<(), ExecutionError> {
    match return_to.disposition {
        ReturnDisposition::Push => {
            if parent.stack.len() == parent.stack.capacity() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified call result exceeds frame stack capacity",
                }
                .into());
            }
            push(parent, value);
        }
        ReturnDisposition::Discard => {}
        ReturnDisposition::InitializeDerivedThis => {
            if parent.constructor_state != ConstructorState::DerivedUninitialized {
                return Err(EngineFault::RuntimeInvariant {
                    message: "derived constructor completion reached an invalid parent frame",
                }
                .into());
            }
            if value.heap_reference().is_none() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "superclass construction completed without an object receiver",
                }
                .into());
            }
            if parent.stack.len() == parent.stack.capacity() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified derived constructor result exceeds frame stack capacity",
                }
                .into());
            }
            parent.receiver = value.duplicate();
            parent.constructor_state = ConstructorState::DerivedInitialized;
            push(parent, value);
        }
    }
    parent.instruction = return_to.instruction;
    Ok(())
}

fn push_operator_pair(
    parent: &mut Frame,
    original: StoredValue,
    updated: StoredValue,
    return_to: CallReturn,
) -> Result<(), ExecutionError> {
    if !matches!(return_to.disposition, ReturnDisposition::Push) {
        return Err(EngineFault::RuntimeInvariant {
            message: "postfix operator pair reached a discarding continuation",
        }
        .into());
    }
    if parent.stack.capacity().saturating_sub(parent.stack.len()) < 2 {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified postfix operator result exceeds frame stack capacity",
        }
        .into());
    }
    push(parent, original);
    push(parent, updated);
    parent.instruction = return_to.instruction;
    Ok(())
}

fn native_step(
    result: Result<NativeDispatch, NativeFailure>,
    return_to: CallReturn,
) -> Result<Step, ExecutionError> {
    match result {
        Ok(dispatch) => Ok(Step::Native {
            dispatch,
            return_to,
        }),
        Err(NativeFailure::Abrupt(pending)) => Ok(Step::Abrupt(pending)),
        Err(NativeFailure::AbruptAfterTransient(_)) => Err(EngineFault::RuntimeInvariant {
            message: "unresolved native step returned a resolver-only transient throw",
        }
        .into()),
        Err(NativeFailure::Execution(error)) => Err(error),
    }
}

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;
