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

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{
    BytecodePc, CompilerBindingKind, CompilerClosureBinding, CompilerClosureSource,
    CompilerExecutableKind, FinalOpcode, FunctionTemplateId, InstructionIndex, Operands,
    SourceByteSpan, VerifiedBytecodeFunction, VerifiedSuccessorKind,
};

use crate::{
    ArrayIndex, Context, DynamicFunctionCompileFailure, EngineFault, ExceptionKind, ExecutionError,
    Function, HandleError, HandleKind, JsException, JsNumber, JsStackFrame, JsString,
    JsStringError, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    PredefinedAtom, PropertyKey, PropertyLayout, Runtime, RuntimeError, RuntimeResource,
    conversion::{number_to_int32, number_to_uint32, string_to_number},
    ids::{BindingCellId, FunctionId, InstalledCodeId, ObjectId, RealmGlobalBindingId, RealmId},
    object::{ForInSnapshot, OwnProperty},
    runtime::{
        ArrayDefineOutcome, ArrayLengthWriteOutcome, BindingCell, BoundFunction, BytecodeFunction,
        CollectionRoot, EnvironmentBinding, ForInAdvance, FrameBindingAddress,
        FunctionImplementation, HeapFunction, InstalledCode, InstalledConstant, InstalledRoot,
        InstalledTemplate, NativeFunction, NativeFunctionKind, PreparedIteratorResultPlan,
        RealmGlobalBindingState, array_length_from_number, check_execution_limit,
        global_declaration_error, usize_to_u64,
    },
    value::{HeapReference, SlotValue, StoredValue},
};

mod aggregate_error;
mod bindings;
mod conversions;
mod dynamic;
mod error_stack;
mod errors;
mod exceptions;
mod execution;
mod instanceof;
mod iterators;
mod native;
mod properties;
mod stack;

#[allow(
    clippy::wildcard_imports,
    reason = "private VM sibling modules share one interpreter implementation namespace"
)]
use {
    aggregate_error::*, bindings::*, conversions::*, dynamic::*, error_stack::*, errors::*,
    exceptions::*, execution::*, iterators::*, native::*, properties::*, stack::*,
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

    /// Replaces the maximum ordinary dynamic-Function compilations in one
    /// interpreter session.
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
    instruction_limit: u64,
    executed_instructions: u64,
    compilation_limit: u64,
    source_code_unit_limit: u64,
    compilations: u64,
    source_code_units: u64,
}

impl ExecutionBudget {
    const fn new(limits: ExecutionLimits) -> Self {
        Self {
            instruction_limit: limits.instruction_fuel,
            executed_instructions: 0,
            compilation_limit: limits.dynamic_compilations,
            source_code_unit_limit: limits.dynamic_source_code_units,
            compilations: 0,
            source_code_units: 0,
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

struct Frame {
    function: FunctionId,
    code: InstalledCodeId,
    template: FunctionTemplateId,
    strict: bool,
    receiver: StoredValue,
    instruction: InstructionIndex,
    return_to: Option<CallReturn>,
    dynamic_return: Option<DynamicFunctionReturn>,
    native_returns: Vec<NativeContinuation>,
    transient_cleanup_pending: bool,
    ordinary_constructor: bool,
    native_caller: Option<SyntheticNativeFrame>,
    reserved_values: u64,
    arguments: Vec<FrameBinding>,
    locals: Vec<FrameBinding>,
    own_cells: Vec<Option<BindingCellId>>,
    own_cell_bindings: Vec<FrameBindingAddress>,
    environment: Vec<EnvironmentBinding>,
    stack: Vec<OperandStackEntry>,
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
    construction: Option<FunctionId>,
    origin: Option<JsStackFrame>,
}

enum NativeContinuation {
    FunctionSource(FunctionSourceContinuation),
    FunctionApply(FunctionApplyContinuation),
    FunctionBind(FunctionBindContinuation),
    PropertyKey(PropertyKeyContinuation),
    OperatorPrimitive(OperatorPrimitiveContinuation),
    IntrinsicGet(IntrinsicGetContinuation),
    AggregateError(AggregateErrorContinuation),
    ErrorConstructor(ErrorConstructorContinuation),
    ErrorToString(ErrorToStringContinuation),
    ArrayIteratorNext(ArrayIteratorNextContinuation),
    ForOfStart(ForOfStartContinuation),
    ForOfNext(ForOfNextContinuation),
    ForOfClose(ForOfCloseContinuation),
    IteratorAppend(IteratorAppendContinuation),
    IteratorClose(IteratorCloseContinuation),
    CopyDataProperties(CopyDataPropertiesContinuation),
    InstanceOf(InstanceOfContinuation),
    FunctionCall,
}

impl NativeContinuation {
    fn retained_values(&self) -> u64 {
        match self {
            Self::FunctionSource(state) => usize_to_u64(state.arguments.len())
                .saturating_add(u64::from(state.construction.is_some())),
            Self::FunctionApply(state) => state.retained_values(),
            Self::FunctionBind(state) => state.retained_values(),
            Self::PropertyKey(state) => state.retained_values(),
            Self::OperatorPrimitive(state) => state.retained_values(),
            Self::IntrinsicGet(state) => state.retained_values(),
            Self::AggregateError(state) => state.retained_values(),
            Self::ErrorConstructor(state) => state.retained_values(),
            Self::ErrorToString(state) => state.retained_values(),
            Self::ArrayIteratorNext(state) => state.retained_values(),
            Self::ForOfStart(state) => state.retained_values(),
            Self::ForOfNext(state) => state.retained_values(),
            Self::ForOfClose(state) => state.retained_values(),
            Self::IteratorAppend(state) => state.retained_values(),
            Self::IteratorClose(state) => state.retained_values(),
            Self::CopyDataProperties(state) => state.retained_values(),
            Self::InstanceOf(state) => state.retained_values(),
            Self::FunctionCall => 0,
        }
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
            | Self::StringConstructor { .. } => 1,
            Self::ArrayConstructor { arguments, .. } => {
                1_u64.saturating_add(usize_to_u64(arguments.len()))
            }
            Self::ObjectPrototypeToString {
                temporary_receiver, ..
            } => u64::from(temporary_receiver.is_some()),
        }
    }
}

#[derive(Clone, Copy)]
enum ObjectPrototypeTag {
    Array,
    Boolean,
    Error,
    Function,
    Number,
    Object,
    String,
    Symbol,
}

impl ObjectPrototypeTag {
    const fn name(self) -> &'static str {
        match self {
            Self::Array => "Array",
            Self::Boolean => "Boolean",
            Self::Error => "Error",
            Self::Function => "Function",
            Self::Number => "Number",
            Self::Object => "Object",
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
    Iterator,
    NextMethod,
}

struct ForOfStartContinuation {
    iterable: StoredValue,
    iterator: Option<StoredValue>,
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
    Next,
    ReadValue,
}

struct CopyDataPropertiesContinuation {
    target: StoredValue,
    source: StoredValue,
    excluded: Option<StoredValue>,
    snapshot: ForInSnapshot,
    next: usize,
    current_key: Option<PropertyKey>,
    realm: RealmId,
    stage: CopyDataPropertiesStage,
    origin: JsStackFrame,
}

impl CopyDataPropertiesContinuation {
    fn retained_values(&self) -> u64 {
        3_u64.saturating_add(u64::from(self.current_key.is_some()))
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

enum PrimitiveConversionPropertyLookup {
    Value(StoredValue),
    Getter(FunctionId),
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
}

impl PropertyKeyTarget {
    const fn retained_values(&self) -> u64 {
        match self {
            Self::ToKey => 0,
            Self::Read { .. } => 1,
            Self::Write { .. } | Self::DefineMethod { .. } => 2,
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
enum FunctionBindStage {
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
}

struct InstanceOfContinuation {
    value: StoredValue,
    target: StoredValue,
    realm: RealmId,
    stage: InstanceOfStage,
    origin: JsStackFrame,
}

struct ArrayLengthWriteState {
    base: StoredValue,
    name: JsString,
    strict: bool,
    original: Option<StoredValue>,
    first_length: Option<u32>,
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
    #[allow(
        clippy::unused_self,
        reason = "the continuation keeps one uniform retained-values shape for every suspended value pair"
    )]
    fn retained_values(&self) -> u64 {
        2
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
    NumberToString {
        number: JsNumber,
    },
    StringIntrinsic {
        new_target: Option<FunctionId>,
    },
    SymbolIntrinsic {
        global_registry: bool,
    },
    StringIteratorIntrinsic,
    ErrorConstructorMessage(ErrorConstructorContinuation),
    ErrorToStringName(ErrorToStringContinuation),
    ErrorToStringMessage(ErrorToStringContinuation),
    ArrayIteratorLength(ArrayIteratorNextContinuation),
    FunctionApplyLength(FunctionApplyContinuation),
    ArrayLengthWrite(ArrayLengthWriteState),
}

impl OperatorPrimitiveTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::Unary { .. }
            | Self::NumberIntrinsic { new_target: None }
            | Self::StringIntrinsic { new_target: None }
            | Self::SymbolIntrinsic { .. }
            | Self::StringIteratorIntrinsic => 0,
            Self::BinaryRight { .. }
            | Self::BinaryFinish { .. }
            | Self::EqualityFinish { .. }
            | Self::NumberToString { .. }
            | Self::NumberIntrinsic {
                new_target: Some(_),
            }
            | Self::StringIntrinsic {
                new_target: Some(_),
            } => 1,
            Self::ErrorConstructorMessage(state) => state.retained_values(),
            Self::ErrorToStringName(state) | Self::ErrorToStringMessage(state) => {
                state.retained_values()
            }
            Self::ArrayIteratorLength(state) => state.retained_values(),
            Self::FunctionApplyLength(state) => state.retained_values(),
            Self::ArrayLengthWrite(state) => {
                1_u64.saturating_add(u64::from(state.original.is_some()))
            }
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

    #[cfg(test)]
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
        PropertyKeyTarget::Read { base, .. } => trace_stored_value_root(base, mark),
        PropertyKeyTarget::Write { base, value, .. } => {
            trace_stored_value_root(base, mark);
            trace_stored_value_root(value, mark);
        }
        PropertyKeyTarget::DefineMethod { base, function, .. } => {
            trace_stored_value_root(base, mark);
            trace_stored_value_root(function, mark);
        }
    }
}

fn trace_operator_primitive_target_roots(
    target: &OperatorPrimitiveTarget,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    match target {
        OperatorPrimitiveTarget::Unary { .. }
        | OperatorPrimitiveTarget::NumberToString { .. }
        | OperatorPrimitiveTarget::SymbolIntrinsic { .. }
        | OperatorPrimitiveTarget::StringIteratorIntrinsic => {}
        OperatorPrimitiveTarget::BinaryRight { right, .. } => {
            trace_stored_value_root(right, mark);
        }
        OperatorPrimitiveTarget::BinaryFinish { left, .. } => {
            trace_stored_value_root(left, mark);
        }
        OperatorPrimitiveTarget::EqualityFinish { other, .. } => {
            trace_stored_value_root(other, mark);
        }
        OperatorPrimitiveTarget::NumberIntrinsic { new_target }
        | OperatorPrimitiveTarget::StringIntrinsic { new_target } => {
            if let Some(new_target) = new_target {
                mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
            }
        }
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
        NativeContinuation::IntrinsicGet(state) => match state {
            IntrinsicGetContinuation::BooleanConstructor { new_target, .. }
            | IntrinsicGetContinuation::NumberConstructor { new_target, .. }
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
            IntrinsicGetContinuation::ObjectPrototypeToString {
                temporary_receiver, ..
            } => {
                if let Some(receiver) = temporary_receiver {
                    mark(CollectionRoot::Heap(HeapReference::Object(*receiver)));
                }
            }
        },
        NativeContinuation::AggregateError(state) => state.trace_roots(mark),
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
        NativeContinuation::CopyDataProperties(state) => {
            trace_stored_value_root(&state.target, mark);
            trace_stored_value_root(&state.source, mark);
            if let Some(excluded) = &state.excluded {
                trace_stored_value_root(excluded, mark);
            }
        }
        NativeContinuation::InstanceOf(state) => {
            trace_instance_of_roots(state, mark);
        }
        NativeContinuation::FunctionCall => {}
    }
}

fn trace_frame_roots(frame: &Frame, mark: &mut dyn FnMut(CollectionRoot)) {
    mark(CollectionRoot::Heap(HeapReference::Function(
        frame.function,
    )));
    trace_stored_value_root(&frame.receiver, mark);
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
        | NativeDispatch::CopyDataPropertiesDone => false,
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
    strict: bool,
    receiver_access: ReceiverAccess,
    instruction: InstructionIndex,
}

#[derive(Clone, Copy)]
enum ReceiverAccess {
    Direct,
    NormalizeSloppy,
}

impl ReceiverAccess {
    const fn for_function(strict: bool, executable_kind: CompilerExecutableKind) -> Self {
        if strict
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
}

#[derive(Clone, Copy)]
enum ReturnDisposition {
    Push,
    Discard,
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
    Return(StoredValue),
}

enum PendingExceptionPayload {
    EngineError {
        kind: ExceptionKind,
        message: JsString,
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

    /// Invokes a runtime function with an immutable ordinary dynamic-Function
    /// compiler available to nested `%Function%` calls.
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
                let value = execute_native_entry(
                    self.runtime,
                    function_id,
                    native,
                    materialized,
                    limits,
                    compiler,
                )?;
                return self.runtime.public_value(value);
            }
            let Some(bound) = node.bound() else {
                break;
            };
            let mut merged = Vec::new();
            merged
                .try_reserve_exact(bound.bound_arguments.len().saturating_add(arguments.len()))
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: bound.bound_arguments.len().saturating_add(arguments.len()),
                })?;
            for argument in &bound.bound_arguments {
                merged.push(argument.duplicate());
            }
            for argument in arguments {
                merged.push(argument.stored()?.duplicate());
            }
            owned_arguments = Some(merged);
            receiver = bound.bound_this.duplicate();
            function_id = bound.target;
        }

        let plan = plan_frame(self.runtime, function_id, 0, 0)?;
        let frame = create_frame(
            self.runtime,
            plan,
            receiver,
            match owned_arguments {
                Some(arguments) => FrameArguments::Owned(CallArguments::from_values(arguments)),
                None => FrameArguments::Public(arguments),
            },
            None,
            None,
        )?;
        let value = execute_frames(self.runtime, frame, limits, compiler, None)?;
        self.runtime.public_value(value)
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
        let plan = plan_frame(self.runtime, root.function, 0, 0)?;
        let frame = create_frame(
            self.runtime,
            plan,
            receiver,
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
        execution_budget.charge_instructions(1)?;
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
                            | NativeDispatch::CopyDataPropertiesDone,
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
                let plan = plan_frame(
                    runtime,
                    function,
                    active_execution_frames(frames),
                    *active_frame_values,
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
                    FrameArguments::Owned(inputs.arguments),
                    Some(return_to),
                    None,
                )?;
                if let Some(new_target) = construction {
                    child.receiver = StoredValue::Object(create_ordinary_constructor_receiver(
                        runtime, new_target,
                    )?);
                    child.ordinary_constructor = true;
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
                let new_target = if magic & 1 != 0 { Some(function) } else { None };
                let inputs = CallInputs {
                    receiver: StoredValue::Function(function),
                    arguments: CallArguments::from_values(vec![receiver, array_like]),
                    new_target,
                };
                if frames.try_reserve(1).is_err() {
                    return Err(ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    });
                }
                let dispatch = begin_function_apply(
                    runtime,
                    operation_realm,
                    inputs,
                    Some(return_to),
                    origin,
                    active_frames,
                    *active_frame_values,
                    execution_budget,
                    new_target,
                    None,
                );
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
                        | NativeDispatch::CopyDataPropertiesDone,
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
            Step::Return(value) => {
                let mut finished = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values = active_frame_values.saturating_sub(finished.reserved_values);
                let return_to = finished.return_to;
                let mut value = if let Some(dynamic) = finished.dynamic_return.take() {
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
                } else if finished.ordinary_constructor {
                    match value {
                        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
                        StoredValue::Undefined
                        | StoredValue::Null
                        | StoredValue::Boolean(_)
                        | StoredValue::Number(_)
                        | StoredValue::String(_)
                        | StoredValue::Symbol(_) => finished.receiver,
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

fn push_call_result(
    parent: &mut Frame,
    value: StoredValue,
    return_to: CallReturn,
) -> Result<(), ExecutionError> {
    if matches!(return_to.disposition, ReturnDisposition::Push) {
        if parent.stack.len() == parent.stack.capacity() {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified call result exceeds frame stack capacity",
            }
            .into());
        }
        push(parent, value);
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
