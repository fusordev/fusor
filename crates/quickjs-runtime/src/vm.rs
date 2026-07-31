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
    object::OwnProperty,
    runtime::{
        BindingCell, BytecodeFunction, CollectionRoot, EnvironmentBinding, ForInAdvance,
        FrameBindingAddress, FunctionImplementation, HeapFunction, InstalledCode,
        InstalledConstant, InstalledRoot, InstalledTemplate, NativeFunction, NativeFunctionKind,
        RealmGlobalBindingState, check_execution_limit, global_declaration_error, usize_to_u64,
    },
    value::{HeapReference, SlotValue, StoredValue},
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
    reserved_values: u64,
    arguments: Vec<FrameBinding>,
    locals: Vec<FrameBinding>,
    own_cells: Vec<Option<BindingCellId>>,
    own_cell_bindings: Vec<FrameBindingAddress>,
    environment: Vec<EnvironmentBinding>,
    stack: Vec<StoredValue>,
}

struct DynamicFunctionReturn {
    root: InstalledRoot,
    construction: Option<FunctionId>,
    origin: Option<JsStackFrame>,
}

enum NativeContinuation {
    FunctionSource(FunctionSourceContinuation),
    FunctionApply(FunctionApplyContinuation),
    PropertyKey(PropertyKeyContinuation),
    OperatorPrimitive(OperatorPrimitiveContinuation),
    IntrinsicGet(IntrinsicGetContinuation),
    FunctionCall,
}

impl NativeContinuation {
    fn retained_values(&self) -> u64 {
        match self {
            Self::FunctionSource(state) => usize_to_u64(state.arguments.len())
                .saturating_add(u64::from(state.construction.is_some())),
            Self::FunctionApply(state) => state.retained_values(),
            Self::PropertyKey(state) => state.retained_values(),
            Self::OperatorPrimitive(state) => state.retained_values(),
            Self::IntrinsicGet(state) => state.retained_values(),
            Self::FunctionCall => 0,
        }
    }
}

#[derive(Clone)]
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
    ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag,
        temporary_receiver: Option<ObjectId>,
    },
}

impl IntrinsicGetContinuation {
    const fn retained_values(&self) -> u64 {
        match self {
            Self::BooleanConstructor { .. }
            | Self::NumberConstructor { .. }
            | Self::StringConstructor { .. } => 1,
            Self::ObjectPrototypeToString {
                temporary_receiver, ..
            } => {
                if temporary_receiver.is_some() {
                    1
                } else {
                    0
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ObjectPrototypeTag {
    Boolean,
    Function,
    Number,
    Object,
    String,
}

impl ObjectPrototypeTag {
    const fn name(self) -> &'static str {
        match self {
            Self::Boolean => "Boolean",
            Self::Function => "Function",
            Self::Number => "Number",
            Self::Object => "Object",
            Self::String => "String",
        }
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
}

impl FunctionApplyContinuation {
    fn retained_values(&self) -> u64 {
        3_u64.saturating_add(usize_to_u64(self.arguments.len()))
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
    FunctionApplyLength(FunctionApplyContinuation),
}

impl OperatorPrimitiveTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::Unary { .. }
            | Self::NumberIntrinsic { new_target: None }
            | Self::StringIntrinsic { new_target: None } => 0,
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
            Self::FunctionApplyLength(state) => state.retained_values(),
        }
    }
}

struct OperatorPrimitiveContinuation {
    receiver: StoredValue,
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
        OperatorPrimitiveTarget::Unary { .. } | OperatorPrimitiveTarget::NumberToString { .. } => {}
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
        OperatorPrimitiveTarget::FunctionApplyLength(state) => {
            trace_function_apply_roots(state, mark);
        }
    }
}

fn trace_function_apply_roots(
    state: &FunctionApplyContinuation,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    mark(CollectionRoot::Heap(HeapReference::Function(state.target)));
    trace_stored_value_root(&state.receiver, mark);
    trace_stored_value_root(&state.array_like, mark);
    for argument in &state.arguments {
        trace_stored_value_root(argument, mark);
    }
}

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
            IntrinsicGetContinuation::ObjectPrototypeToString {
                temporary_receiver, ..
            } => {
                if let Some(receiver) = temporary_receiver {
                    mark(CollectionRoot::Heap(HeapReference::Object(*receiver)));
                }
            }
        },
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
    for value in &frame.stack {
        trace_stored_value_root(value, mark);
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
        .map(|_| ())
        .map_err(runtime_collection_execution_error)
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
        NativeDispatch::Immediate(_) | NativeDispatch::Pair(_, _) => false,
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

        if let Some(native) = self
            .runtime
            .functions
            .get(function_id)
            .and_then(HeapFunction::native)
            .copied()
        {
            let mut owned_arguments = Vec::new();
            owned_arguments
                .try_reserve_exact(arguments.len())
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: arguments.len(),
                })?;
            for argument in arguments {
                owned_arguments.push(argument.stored()?.duplicate());
            }
            let value = execute_native_entry(
                self.runtime,
                function_id,
                native,
                owned_arguments,
                limits,
                compiler,
            )?;
            return self.runtime.public_value(value);
        }

        let plan = plan_frame(self.runtime, function_id, 0, 0)?;
        let frame = create_frame(
            self.runtime,
            plan,
            StoredValue::Undefined,
            FrameArguments::Public(arguments),
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
    loop {
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
                let construction = inputs.is_construction();
                let native = runtime
                    .functions
                    .get(function)
                    .ok_or(EngineFault::StaleHeapEdge {
                        edge: "function",
                        index: function.index(),
                        generation: function.generation(),
                    })?
                    .native()
                    .copied();
                let origin = instruction_location(
                    runtime,
                    frames.last().ok_or(EngineFault::MissingInstruction {
                        function: FunctionTemplateId::new(0),
                        instruction: 0,
                    })?,
                    source_pc,
                )?;
                if let Some(native) = native {
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
                    let dispatch = dispatch_native_call(
                        runtime,
                        function,
                        native,
                        inputs,
                        Some(return_to),
                        Some(origin),
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
                        Ok(NativeDispatch::Pair(_, _)) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "native function call returned an operator value pair",
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
                            let caller_frames = exception_caller_frames(runtime, frames)?;
                            let exception = finish_exception(runtime, pending, caller_frames)?;
                            return Err(ExecutionError::Exception(exception));
                        }
                        Err(NativeFailure::AbruptAfterTransient(pending)) => {
                            let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                message: "transient native throw has no executing frame",
                            })?;
                            frame.transient_cleanup_pending = true;
                            let caller_frames = exception_caller_frames(runtime, frames)?;
                            let exception = finish_exception(runtime, pending, caller_frames)?;
                            return Err(ExecutionError::Exception(exception));
                        }
                        Err(NativeFailure::Execution(error)) => return Err(error),
                    }
                    continue;
                }
                if construction && !bytecode_function_is_constructor(runtime, function)? {
                    let pending = PendingException {
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::TypeError,
                            message: function_not_constructor_message(runtime, function)?,
                        },
                        origin,
                    };
                    let caller_frames = exception_caller_frames(runtime, frames)?;
                    let exception = finish_exception(runtime, pending, caller_frames)?;
                    return Err(ExecutionError::Exception(exception));
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
                        let caller_frames = exception_caller_frames(runtime, frames)?;
                        let exception = finish_exception(runtime, pending, caller_frames)?;
                        return Err(ExecutionError::Exception(exception));
                    }
                    Err(NativeFailure::AbruptAfterTransient(pending)) => {
                        let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                            message: "transient native throw has no executing frame",
                        })?;
                        frame.transient_cleanup_pending = true;
                        let caller_frames = exception_caller_frames(runtime, frames)?;
                        let exception = finish_exception(runtime, pending, caller_frames)?;
                        return Err(ExecutionError::Exception(exception));
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
                let caller_frames = exception_caller_frames(runtime, frames)?;
                let exception = finish_exception(runtime, pending, caller_frames)?;
                return Err(ExecutionError::Exception(exception));
            }
            Step::Return(value) => {
                let mut finished = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                *active_frame_values = active_frame_values.saturating_sub(finished.reserved_values);
                let return_to = finished.return_to;
                let mut value = if let Some(dynamic) = finished.dynamic_return.take() {
                    finish_dynamic_function_return(runtime, frames, dynamic, value)?
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
                            let caller_frames = exception_caller_frames(runtime, frames)?;
                            let exception = finish_exception(runtime, pending, caller_frames)?;
                            return Err(ExecutionError::Exception(exception));
                        }
                        Err(NativeFailure::AbruptAfterTransient(pending)) => {
                            let frame = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                                message: "transient native throw has no executing frame",
                            })?;
                            frame.transient_cleanup_pending = true;
                            let caller_frames = exception_caller_frames(runtime, frames)?;
                            let exception = finish_exception(runtime, pending, caller_frames)?;
                            return Err(ExecutionError::Exception(exception));
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

#[allow(
    clippy::large_enum_variant,
    reason = "boxing a Frame would introduce an unaccounted infallible allocation in the interpreter path"
)]
enum NativeDispatch {
    Immediate(StoredValue),
    Pair(StoredValue, StoredValue),
    Frame(Frame),
    Call(NativeCall),
}

enum NativeFailure {
    Abrupt(PendingException),
    AbruptAfterTransient(PendingException),
    Execution(ExecutionError),
}

impl From<ExecutionError> for NativeFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<JsStringError> for NativeFailure {
    fn from(error: JsStringError) -> Self {
        Self::Execution(error.into())
    }
}

impl From<crate::AtomError> for NativeFailure {
    fn from(error: crate::AtomError) -> Self {
        Self::Execution(error.into())
    }
}

impl From<EngineFault> for NativeFailure {
    fn from(error: EngineFault) -> Self {
        Self::Execution(error.into())
    }
}

fn native_continuation_values(continuations: &[NativeContinuation]) -> u64 {
    continuations.iter().fold(0_u64, |total, continuation| {
        total.saturating_add(continuation.retained_values())
    })
}

fn active_execution_frames(frames: &[Frame]) -> usize {
    frames.iter().fold(frames.len(), |total, frame| {
        total.saturating_add(frame.native_returns.len())
    })
}

fn attach_native_continuations(
    frame: &mut Frame,
    mut outer: Vec<NativeContinuation>,
) -> Result<(), NativeFailure> {
    if outer.is_empty() {
        return Ok(());
    }
    // Every frame returned by an unresolved native dispatch is freshly
    // created. Continuations are attached exactly once at the resolver
    // boundary, so this reservation is always for zero additional elements
    // and cannot fail after a dynamic root commits its environment.
    debug_assert!(frame.native_returns.is_empty());
    let retained_values = native_continuation_values(&outer);
    outer.try_reserve(frame.native_returns.len()).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: frame.native_returns.len(),
        }
    })?;
    outer.append(&mut frame.native_returns);
    frame.native_returns = outer;
    frame.reserved_values = frame.reserved_values.saturating_add(retained_values);
    Ok(())
}

fn prepend_native_continuations(
    call: &mut NativeCall,
    mut outer: Vec<NativeContinuation>,
) -> Result<(), NativeFailure> {
    if outer.is_empty() {
        return Ok(());
    }
    outer
        .try_reserve(call.continuations.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: call.continuations.len(),
        })?;
    outer.append(&mut call.continuations);
    call.continuations = outer;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "resuming a native abstract operation needs the same explicit execution authority and budgets as its originating call"
)]
fn resume_native_continuations(
    runtime: &mut Runtime,
    mut continuations: Vec<NativeContinuation>,
    mut value: StoredValue,
    return_to: Option<CallReturn>,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    while let Some(continuation) = continuations.pop() {
        let suspended_frames = active_frames.saturating_add(continuations.len());
        let suspended_values =
            active_frame_values.saturating_add(native_continuation_values(&continuations));
        let dispatch = match continuation {
            NativeContinuation::FunctionSource(state) => {
                let Some(compiler) = compiler else {
                    return Err(NativeFailure::Execution(
                        DynamicFunctionCompileFailure::Engine {
                            source: Arc::new(DynamicFunctionServiceUnavailable),
                        }
                        .into(),
                    ));
                };
                advance_function_source_conversion(
                    runtime,
                    state,
                    Some(value),
                    return_to,
                    suspended_frames,
                    suspended_values,
                    compiler,
                    execution_budget,
                )?
            }
            NativeContinuation::FunctionApply(state) => {
                advance_function_apply(runtime, state, Some(value), return_to, execution_budget)?
            }
            NativeContinuation::PropertyKey(state) => {
                advance_property_key_conversion(runtime, state, Some(value), return_to)?
            }
            NativeContinuation::OperatorPrimitive(state) => advance_operator_primitive_conversion(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::IntrinsicGet(state) => {
                finish_intrinsic_get(runtime, state, value, active_root_frames, &continuations)?
            }
            NativeContinuation::FunctionCall => NativeDispatch::Immediate(value),
        };
        match dispatch {
            NativeDispatch::Immediate(next) => value = next,
            pair @ NativeDispatch::Pair(_, _) => {
                if continuations.is_empty() {
                    return Ok(pair);
                }
                return Err(EngineFault::RuntimeInvariant {
                    message: "operator value pair escaped into an outer native continuation",
                }
                .into());
            }
            NativeDispatch::Frame(mut frame) => {
                attach_native_continuations(&mut frame, continuations)?;
                return Ok(NativeDispatch::Frame(frame));
            }
            NativeDispatch::Call(mut call) => {
                prepend_native_continuations(&mut call, continuations)?;
                return Ok(NativeDispatch::Call(call));
            }
        }
    }
    Ok(NativeDispatch::Immediate(value))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the iterative native-to-bytecode transition carries explicit frame and dynamic-compilation budgets"
)]
fn resolve_native_dispatch(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut saw_temporary_receiver = false;
    let result = resolve_native_dispatch_inner(
        runtime,
        dispatch,
        active_root_frames,
        active_frames,
        active_frame_values,
        compiler,
        execution_budget,
        &mut saw_temporary_receiver,
    );
    match result {
        Err(NativeFailure::Execution(error)) => {
            if saw_temporary_receiver && runtime.collection_pending {
                let _ = collect_cycles_with_execution_roots(runtime, active_root_frames, &[], &[]);
            }
            Err(NativeFailure::Execution(error))
        }
        Err(NativeFailure::Abrupt(pending)) if saw_temporary_receiver => {
            Err(NativeFailure::AbruptAfterTransient(pending))
        }
        result => result,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the iterative native-to-bytecode transition carries explicit frame and dynamic-compilation budgets"
)]
fn resolve_native_dispatch_inner(
    runtime: &mut Runtime,
    mut dispatch: NativeDispatch,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
    saw_temporary_receiver: &mut bool,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let NativeDispatch::Call(call) = dispatch else {
            return Ok(dispatch);
        };
        *saw_temporary_receiver |=
            native_continuations_have_temporary_receiver(&call.continuations);
        let suspended_frames = active_frames.saturating_add(call.continuations.len());
        // Call inputs are a synchronous transfer buffer, not values reserved
        // by an active frame. Persistent native state lives in continuations
        // and is charged here; a bytecode callee is charged by `plan_frame`.
        let suspended_values =
            active_frame_values.saturating_add(native_continuation_values(&call.continuations));
        check_execution_limit(
            RuntimeResource::Frames,
            u64::from(runtime.limits.max_active_frames),
            usize_to_u64(suspended_frames),
        )?;
        check_execution_limit(
            RuntimeResource::FrameValues,
            runtime.limits.max_active_frame_values,
            suspended_values,
        )?;
        let native = runtime
            .functions
            .get(call.function)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "function",
                index: call.function.index(),
                generation: call.function.generation(),
            })?
            .native()
            .copied();
        if let Some(native) = native {
            let outcome = dispatch_native_call(
                runtime,
                call.function,
                native,
                CallInputs {
                    receiver: call.receiver,
                    arguments: call.arguments,
                    new_target: None,
                },
                call.return_to,
                Some(call.origin),
                suspended_frames,
                suspended_values,
                compiler,
                execution_budget,
            )?;
            dispatch = match outcome {
                NativeDispatch::Immediate(value) => resume_native_continuations(
                    runtime,
                    call.continuations,
                    value,
                    call.return_to,
                    active_root_frames,
                    active_frames,
                    active_frame_values,
                    compiler,
                    execution_budget,
                )?,
                NativeDispatch::Pair(_, _) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "native function produced an operator value pair",
                    }
                    .into());
                }
                NativeDispatch::Frame(mut frame) => {
                    attach_native_continuations(&mut frame, call.continuations)?;
                    NativeDispatch::Frame(frame)
                }
                NativeDispatch::Call(mut inner) => {
                    prepend_native_continuations(&mut inner, call.continuations)?;
                    NativeDispatch::Call(inner)
                }
            };
            continue;
        }

        let plan = plan_frame(runtime, call.function, suspended_frames, suspended_values)
            .map_err(NativeFailure::Execution)?;
        let mut frame = create_frame(
            runtime,
            plan,
            call.receiver,
            FrameArguments::Owned(call.arguments),
            call.return_to,
            None,
        )
        .map_err(NativeFailure::Execution)?;
        attach_native_continuations(&mut frame, call.continuations)?;
        return Ok(NativeDispatch::Frame(frame));
    }
}

#[derive(Debug)]
struct DynamicFunctionServiceUnavailable;

impl fmt::Display for DynamicFunctionServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("no ordinary dynamic-Function compiler was supplied for this execution")
    }
}

impl Error for DynamicFunctionServiceUnavailable {}

fn execute_native_entry(
    runtime: &mut Runtime,
    function: FunctionId,
    native: NativeFunction,
    arguments: Vec<StoredValue>,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<StoredValue, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(limits);
    let mut prepared_frames = Vec::new();
    prepared_frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    let inputs = CallInputs {
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(arguments),
        new_target: None,
    };
    let dispatch = dispatch_native_call(
        runtime,
        function,
        native,
        inputs,
        None,
        None,
        0,
        0,
        compiler,
        &mut execution_budget,
    );
    let dispatch = match dispatch {
        Ok(dispatch) => resolve_native_dispatch(
            runtime,
            dispatch,
            &prepared_frames,
            0,
            0,
            compiler,
            &mut execution_budget,
        ),
        Err(error) => Err(error),
    };
    match dispatch {
        Ok(NativeDispatch::Immediate(value)) => Ok(value),
        Ok(NativeDispatch::Pair(_, _)) => Err(EngineFault::RuntimeInvariant {
            message: "host native entry returned an operator value pair",
        }
        .into()),
        Ok(NativeDispatch::Frame(frame)) => {
            prepared_frames.push(frame);
            execute_prepared_frames_with_budget(
                runtime,
                prepared_frames,
                compiler,
                None,
                &mut execution_budget,
            )
        }
        Ok(NativeDispatch::Call(_)) => Err(EngineFault::RuntimeInvariant {
            message: "native dispatch resolver returned an unresolved call",
        }
        .into()),
        Err(NativeFailure::Execution(error)) => Err(error),
        Err(NativeFailure::Abrupt(pending)) => {
            let exception = finish_exception(runtime, pending, Vec::new())?;
            Err(ExecutionError::Exception(exception))
        }
        Err(NativeFailure::AbruptAfterTransient(pending)) => {
            let exception = match finish_exception(runtime, pending, Vec::new()) {
                Ok(exception) => exception,
                Err(error) => {
                    if runtime.collection_pending {
                        let _ = runtime.collect_cycles();
                    }
                    return Err(error);
                }
            };
            if runtime.collection_pending {
                let _ = runtime.collect_cycles();
            }
            Err(ExecutionError::Exception(exception))
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "native invocation, compilation, installation, and rollback remain one explicit audited boundary"
)]
fn dispatch_native_call(
    runtime: &mut Runtime,
    function: FunctionId,
    native: NativeFunction,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if inputs.new_target.is_some() && !native.kind.is_constructor() {
        let Some(origin) = origin else {
            return Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "host construction of a nonconstructor native function is not implemented",
                }
                .into(),
            ));
        };
        return Err(NativeFailure::Abrupt(PendingException {
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: function_not_constructor_message(runtime, function)?,
            },
            origin,
        }));
    }
    match native.kind {
        NativeFunctionKind::FunctionPrototype => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        NativeFunctionKind::FunctionPrototypeApply => begin_function_apply(
            runtime,
            native.realm,
            inputs,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            active_frames,
            active_frame_values,
            execution_budget,
        ),
        NativeFunctionKind::FunctionPrototypeCall => {
            let origin = origin.unwrap_or_else(native_function_host_origin);
            let StoredValue::Function(function) = inputs.receiver else {
                return Err(NativeFailure::Abrupt(PendingException {
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin,
                }));
            };
            let mut arguments = inputs.arguments;
            let receiver = arguments.take_first_or_undefined();
            let mut continuations = Vec::new();
            continuations
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            continuations.push(NativeContinuation::FunctionCall);
            Ok(NativeDispatch::Call(NativeCall {
                function,
                receiver,
                arguments,
                return_to,
                origin,
                continuations,
            }))
        }
        NativeFunctionKind::OrdinaryFunctionConstructor => {
            let Some(compiler) = compiler else {
                return Err(NativeFailure::Execution(
                    DynamicFunctionCompileFailure::Engine {
                        source: Arc::new(DynamicFunctionServiceUnavailable),
                    }
                    .into(),
                ));
            };
            let origin = origin.unwrap_or_else(native_function_host_origin);
            begin_function_source_conversion(
                runtime,
                native,
                inputs.arguments.into_remaining_values(),
                inputs.new_target,
                return_to,
                origin,
                active_frames,
                active_frame_values,
                compiler,
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectPrototypeToString => begin_object_prototype_to_string(
            runtime,
            native.realm,
            inputs.receiver,
            return_to,
            origin,
        ),
        NativeFunctionKind::ObjectPrototypeValueOf => match inputs.receiver {
            value @ (StoredValue::Function(_) | StoredValue::Object(_)) => {
                Ok(NativeDispatch::Immediate(value))
            }
            StoredValue::Boolean(value) => {
                let object = runtime.allocate_boxed_boolean(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::Number(value) => {
                let object = runtime.allocate_boxed_number(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::String(value) => {
                let object = runtime.allocate_boxed_string(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::Undefined | StoredValue::Null => {
                let Some(origin) = origin else {
                    return Err(NativeFailure::Execution(
                        EngineFault::RuntimeInvariant {
                            message: "host Object.prototype.valueOf error has no source origin",
                        }
                        .into(),
                    ));
                };
                Err(NativeFailure::Abrupt(PendingException {
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("cannot convert to object")?,
                    },
                    origin,
                }))
            }
            StoredValue::Symbol(_) => Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "Object.prototype.valueOf Symbol boxing is not implemented",
                }
                .into(),
            )),
        },
        NativeFunctionKind::BooleanConstructor => {
            let mut arguments = inputs.arguments;
            let value = arguments.take_first_or_undefined().is_truthy();
            let Some(new_target) = inputs.new_target else {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(value)));
            };
            begin_boolean_constructor_wrapper(runtime, new_target, value, return_to, origin)
        }
        NativeFunctionKind::BooleanPrototypeToString => {
            let value = boolean_receiver_value(runtime, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(if value { "true" } else { "false" })?,
            )))
        }
        NativeFunctionKind::BooleanPrototypeValueOf => {
            let value = boolean_receiver_value(runtime, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(value)))
        }
        NativeFunctionKind::NumberConstructor => {
            let mut arguments = inputs.arguments;
            let Some(argument) = arguments.take_first() else {
                let value = JsNumber::from_i32(0);
                return inputs.new_target.map_or_else(
                    || Ok(NativeDispatch::Immediate(StoredValue::Number(value))),
                    |new_target| {
                        begin_number_constructor_wrapper(
                            runtime, new_target, value, return_to, origin,
                        )
                    },
                );
            };
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::NumberIntrinsic {
                    new_target: inputs.new_target,
                },
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::NumberPrototypeToString => {
            let number = number_receiver_value(runtime, &inputs.receiver, origin.as_ref())?;
            let mut arguments = inputs.arguments;
            match arguments.take_first() {
                None | Some(StoredValue::Undefined) => Ok(NativeDispatch::Immediate(
                    StoredValue::String(number.to_radix_string(10)?),
                )),
                Some(radix) => begin_operator_primitive_conversion(
                    runtime,
                    radix,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::NumberToString { number },
                    return_to,
                    origin.unwrap_or_else(native_function_host_origin),
                    execution_budget,
                ),
            }
        }
        NativeFunctionKind::NumberPrototypeValueOf => {
            let value = number_receiver_value(runtime, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::Number(value)))
        }
        NativeFunctionKind::StringConstructor => {
            let mut arguments = inputs.arguments;
            let Some(argument) = arguments.take_first() else {
                let value = JsString::empty();
                return if let Some(new_target) = inputs.new_target {
                    begin_string_constructor_wrapper(runtime, new_target, value, return_to, origin)
                } else {
                    Ok(NativeDispatch::Immediate(StoredValue::String(value)))
                };
            };
            if inputs.new_target.is_none()
                && let StoredValue::Symbol(symbol) = &argument
            {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    symbol_descriptive_string(symbol)?,
                )));
            }
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::StringIntrinsic {
                    new_target: inputs.new_target,
                },
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::StringPrototypeToString
        | NativeFunctionKind::StringPrototypeValueOf => {
            let value = string_receiver_value(runtime, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::String(value)))
        }
        NativeFunctionKind::FunctionPrototypeToString => {
            let StoredValue::Function(function) = inputs.receiver else {
                let Some(origin) = origin else {
                    return Err(NativeFailure::Execution(
                        EngineFault::RuntimeInvariant {
                            message: "host Function.prototype.toString error has no source origin",
                        }
                        .into(),
                    ));
                };
                return Err(NativeFailure::Abrupt(PendingException {
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin,
                }));
            };
            Ok(NativeDispatch::Immediate(StoredValue::String(
                function_to_string(runtime, function, origin.as_ref())?,
            )))
        }
    }
}

const MAX_FUNCTION_APPLY_ARGUMENTS: u32 = 65_534;

#[allow(
    clippy::too_many_arguments,
    reason = "apply admission keeps callable validation, retained-value preflight, and native work budget explicit"
)]
fn begin_function_apply(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(target) = inputs.receiver else {
        return Err(function_apply_exception(
            ExceptionKind::TypeError,
            "not a function",
            origin,
        )?);
    };
    let mut supplied = inputs.arguments;
    let receiver = supplied.take_first_or_undefined();
    let array_like = supplied.take_first_or_undefined();
    if matches!(array_like, StoredValue::Undefined | StoredValue::Null) {
        return function_apply_target_call(target, receiver, Vec::new(), return_to, origin);
    }
    if !matches!(
        array_like,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return Err(function_apply_exception(
            ExceptionKind::TypeError,
            "not a object",
            origin,
        )?);
    }

    check_execution_limit(
        RuntimeResource::Frames,
        u64::from(runtime.limits.max_active_frames),
        usize_to_u64(active_frames).saturating_add(1),
    )?;
    // The fourth retained slot covers an object-valued length while its
    // Number-hint ToPrimitive state is suspended.
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        active_frame_values.saturating_add(4),
    )?;
    let state = FunctionApplyContinuation {
        target,
        receiver,
        array_like,
        realm,
        length: None,
        next_index: 0,
        arguments: Vec::new(),
        stage: FunctionApplyStage::AwaitLength,
        active_frame_values,
        origin,
    };
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    charge_function_apply_property_lookup(runtime, &state.array_like, execution_budget)?;
    match read_static_property(runtime, realm, &state.array_like, &length_key)? {
        PropertyReadOutcome::Value(value) => begin_function_apply_length_conversion(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        PropertyReadOutcome::Getter { function, receiver } => {
            function_apply_getter_call(state, function, receiver, return_to)
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "object-valued apply argument list failed its length read",
        }
        .into()),
    }
}

fn advance_function_apply(
    runtime: &mut Runtime,
    mut state: FunctionApplyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(value) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "apply continuation resumed without a getter completion",
        }
        .into());
    };
    match state.stage {
        FunctionApplyStage::AwaitLength => begin_function_apply_length_conversion(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        FunctionApplyStage::AwaitIndex => {
            let length = state.length.ok_or(EngineFault::RuntimeInvariant {
                message: "apply index continuation has no fixed length",
            })?;
            if state.next_index >= length {
                return Err(EngineFault::RuntimeInvariant {
                    message: "apply index continuation resumed after its fixed length",
                }
                .into());
            }
            state.arguments.push(value);
            state.next_index = state.next_index.saturating_add(1);
            advance_function_apply_indices(runtime, state, return_to, execution_budget)
        }
    }
}

fn begin_function_apply_length_conversion(
    runtime: &mut Runtime,
    state: FunctionApplyContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::FunctionApplyLength(state),
        return_to,
        origin,
        execution_budget,
    )
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ToLength is clamped and checked against the 65,534 QuickJS call-argument ceiling before conversion"
)]
fn finish_function_apply_length(
    runtime: &mut Runtime,
    mut state: FunctionApplyContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, &state.origin)?.as_f64();
    let integer = if number.is_nan() || number <= 0.0 {
        0.0
    } else if number.is_infinite() {
        number
    } else {
        number.floor()
    };
    if integer > f64::from(MAX_FUNCTION_APPLY_ARGUMENTS) {
        return Err(function_apply_exception(
            ExceptionKind::RangeError,
            "too many arguments in function call (only 65534 allowed)",
            state.origin,
        )?);
    }
    let length = integer as u32;
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        state
            .active_frame_values
            .saturating_add(3)
            .saturating_add(u64::from(length)),
    )?;
    state
        .arguments
        .try_reserve_exact(length as usize)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: length as usize,
        })?;
    // Charge the complete fixed scan before the first observable indexed Get.
    execution_budget.charge_instructions(u64::from(length))?;
    state.length = Some(length);
    state.stage = FunctionApplyStage::AwaitIndex;
    advance_function_apply_indices(runtime, state, return_to, execution_budget)
}

fn advance_function_apply_indices(
    runtime: &mut Runtime,
    mut state: FunctionApplyContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = state.length.ok_or(EngineFault::RuntimeInvariant {
        message: "apply argument scan has no fixed length",
    })?;
    while state.next_index < length {
        let index = ArrayIndex::new(state.next_index).ok_or(EngineFault::RuntimeInvariant {
            message: "apply argument index exceeds the array-index domain",
        })?;
        let key = PropertyKey::from_index(index);
        charge_function_apply_property_lookup(runtime, &state.array_like, execution_budget)?;
        match read_static_property(runtime, state.realm, &state.array_like, &key)? {
            PropertyReadOutcome::Value(value) => {
                state.arguments.push(value);
                state.next_index = state.next_index.saturating_add(1);
            }
            PropertyReadOutcome::Getter { function, receiver } => {
                state.stage = FunctionApplyStage::AwaitIndex;
                return function_apply_getter_call(state, function, receiver, return_to);
            }
            PropertyReadOutcome::Failed(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "object-valued apply argument list failed an indexed read",
                }
                .into());
            }
        }
    }
    function_apply_target_call(
        state.target,
        state.receiver,
        state.arguments,
        return_to,
        state.origin,
    )
}

fn charge_function_apply_property_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let mut current = Some(base.heap_reference().ok_or(EngineFault::RuntimeInvariant {
        message: "apply property lookup base has no heap reference",
    })?);
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    while let Some(reference) = current {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "ordinary prototype chain contains a cycle",
            }
            .into());
        }
        remaining -= 1;
        let record = runtime.object_record(reference)?;
        // `ObjectRecord::own_property` is a linear shape scan. Charge its
        // complete upper bound, plus the prototype transition, before the
        // observable Get so a hostile dense array-like cannot hide O(n²)
        // native work behind O(n) fuel.
        execution_budget
            .charge_instructions(usize_to_u64(record.property_count()).saturating_add(1))?;
        current = record.prototype();
    }
    Ok(())
}

fn function_apply_getter_call(
    state: FunctionApplyContinuation,
    function: FunctionId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FunctionApply(state));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
    }))
}

fn function_apply_target_call(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FunctionCall);
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
    }))
}

fn function_apply_exception(
    kind: ExceptionKind,
    message: &str,
    origin: JsStackFrame,
) -> Result<NativeFailure, JsStringError> {
    Ok(NativeFailure::Abrupt(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

fn symbol_descriptive_string(symbol: &crate::Atom) -> Result<JsString, NativeFailure> {
    let description = symbol
        .description()
        .cloned()
        .unwrap_or_else(JsString::empty);
    Ok(JsString::from_utf8("Symbol(")?
        .concat(&description)?
        .concat(&JsString::from_utf8(")")?)?)
}

fn boolean_receiver_value(
    runtime: &Runtime,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<bool, NativeFailure> {
    let value = match receiver {
        StoredValue::Boolean(value) => Some(*value),
        StoredValue::Object(object) => runtime.boxed_boolean(*object)?,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    let origin = origin.cloned().unwrap_or_else(native_function_host_origin);
    Err(NativeFailure::Abrupt(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a boolean")?,
        },
        origin,
    }))
}

fn number_receiver_value(
    runtime: &Runtime,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<JsNumber, NativeFailure> {
    let value = match receiver {
        StoredValue::Number(value) => Some(*value),
        StoredValue::Object(object) => runtime.boxed_number(*object)?,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    let origin = origin.cloned().unwrap_or_else(native_function_host_origin);
    Err(NativeFailure::Abrupt(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a number")?,
        },
        origin,
    }))
}

fn string_receiver_value(
    runtime: &Runtime,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<JsString, NativeFailure> {
    let value = match receiver {
        StoredValue::String(value) => Some(value.clone()),
        StoredValue::Object(object) => runtime.boxed_string(*object)?.cloned(),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    let origin = origin.cloned().unwrap_or_else(native_function_host_origin);
    Err(NativeFailure::Abrupt(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a string")?,
        },
        origin,
    }))
}

fn begin_boolean_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: bool,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::BooleanConstructor { new_target, value },
        return_to,
        origin,
    )
}

fn begin_number_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: JsNumber,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::NumberConstructor { new_target, value },
        return_to,
        origin,
    )
}

fn begin_string_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: JsString,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::StringConstructor { new_target, value },
        return_to,
        origin,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the generic resumable Get boundary preserves its receiver, continuation target, caller continuation, and source origin"
)]
fn begin_intrinsic_get(
    runtime: &mut Runtime,
    reference: HeapReference,
    receiver: StoredValue,
    key: &PropertyKey,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    match read_heap_property_for_receiver(runtime, reference, receiver, key)? {
        PropertyReadOutcome::Value(value) => {
            finish_intrinsic_get(runtime, continuation, value, &[], &[])
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            intrinsic_getter_call(function, receiver, continuation, return_to, origin)
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "heap-only intrinsic Get produced a primitive property failure",
        }
        .into()),
    }
}

fn intrinsic_getter_call(
    function: FunctionId,
    receiver: StoredValue,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    Ok(intrinsic_getter_call_with_reserved_continuation(
        function,
        receiver,
        continuation,
        return_to,
        origin,
        continuations,
    ))
}

fn reserve_intrinsic_get_continuation() -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    Ok(continuations)
}

fn intrinsic_getter_call_with_reserved_continuation(
    function: FunctionId,
    receiver: StoredValue,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    mut continuations: Vec<NativeContinuation>,
) -> NativeDispatch {
    debug_assert!(continuations.capacity() >= 1);
    continuations.push(NativeContinuation::IntrinsicGet(continuation));
    NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin: origin.unwrap_or_else(native_function_host_origin),
        continuations,
    })
}

fn finish_intrinsic_get(
    runtime: &mut Runtime,
    continuation: IntrinsicGetContinuation,
    value: StoredValue,
    active_root_frames: &[Frame],
    outer_continuations: &[NativeContinuation],
) -> Result<NativeDispatch, NativeFailure> {
    match continuation {
        IntrinsicGetContinuation::BooleanConstructor {
            new_target,
            value: boolean_value,
        } => finish_boolean_constructor_wrapper(runtime, new_target, boolean_value, &value),
        IntrinsicGetContinuation::NumberConstructor {
            new_target,
            value: number_value,
        } => finish_number_constructor_wrapper(runtime, new_target, number_value, &value),
        IntrinsicGetContinuation::StringConstructor {
            new_target,
            value: string_value,
        } => finish_string_constructor_wrapper(runtime, new_target, string_value, &value),
        IntrinsicGetContinuation::ObjectPrototypeToString {
            default_tag,
            temporary_receiver,
        } => {
            if temporary_receiver.is_none() {
                return finish_object_prototype_to_string(default_tag, value);
            }

            // Release the intrinsic's temporary receiver before allocating the
            // result string, exactly as QuickJS releases its local boxed value
            // immediately after Get. The getter completion remains a root
            // until it has been consumed, so `return this` and heap graphs
            // reachable only through the completion cannot be reclaimed early.
            let completion_holds_heap =
                matches!(value, StoredValue::Function(_) | StoredValue::Object(_));
            let cleanup = collect_cycles_with_execution_roots(
                runtime,
                active_root_frames,
                outer_continuations,
                std::slice::from_ref(&value),
            );
            let result = finish_object_prototype_to_string(default_tag, value);

            // Collection scratch allocation is host bookkeeping, not a
            // JavaScript operation. It must never replace a completed getter,
            // a successful result, or the formatting failure that already won.
            // Retry after consuming a heap-valued completion so a receiver
            // kept alive only by `tag` is released at the same boundary.
            if cleanup.is_err() || completion_holds_heap {
                let _ = collect_cycles_with_execution_roots(
                    runtime,
                    active_root_frames,
                    outer_continuations,
                    &[],
                );
            }
            result
        }
    }
}

fn finish_boolean_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    boolean_value: bool,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_boolean_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_boxed_boolean_with_prototype(prototype, boolean_value)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn finish_number_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    number_value: JsNumber,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_number_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_boxed_number_with_prototype(prototype, number_value)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn finish_string_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    string_value: JsString,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_string_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_boxed_string_with_prototype(prototype, string_value)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the suspended source-conversion state preserves the original native call boundary"
)]
fn begin_function_source_conversion(
    runtime: &mut Runtime,
    native: NativeFunction,
    arguments: Vec<StoredValue>,
    construction: Option<FunctionId>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    advance_function_source_conversion(
        runtime,
        FunctionSourceContinuation {
            native,
            arguments,
            index: 0,
            stage: PrimitiveConversionStage::Start,
            construction,
            origin,
        },
        None,
        return_to,
        active_frames,
        active_frame_values,
        compiler,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the explicit ToPrimitive(String) state machine keeps every observable lookup and call in one audited order"
)]
fn advance_function_source_conversion(
    runtime: &mut Runtime,
    mut state: FunctionSourceContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty => {
                let property = match state.stage {
                    PrimitiveConversionStage::AwaitExoticProperty => {
                        PrimitiveConversionProperty::Exotic
                    }
                    PrimitiveConversionStage::AwaitToStringProperty => {
                        PrimitiveConversionProperty::ToString
                    }
                    PrimitiveConversionStage::AwaitValueOfProperty => {
                        PrimitiveConversionProperty::ValueOf
                    }
                    PrimitiveConversionStage::Start
                    | PrimitiveConversionStage::ToString
                    | PrimitiveConversionStage::ValueOf
                    | PrimitiveConversionStage::AwaitExotic
                    | PrimitiveConversionStage::AwaitToString
                    | PrimitiveConversionStage::AwaitValueOf => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "dynamic Function property stage changed while resuming",
                        }
                        .into());
                    }
                };
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return function_source_method_call(state, function, arguments, return_to);
                    }
                }
            }
            PrimitiveConversionStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                state.stage = PrimitiveConversionStage::ValueOf;
            }
            PrimitiveConversionStage::AwaitExotic | PrimitiveConversionStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(primitive_conversion_type_error(
                    &state.origin,
                    "toPrimitive",
                )?);
            }
            PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                let converted = dynamic_source_primitive_to_string(value, &state.origin)?;
                let argument =
                    state
                        .arguments
                        .get_mut(state.index)
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "dynamic Function source conversion lost its current argument",
                        })?;
                *argument = StoredValue::String(converted);
                state.index = state.index.saturating_add(1);
                state.stage = PrimitiveConversionStage::Start;
            }
            PrimitiveConversionStage::Start
            | PrimitiveConversionStage::ToString
            | PrimitiveConversionStage::ValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "dynamic Function source conversion resumed outside a call stage",
                }
                .into());
            }
        }
    }

    loop {
        if state.index == state.arguments.len() {
            let source = completed_dynamic_function_source(state.arguments)?;
            return finish_ordinary_function_constructor(
                runtime,
                state.native,
                state.construction,
                source,
                return_to,
                state.origin,
                active_frames,
                active_frame_values,
                compiler,
                execution_budget,
            );
        }

        let current = state
            .arguments
            .get(state.index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "dynamic Function source conversion index escaped its arguments",
            })?;
        if !matches!(current, StoredValue::Function(_) | StoredValue::Object(_)) {
            let converted = dynamic_source_primitive_to_string(current.duplicate(), &state.origin)?;
            let current =
                state
                    .arguments
                    .get_mut(state.index)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "dynamic Function primitive conversion lost its argument",
                    })?;
            *current = StoredValue::String(converted);
            state.index = state.index.saturating_add(1);
            state.stage = PrimitiveConversionStage::Start;
            continue;
        }

        let reference = current
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued dynamic Function source has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            PrimitiveConversionStage::Start => (
                PrimitiveConversionProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                PrimitiveConversionStage::AwaitExoticProperty,
            ),
            PrimitiveConversionStage::ToString => (
                PrimitiveConversionProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PrimitiveConversionStage::AwaitToStringProperty,
            ),
            PrimitiveConversionStage::ValueOf => (
                PrimitiveConversionProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                PrimitiveConversionStage::AwaitValueOfProperty,
            ),
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty
            | PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "dynamic Function source conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_primitive_conversion_property(runtime, reference, &key)? {
            PrimitiveConversionPropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return function_source_method_call(state, function, Vec::new(), return_to);
            }
            PrimitiveConversionPropertyLookup::Value(value) => {
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return function_source_method_call(state, function, arguments, return_to);
                    }
                }
            }
        }
    }
}

fn lookup_primitive_conversion_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<PrimitiveConversionPropertyLookup, NativeFailure> {
    Ok(match lookup_heap_property(runtime, Some(reference), key)? {
        None => PrimitiveConversionPropertyLookup::Value(StoredValue::Undefined),
        Some(OwnProperty::Data { value, .. }) => PrimitiveConversionPropertyLookup::Value(value),
        Some(OwnProperty::Accessor {
            getter: Some(function),
            ..
        }) => PrimitiveConversionPropertyLookup::Getter(function),
        Some(OwnProperty::Accessor { getter: None, .. }) => {
            PrimitiveConversionPropertyLookup::Value(StoredValue::Undefined)
        }
    })
}

fn use_primitive_conversion_property(
    stage: &mut PrimitiveConversionStage,
    property: PrimitiveConversionProperty,
    value: &StoredValue,
    origin: &JsStackFrame,
) -> Result<PrimitiveConversionPropertyAction, NativeFailure> {
    match property {
        PrimitiveConversionProperty::Exotic => match value {
            StoredValue::Undefined | StoredValue::Null => {
                *stage = PrimitiveConversionStage::ToString;
                Ok(PrimitiveConversionPropertyAction::Continue)
            }
            StoredValue::Function(function) => {
                *stage = PrimitiveConversionStage::AwaitExotic;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(StoredValue::String(JsString::from_utf8("string")?));
                Ok(PrimitiveConversionPropertyAction::Call {
                    function: *function,
                    arguments,
                })
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                Err(primitive_conversion_type_error(origin, "not a function")?)
            }
        },
        PrimitiveConversionProperty::ToString => match value {
            StoredValue::Function(function) => {
                *stage = PrimitiveConversionStage::AwaitToString;
                Ok(PrimitiveConversionPropertyAction::Call {
                    function: *function,
                    arguments: Vec::new(),
                })
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                *stage = PrimitiveConversionStage::ValueOf;
                Ok(PrimitiveConversionPropertyAction::Continue)
            }
        },
        PrimitiveConversionProperty::ValueOf => match value {
            StoredValue::Function(function) => {
                *stage = PrimitiveConversionStage::AwaitValueOf;
                Ok(PrimitiveConversionPropertyAction::Call {
                    function: *function,
                    arguments: Vec::new(),
                })
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                Err(primitive_conversion_type_error(origin, "toPrimitive")?)
            }
        },
    }
}

fn function_source_method_call(
    state: FunctionSourceContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = state
        .arguments
        .get(state.index)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "dynamic Function source method lost its receiver",
        })?
        .duplicate();
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FunctionSource(state));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
    }))
}

fn begin_property_key_conversion(
    runtime: &mut Runtime,
    value: StoredValue,
    target: PropertyKeyTarget,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
        return advance_property_key_conversion(
            runtime,
            PropertyKeyContinuation {
                receiver: value,
                stage: PrimitiveConversionStage::Start,
                target,
                origin,
            },
            None,
            return_to,
        );
    }
    finish_property_key_target(runtime, value, target, return_to, &origin)
}

#[allow(
    clippy::too_many_lines,
    reason = "the explicit ToPropertyKey state machine preserves every observable lookup, getter, and call boundary"
)]
fn advance_property_key_conversion(
    runtime: &mut Runtime,
    mut state: PropertyKeyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty => {
                let property = match state.stage {
                    PrimitiveConversionStage::AwaitExoticProperty => {
                        PrimitiveConversionProperty::Exotic
                    }
                    PrimitiveConversionStage::AwaitToStringProperty => {
                        PrimitiveConversionProperty::ToString
                    }
                    PrimitiveConversionStage::AwaitValueOfProperty => {
                        PrimitiveConversionProperty::ValueOf
                    }
                    PrimitiveConversionStage::Start
                    | PrimitiveConversionStage::ToString
                    | PrimitiveConversionStage::ValueOf
                    | PrimitiveConversionStage::AwaitExotic
                    | PrimitiveConversionStage::AwaitToString
                    | PrimitiveConversionStage::AwaitValueOf => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "property-key conversion property stage changed while resuming",
                        }
                        .into());
                    }
                };
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return property_key_method_call(state, function, arguments, return_to);
                    }
                }
            }
            PrimitiveConversionStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                state.stage = PrimitiveConversionStage::ValueOf;
            }
            PrimitiveConversionStage::AwaitExotic | PrimitiveConversionStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(primitive_conversion_type_error(
                    &state.origin,
                    "toPrimitive",
                )?);
            }
            PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                return finish_property_key_target(
                    runtime,
                    value,
                    state.target,
                    return_to,
                    &state.origin,
                );
            }
            PrimitiveConversionStage::Start
            | PrimitiveConversionStage::ToString
            | PrimitiveConversionStage::ValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "property-key conversion resumed outside a call stage",
                }
                .into());
            }
        }
    }

    loop {
        let reference = state
            .receiver
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued property key has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            PrimitiveConversionStage::Start => (
                PrimitiveConversionProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                PrimitiveConversionStage::AwaitExoticProperty,
            ),
            PrimitiveConversionStage::ToString => (
                PrimitiveConversionProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PrimitiveConversionStage::AwaitToStringProperty,
            ),
            PrimitiveConversionStage::ValueOf => (
                PrimitiveConversionProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                PrimitiveConversionStage::AwaitValueOfProperty,
            ),
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty
            | PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "property-key conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_primitive_conversion_property(runtime, reference, &key)? {
            PrimitiveConversionPropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return property_key_method_call(state, function, Vec::new(), return_to);
            }
            PrimitiveConversionPropertyLookup::Value(value) => {
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return property_key_method_call(state, function, arguments, return_to);
                    }
                }
            }
        }
    }
}

fn property_key_method_call(
    state: PropertyKeyContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = state.receiver.duplicate();
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::PropertyKey(state));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
    }))
}

fn begin_operator_primitive_conversion(
    runtime: &mut Runtime,
    value: StoredValue,
    hint: OperatorPrimitiveHint,
    target: OperatorPrimitiveTarget,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
        return advance_operator_primitive_conversion(
            runtime,
            OperatorPrimitiveContinuation {
                receiver: value,
                hint,
                stage: OperatorPrimitiveStage::Start,
                target,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }
    finish_operator_primitive_target(runtime, value, target, return_to, &origin, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "the explicit ToPrimitive state machine preserves every observable lookup, getter, and call boundary"
)]
fn advance_operator_primitive_conversion(
    runtime: &mut Runtime,
    mut state: OperatorPrimitiveContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            OperatorPrimitiveStage::AwaitExoticProperty
            | OperatorPrimitiveStage::AwaitValueOfProperty
            | OperatorPrimitiveStage::AwaitToStringProperty => {
                let property = match state.stage {
                    OperatorPrimitiveStage::AwaitExoticProperty => {
                        PrimitiveConversionProperty::Exotic
                    }
                    OperatorPrimitiveStage::AwaitValueOfProperty => {
                        PrimitiveConversionProperty::ValueOf
                    }
                    OperatorPrimitiveStage::AwaitToStringProperty => {
                        PrimitiveConversionProperty::ToString
                    }
                    OperatorPrimitiveStage::Start
                    | OperatorPrimitiveStage::ValueOf
                    | OperatorPrimitiveStage::ToString
                    | OperatorPrimitiveStage::AwaitExotic
                    | OperatorPrimitiveStage::AwaitValueOf
                    | OperatorPrimitiveStage::AwaitToString => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "operator primitive property stage changed while resuming",
                        }
                        .into());
                    }
                };
                if let Some((function, arguments)) =
                    use_operator_primitive_property(&mut state, property, &value)?
                {
                    return operator_primitive_method_call(state, function, arguments, return_to);
                }
            }
            OperatorPrimitiveStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    return Err(primitive_conversion_type_error(
                        &state.origin,
                        "toPrimitive",
                    )?);
                }
                state.stage = OperatorPrimitiveStage::ToString;
            }
            OperatorPrimitiveStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    state.stage = OperatorPrimitiveStage::ValueOf;
                } else {
                    return Err(primitive_conversion_type_error(
                        &state.origin,
                        "toPrimitive",
                    )?);
                }
            }
            OperatorPrimitiveStage::AwaitExotic
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(primitive_conversion_type_error(
                    &state.origin,
                    "toPrimitive",
                )?);
            }
            OperatorPrimitiveStage::AwaitExotic
            | OperatorPrimitiveStage::AwaitValueOf
            | OperatorPrimitiveStage::AwaitToString => {
                return finish_operator_primitive_target(
                    runtime,
                    value,
                    state.target,
                    return_to,
                    &state.origin,
                    execution_budget,
                );
            }
            OperatorPrimitiveStage::Start
            | OperatorPrimitiveStage::ValueOf
            | OperatorPrimitiveStage::ToString => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "operator primitive conversion resumed outside a call stage",
                }
                .into());
            }
        }
    }

    loop {
        let reference = state
            .receiver
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued operator operand has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            OperatorPrimitiveStage::Start => (
                PrimitiveConversionProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                OperatorPrimitiveStage::AwaitExoticProperty,
            ),
            OperatorPrimitiveStage::ValueOf => (
                PrimitiveConversionProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                OperatorPrimitiveStage::AwaitValueOfProperty,
            ),
            OperatorPrimitiveStage::ToString => (
                PrimitiveConversionProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                OperatorPrimitiveStage::AwaitToStringProperty,
            ),
            OperatorPrimitiveStage::AwaitExoticProperty
            | OperatorPrimitiveStage::AwaitValueOfProperty
            | OperatorPrimitiveStage::AwaitToStringProperty
            | OperatorPrimitiveStage::AwaitExotic
            | OperatorPrimitiveStage::AwaitValueOf
            | OperatorPrimitiveStage::AwaitToString => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "operator primitive conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_primitive_conversion_property(runtime, reference, &key)? {
            PrimitiveConversionPropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return operator_primitive_method_call(state, function, Vec::new(), return_to);
            }
            PrimitiveConversionPropertyLookup::Value(value) => {
                if let Some((function, arguments)) =
                    use_operator_primitive_property(&mut state, property, &value)?
                {
                    return operator_primitive_method_call(state, function, arguments, return_to);
                }
            }
        }
    }
}

fn use_operator_primitive_property(
    state: &mut OperatorPrimitiveContinuation,
    property: PrimitiveConversionProperty,
    value: &StoredValue,
) -> Result<Option<(FunctionId, Vec<StoredValue>)>, NativeFailure> {
    match property {
        PrimitiveConversionProperty::Exotic => match value {
            StoredValue::Undefined | StoredValue::Null => {
                state.stage = state.hint.first_ordinary_stage();
                Ok(None)
            }
            StoredValue::Function(function) => {
                state.stage = OperatorPrimitiveStage::AwaitExotic;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(StoredValue::String(JsString::from_utf8(state.hint.name())?));
                Ok(Some((*function, arguments)))
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(primitive_conversion_type_error(
                &state.origin,
                "not a function",
            )?),
        },
        PrimitiveConversionProperty::ValueOf => match value {
            StoredValue::Function(function) => {
                state.stage = OperatorPrimitiveStage::AwaitValueOf;
                Ok(Some((*function, Vec::new())))
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    return Err(primitive_conversion_type_error(
                        &state.origin,
                        "toPrimitive",
                    )?);
                }
                state.stage = OperatorPrimitiveStage::ToString;
                Ok(None)
            }
        },
        PrimitiveConversionProperty::ToString => match value {
            StoredValue::Function(function) => {
                state.stage = OperatorPrimitiveStage::AwaitToString;
                Ok(Some((*function, Vec::new())))
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    state.stage = OperatorPrimitiveStage::ValueOf;
                    Ok(None)
                } else {
                    Err(primitive_conversion_type_error(
                        &state.origin,
                        "toPrimitive",
                    )?)
                }
            }
        },
    }
}

fn operator_primitive_method_call(
    state: OperatorPrimitiveContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = state.receiver.duplicate();
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::OperatorPrimitive(state));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
    }))
}

fn finish_operator_primitive_target(
    runtime: &mut Runtime,
    value: StoredValue,
    target: OperatorPrimitiveTarget,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        OperatorPrimitiveTarget::Unary { opcode } => apply_unary_operator(opcode, value, origin),
        OperatorPrimitiveTarget::BinaryRight {
            opcode,
            right,
            hint,
        } => {
            let left = if binary_operator_converts_left_to_number_first(opcode) {
                StoredValue::Number(operator_to_number(value, origin)?)
            } else {
                value
            };
            begin_operator_primitive_conversion(
                runtime,
                right,
                hint,
                OperatorPrimitiveTarget::BinaryFinish { opcode, left },
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        OperatorPrimitiveTarget::BinaryFinish { opcode, left } => {
            apply_binary_operator(opcode, left, value, origin)
        }
        OperatorPrimitiveTarget::EqualityFinish { opcode, other } => begin_abstract_equality(
            runtime,
            value,
            other,
            opcode,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        OperatorPrimitiveTarget::NumberIntrinsic { new_target } => {
            let value = operator_to_number(value, origin)?;
            new_target.map_or_else(
                || Ok(NativeDispatch::Immediate(StoredValue::Number(value))),
                |new_target| {
                    begin_number_constructor_wrapper(
                        runtime,
                        new_target,
                        value,
                        return_to,
                        Some(origin.clone()),
                    )
                },
            )
        }
        OperatorPrimitiveTarget::NumberToString { number } => {
            let radix = operator_to_number(value, origin)?;
            finish_number_to_string_radix(number, radix, origin)
        }
        OperatorPrimitiveTarget::StringIntrinsic { new_target } => {
            let value = operator_primitive_to_string(value, origin)?;
            if let Some(new_target) = new_target {
                begin_string_constructor_wrapper(
                    runtime,
                    new_target,
                    value,
                    return_to,
                    Some(origin.clone()),
                )
            } else {
                Ok(NativeDispatch::Immediate(StoredValue::String(value)))
            }
        }
        OperatorPrimitiveTarget::FunctionApplyLength(state) => {
            finish_function_apply_length(runtime, state, value, return_to, execution_budget)
        }
    }
}

fn finish_number_to_string_radix(
    number: JsNumber,
    radix: JsNumber,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let radix = saturated_i32_from_number(radix);
    let Some(radix) = u32::try_from(radix)
        .ok()
        .filter(|radix| (2..=36).contains(radix))
    else {
        return Err(NativeFailure::Abrupt(PendingException {
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("radix must be between 2 and 36")?,
            },
            origin: origin.clone(),
        }));
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        number.to_radix_string(radix)?,
    )))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Rust float-to-int casts exactly provide QuickJS JS_ToInt32Sat semantics: truncation, saturation, and NaN to zero"
)]
fn saturated_i32_from_number(number: JsNumber) -> i32 {
    number.as_f64() as i32
}

const fn binary_operator_converts_left_to_number_first(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::Mul
            | FinalOpcode::Div
            | FinalOpcode::Mod
            | FinalOpcode::Sub
            | FinalOpcode::Pow
            | FinalOpcode::Shl
            | FinalOpcode::Sar
            | FinalOpcode::Shr
            | FinalOpcode::And
            | FinalOpcode::Xor
            | FinalOpcode::Or
    )
}

fn apply_unary_operator(
    opcode: FinalOpcode,
    value: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, origin)?;
    let dispatch = match opcode {
        FinalOpcode::Plus => NativeDispatch::Immediate(StoredValue::Number(number)),
        FinalOpcode::Neg => {
            NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_f64(-number.as_f64())))
        }
        FinalOpcode::Inc => NativeDispatch::Immediate(StoredValue::Number(
            number.add_numeric(JsNumber::from_i32(1)),
        )),
        FinalOpcode::Dec => NativeDispatch::Immediate(StoredValue::Number(
            number.add_numeric(JsNumber::from_i32(-1)),
        )),
        FinalOpcode::PostInc => NativeDispatch::Pair(
            StoredValue::Number(number),
            StoredValue::Number(number.add_numeric(JsNumber::from_i32(1))),
        ),
        FinalOpcode::PostDec => NativeDispatch::Pair(
            StoredValue::Number(number),
            StoredValue::Number(number.add_numeric(JsNumber::from_i32(-1))),
        ),
        FinalOpcode::Not => NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i32(
            !number_to_int32(number),
        ))),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-unary opcode reached unary dynamic-operator execution",
            }
            .into());
        }
    };
    Ok(dispatch)
}

fn apply_binary_operator(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match opcode {
        FinalOpcode::Add => apply_addition(left, right, origin),
        FinalOpcode::Mul
        | FinalOpcode::Div
        | FinalOpcode::Mod
        | FinalOpcode::Sub
        | FinalOpcode::Pow => apply_numeric_arithmetic(opcode, left, right, origin),
        FinalOpcode::Shl
        | FinalOpcode::Sar
        | FinalOpcode::Shr
        | FinalOpcode::And
        | FinalOpcode::Xor
        | FinalOpcode::Or => apply_numeric_bitwise(opcode, left, right, origin),
        FinalOpcode::Lt | FinalOpcode::Lte | FinalOpcode::Gt | FinalOpcode::Gte => {
            apply_relational(opcode, left, right, origin)
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "unsupported opcode reached binary dynamic-operator execution",
        }
        .into()),
    }
}

fn apply_addition(
    left: StoredValue,
    right: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(left, StoredValue::String(_)) || matches!(right, StoredValue::String(_)) {
        let left = operator_primitive_to_string(left, origin)?;
        let right = operator_primitive_to_string(right, origin)?;
        let value = match left.concat(&right) {
            Ok(value) => value,
            Err(JsStringError::TooLong { .. }) => {
                return Err(NativeFailure::Abrupt(PendingException {
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::InternalError,
                        message: JsString::from_utf8("string too long")?,
                    },
                    origin: origin.clone(),
                }));
            }
            Err(error) => return Err(error.into()),
        };
        return Ok(NativeDispatch::Immediate(StoredValue::String(value)));
    }
    let left = operator_to_number(left, origin)?;
    let right = operator_to_number(right, origin)?;
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        left.add_numeric(right),
    )))
}

fn apply_numeric_arithmetic(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let left = operator_to_number(left, origin)?.as_f64();
    let right = operator_to_number(right, origin)?.as_f64();
    let result = match opcode {
        FinalOpcode::Mul => left * right,
        FinalOpcode::Div => left / right,
        FinalOpcode::Mod => left % right,
        FinalOpcode::Sub => left - right,
        FinalOpcode::Pow if !right.is_finite() && left.abs().to_bits() == 1.0_f64.to_bits() => {
            f64::NAN
        }
        FinalOpcode::Pow => left.powf(right),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-arithmetic opcode reached numeric arithmetic",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_f64(result),
    )))
}

fn apply_numeric_bitwise(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let left = operator_to_number(left, origin)?;
    let right = operator_to_number(right, origin)?;
    let shift = number_to_uint32(right) & 0x1f;
    let result = match opcode {
        FinalOpcode::Shl => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left).wrapping_shl(shift),
        )),
        FinalOpcode::Sar => StoredValue::Number(JsNumber::from_i32(number_to_int32(left) >> shift)),
        FinalOpcode::Shr => {
            StoredValue::Number(JsNumber::from_u32(number_to_uint32(left) >> shift))
        }
        FinalOpcode::And => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left) & number_to_int32(right),
        )),
        FinalOpcode::Xor => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left) ^ number_to_int32(right),
        )),
        FinalOpcode::Or => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left) | number_to_int32(right),
        )),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-bitwise opcode reached numeric bitwise execution",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(result))
}

fn apply_relational(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let result = match (left, right) {
        (StoredValue::String(left), StoredValue::String(right)) => match opcode {
            FinalOpcode::Lt => left < right,
            FinalOpcode::Lte => left <= right,
            FinalOpcode::Gt => left > right,
            FinalOpcode::Gte => left >= right,
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "non-relational opcode reached string comparison",
                }
                .into());
            }
        },
        (left, right) => {
            let left = operator_to_number(left, origin)?.as_f64();
            let right = operator_to_number(right, origin)?.as_f64();
            match opcode {
                FinalOpcode::Lt => left < right,
                FinalOpcode::Lte => left <= right,
                FinalOpcode::Gt => left > right,
                FinalOpcode::Gte => left >= right,
                _ => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "non-relational opcode reached numeric comparison",
                    }
                    .into());
                }
            }
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(result)))
}

fn begin_abstract_equality(
    runtime: &mut Runtime,
    mut left: StoredValue,
    mut right: StoredValue,
    opcode: FinalOpcode,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let invert = match opcode {
        FinalOpcode::Eq => false,
        FinalOpcode::Neq => true,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-equality opcode reached abstract equality",
            }
            .into());
        }
    };

    loop {
        if left.kind() == right.kind() || (is_object_value(&left) && is_object_value(&right)) {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                left.strict_equals(&right) ^ invert,
            )));
        }
        if matches!(
            (&left, &right),
            (StoredValue::Null, StoredValue::Undefined)
                | (StoredValue::Undefined, StoredValue::Null)
        ) {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(!invert)));
        }

        match (&left, &right) {
            (StoredValue::String(_), StoredValue::Number(_)) => {
                left = StoredValue::Number(operator_to_number(left, &origin)?);
                continue;
            }
            (StoredValue::Number(_), StoredValue::String(_)) => {
                right = StoredValue::Number(operator_to_number(right, &origin)?);
                continue;
            }
            (StoredValue::Boolean(value), _) => {
                left = StoredValue::Number(JsNumber::from_i32(i32::from(*value)));
                continue;
            }
            (_, StoredValue::Boolean(value)) => {
                right = StoredValue::Number(JsNumber::from_i32(i32::from(*value)));
                continue;
            }
            _ => {}
        }

        if is_object_value(&left) && is_equality_conversion_primitive(&right) {
            return begin_operator_primitive_conversion(
                runtime,
                left,
                OperatorPrimitiveHint::Default,
                OperatorPrimitiveTarget::EqualityFinish {
                    opcode,
                    other: right,
                },
                return_to,
                origin,
                execution_budget,
            );
        }
        if is_object_value(&right) && is_equality_conversion_primitive(&left) {
            return begin_operator_primitive_conversion(
                runtime,
                right,
                OperatorPrimitiveHint::Default,
                OperatorPrimitiveTarget::EqualityFinish {
                    opcode,
                    other: left,
                },
                return_to,
                origin,
                execution_budget,
            );
        }

        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(invert)));
    }
}

const fn is_object_value(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

const fn is_equality_conversion_primitive(value: &StoredValue) -> bool {
    matches!(
        value,
        StoredValue::Number(_) | StoredValue::String(_) | StoredValue::Symbol(_)
    )
}

fn operator_to_number(
    value: StoredValue,
    origin: &JsStackFrame,
) -> Result<JsNumber, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsNumber::from_f64(f64::NAN)),
        StoredValue::Null | StoredValue::Boolean(false) => Ok(JsNumber::from_i32(0)),
        StoredValue::Boolean(true) => Ok(JsNumber::from_i32(1)),
        StoredValue::Number(value) => Ok(value),
        StoredValue::String(value) => Ok(string_to_number(&value)?),
        StoredValue::Symbol(_) => Err(primitive_conversion_type_error(
            origin,
            "cannot convert symbol to number",
        )?),
        StoredValue::Function(_) | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "object reached primitive operator Number conversion",
        }
        .into()),
    }
}

fn operator_primitive_to_string(
    value: StoredValue,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::from_utf8("undefined")?),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(value.to_javascript_string()?),
        StoredValue::String(value) => Ok(value),
        StoredValue::Symbol(_) => Err(primitive_conversion_type_error(
            origin,
            "cannot convert symbol to string",
        )?),
        StoredValue::Function(_) | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "object reached primitive operator String conversion",
        }
        .into()),
    }
}

fn finish_property_key_target(
    runtime: &mut Runtime,
    value: StoredValue,
    target: PropertyKeyTarget,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let value = property_key_primitive_to_value(value)?;
    if matches!(target, PropertyKeyTarget::ToKey) {
        return Ok(NativeDispatch::Immediate(value));
    }
    let property = computed_property_operand(runtime, &value)?;
    match target {
        PropertyKeyTarget::ToKey => Err(EngineFault::RuntimeInvariant {
            message: "property-key conversion lost its ToKey fast path",
        }
        .into()),
        PropertyKeyTarget::Read { base, realm } => {
            match read_static_property(runtime, realm, &base, &property.key)? {
                PropertyReadOutcome::Value(value) => Ok(NativeDispatch::Immediate(value)),
                PropertyReadOutcome::Getter { function, receiver } => {
                    Ok(NativeDispatch::Call(NativeCall {
                        function,
                        receiver,
                        arguments: CallArguments::empty(),
                        return_to,
                        origin: origin.clone(),
                        continuations: Vec::new(),
                    }))
                }
                PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                    property_exception_at(origin.clone(), Some(&property.name), failure)?,
                )),
            }
        }
        PropertyKeyTarget::Write {
            base,
            value,
            strict,
            realm,
        } => match write_static_property(runtime, realm, &base, property.key, value, strict)? {
            PropertyWriteOutcome::Complete => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
            PropertyWriteOutcome::Setter {
                function,
                receiver,
                value,
            } => {
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(value);
                Ok(NativeDispatch::Call(NativeCall {
                    function,
                    receiver,
                    arguments: CallArguments::from_values(arguments),
                    return_to,
                    origin: origin.clone(),
                    continuations: Vec::new(),
                }))
            }
            PropertyWriteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                property_exception_at(origin.clone(), Some(&property.name), failure)?,
            )),
        },
        PropertyKeyTarget::DefineMethod {
            base,
            function,
            kind,
            enumerable,
        } => {
            let StoredValue::Function(function) = function else {
                return Err(NativeFailure::Abrupt(PendingException {
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin: origin.clone(),
                }));
            };
            let name = computed_method_name(&value)?;
            match define_static_method(
                runtime,
                &base,
                property.key,
                &name,
                function,
                kind,
                enumerable,
            )? {
                PropertyDefinitionOutcome::Complete => {
                    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
                }
                PropertyDefinitionOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                    property_exception_at(origin.clone(), Some(&property.name), failure)?,
                )),
            }
        }
    }
}

fn property_key_primitive_to_value(value: StoredValue) -> Result<StoredValue, NativeFailure> {
    Ok(match value {
        StoredValue::Undefined => StoredValue::String(JsString::from_utf8("undefined")?),
        StoredValue::Null => StoredValue::String(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => StoredValue::String(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => StoredValue::String(JsString::from_utf8("true")?),
        StoredValue::Number(value) => StoredValue::String(value.to_javascript_string()?),
        value @ (StoredValue::String(_) | StoredValue::Symbol(_)) => value,
        StoredValue::Function(_) | StoredValue::Object(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "object reached primitive property-key conversion",
            }
            .into());
        }
    })
}

fn computed_property_operand(
    runtime: &mut Runtime,
    value: &StoredValue,
) -> Result<StaticPropertyOperand, ExecutionError> {
    match value {
        StoredValue::String(name) => Ok(StaticPropertyOperand {
            key: runtime.property_key_from_string(name)?,
            name: name.clone(),
        }),
        StoredValue::Symbol(atom) => Ok(StaticPropertyOperand {
            key: runtime.property_key_from_symbol(atom)?,
            name: atom.description().cloned().unwrap_or_else(JsString::empty),
        }),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "computed property operand was not a verified property-key value",
        }
        .into()),
    }
}

fn computed_method_name(value: &StoredValue) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::String(name) => Ok(name.clone()),
        StoredValue::Symbol(atom) => atom.description().map_or_else(
            || Ok(JsString::empty()),
            |description| {
                Ok(JsString::from_utf8("[")?
                    .concat(description)?
                    .concat(&JsString::from_utf8("]")?)?)
            },
        ),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "computed method name was not a verified property-key value",
        }
        .into()),
    }
}

fn primitive_conversion_type_error(
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeFailure, JsStringError> {
    Ok(NativeFailure::Abrupt(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}

fn completed_dynamic_function_source(
    arguments: Vec<StoredValue>,
) -> Result<OrdinaryDynamicFunctionSource, NativeFailure> {
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(arguments.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: arguments.len(),
        })?;
    for argument in arguments {
        let StoredValue::String(argument) = argument else {
            return Err(EngineFault::RuntimeInvariant {
                message: "completed dynamic Function source retained a non-string argument",
            }
            .into());
        };
        converted.push(argument);
    }
    if converted.is_empty() {
        return Ok(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::empty(),
        ));
    }
    let body = converted.pop().ok_or(EngineFault::RuntimeInvariant {
        message: "nonempty dynamic Function arguments lost their body",
    })?;
    Ok(OrdinaryDynamicFunctionSource::new(
        Arc::from(converted),
        body,
    ))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "verified compilation, installation, rollback, and frame admission form one failure-atomic boundary"
)]
fn finish_ordinary_function_constructor(
    runtime: &mut Runtime,
    native: NativeFunction,
    construction: Option<FunctionId>,
    source: OrdinaryDynamicFunctionSource,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_dynamic_compilation(&source)?;
    let authority = match compiler.compile(source) {
        Ok(authority) => authority,
        Err(DynamicFunctionCompileFailure::Syntax { message }) => {
            return Err(NativeFailure::Abrupt(PendingException {
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::SyntaxError,
                    message,
                },
                origin,
            }));
        }
        Err(error @ DynamicFunctionCompileFailure::Engine { .. }) => {
            return Err(NativeFailure::Execution(error.into()));
        }
    };

    let exception_authority = Arc::clone(&authority);
    let installation = {
        let mut context = Context {
            runtime,
            realm: native.realm,
        };
        context.install_dynamic_function_script_during_execution(authority)
    };
    let mut installed = match installation {
        Ok(installed) => installed,
        Err(crate::InstallError::GlobalDeclarationRejected {
            name,
            function,
            pc,
            source_span,
        }) => {
            let (message, declaration_origin) =
                global_declaration_error(&exception_authority, &name, function, pc, source_span)
                    .map_err(NativeFailure::Execution)?;
            return Err(NativeFailure::Abrupt(PendingException {
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message,
                },
                origin: declaration_origin,
            }));
        }
        Err(error) => {
            return Err(NativeFailure::Execution(ExecutionError::from(error)));
        }
    };
    let dynamic_return_values = u64::from(construction.is_some());
    let plan = match plan_frame(
        runtime,
        installed.function,
        active_frames,
        active_frame_values.saturating_add(dynamic_return_values),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(error));
        }
    };
    let global = match runtime.realm_global_object(native.realm) {
        Ok(global) => global,
        Err(fault) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(fault.into()));
        }
    };
    let frame = match create_frame(
        runtime,
        plan,
        StoredValue::Object(global),
        FrameArguments::Owned(CallArguments::empty()),
        return_to,
        None,
    ) {
        Ok(frame) => frame,
        Err(error) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(error));
        }
    };
    if let Err(error) = installed.commit_environment() {
        retire_failed_dynamic_root(runtime, installed)?;
        return Err(NativeFailure::Execution(error.into()));
    }
    let mut frame = frame;
    frame.reserved_values = frame.reserved_values.saturating_add(dynamic_return_values);
    frame.dynamic_return = Some(DynamicFunctionReturn {
        root: installed,
        construction,
        origin: Some(origin),
    });
    Ok(NativeDispatch::Frame(frame))
}

fn begin_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let (reference, default_tag) = match &receiver {
        StoredValue::Undefined => {
            let tag = JsString::from_utf8("Undefined")?;
            return format_object_prototype_to_string(&tag);
        }
        StoredValue::Null => {
            let tag = JsString::from_utf8("Null")?;
            return format_object_prototype_to_string(&tag);
        }
        StoredValue::Boolean(value) => {
            return begin_boxed_boolean_object_prototype_to_string(
                runtime, realm, *value, return_to, origin,
            );
        }
        StoredValue::Number(value) => {
            return begin_boxed_number_object_prototype_to_string(
                runtime, realm, *value, return_to, origin,
            );
        }
        StoredValue::String(value) => {
            return begin_boxed_string_object_prototype_to_string(
                runtime,
                realm,
                value.clone(),
                return_to,
                origin,
            );
        }
        StoredValue::Function(function) => (
            HeapReference::Function(*function),
            ObjectPrototypeTag::Function,
        ),
        StoredValue::Object(object) => (
            HeapReference::Object(*object),
            if runtime.boxed_boolean(*object)?.is_some() {
                ObjectPrototypeTag::Boolean
            } else if runtime.boxed_number(*object)?.is_some() {
                ObjectPrototypeTag::Number
            } else if runtime.boxed_string(*object)?.is_some() {
                ObjectPrototypeTag::String
            } else {
                ObjectPrototypeTag::Object
            },
        ),
        StoredValue::Symbol(_) => {
            return Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "Object.prototype.toString Symbol boxing is not implemented",
                }
                .into(),
            ));
        }
    };
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    begin_intrinsic_get(
        runtime,
        reference,
        receiver,
        &to_string_tag,
        IntrinsicGetContinuation::ObjectPrototypeToString {
            default_tag,
            temporary_receiver: None,
        },
        return_to,
        origin,
    )
}

fn begin_boxed_boolean_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: bool,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_boolean(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::Boolean,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::Boolean, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "Boolean boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn begin_boxed_number_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: JsNumber,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_number(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::Number,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::Number, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "Number boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn begin_boxed_string_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: JsString,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_string(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::String,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::String, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "String boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn remove_unobservable_temporary_wrapper(
    runtime: &mut Runtime,
    temporary: ObjectId,
    collection_pending: bool,
) {
    let removed = runtime.objects.remove(temporary);
    if let Some(object) = removed {
        runtime.object_properties = runtime
            .object_properties
            .saturating_sub(usize_to_u64(object.record.property_count()));
    }
    runtime.collection_pending = collection_pending;
}

fn finish_object_prototype_to_string(
    default_tag: ObjectPrototypeTag,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let tag = match value {
        StoredValue::String(tag) => tag,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => JsString::from_utf8(default_tag.name())?,
    };
    format_object_prototype_to_string(&tag)
}

fn format_object_prototype_to_string(tag: &JsString) -> Result<NativeDispatch, NativeFailure> {
    let value = JsString::from_utf8("[object ")?
        .concat(tag)?
        .concat(&JsString::from_utf8("]")?)?;
    Ok(NativeDispatch::Immediate(StoredValue::String(value)))
}

fn function_to_string(
    runtime: &Runtime,
    function: FunctionId,
    origin: Option<&JsStackFrame>,
) -> Result<JsString, NativeFailure> {
    let node = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        })?;
    if let FunctionImplementation::Bytecode(bytecode) = &node.implementation {
        let installed = code(runtime, bytecode.code)?;
        let function = installed.authority.function(bytecode.template).ok_or(
            EngineFault::InvalidClosureEnvironment {
                function: bytecode.template,
            },
        )?;
        return Ok(JsString::from_utf8(
            function.metadata().source().function_source(),
        )?);
    }

    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let name = native_function_name_to_string(
        read_heap_property(runtime, HeapReference::Function(function), &name_key)?,
        origin,
    )?;
    Ok(JsString::from_utf8("function ")?
        .concat(&name)?
        .concat(&JsString::from_utf8("() {\n    [native code]\n}")?)?)
}

fn native_function_name_to_string(
    value: StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::empty()),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(value.to_javascript_string()?),
        StoredValue::String(value) => Ok(value),
        StoredValue::Symbol(_) => {
            let Some(origin) = origin else {
                return Err(NativeFailure::Execution(
                    EngineFault::RuntimeInvariant {
                        message: "host Symbol-to-string error has no source origin",
                    }
                    .into(),
                ));
            };
            Err(NativeFailure::Abrupt(PendingException {
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("cannot convert symbol to string")?,
                },
                origin: origin.clone(),
            }))
        }
        StoredValue::Function(_) | StoredValue::Object(_) => Err(NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "native function name ToPrimitive is not implemented",
            }
            .into(),
        )),
    }
}

fn native_function_host_origin() -> JsStackFrame {
    JsStackFrame::new(
        FunctionTemplateId::new(0),
        BytecodePc::ZERO,
        Arc::from("<native Function>"),
        Arc::from("Function"),
        SourceByteSpan::new(0, 8),
    )
}

fn retire_failed_dynamic_root(
    runtime: &mut Runtime,
    installed: InstalledRoot,
) -> Result<(), NativeFailure> {
    runtime
        .retire_dynamic_root(installed)
        .map_err(|fault| NativeFailure::Execution(fault.into()))
}

fn dynamic_function_source_code_units(source: &OrdinaryDynamicFunctionSource) -> u64 {
    const FIXED_WRAPPER_CODE_UNITS: u64 = 28;
    let parameter_units = source.parameters().iter().fold(0_u64, |total, parameter| {
        total.saturating_add(u64::from(parameter.len()))
    });
    let separator_units = usize_to_u64(source.parameters().len().saturating_sub(1));
    FIXED_WRAPPER_CODE_UNITS
        .saturating_add(parameter_units)
        .saturating_add(separator_units)
        .saturating_add(u64::from(source.body().len()))
}

fn dynamic_source_primitive_to_string(
    value: StoredValue,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::from_utf8("undefined")?),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(value.to_javascript_string()?),
        StoredValue::String(value) => Ok(value),
        StoredValue::Symbol(_) => Err(NativeFailure::Abrupt(PendingException {
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert symbol to string")?,
            },
            origin: origin.clone(),
        })),
        StoredValue::Function(_) | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "object reached primitive dynamic Function source conversion",
        }
        .into()),
    }
}

fn finish_dynamic_function_return(
    runtime: &mut Runtime,
    caller_frames: &[Frame],
    dynamic: DynamicFunctionReturn,
    value: StoredValue,
) -> Result<StoredValue, ExecutionError> {
    let completion = if let Some(new_target) = dynamic.construction {
        apply_dynamic_constructor_prototype(runtime, new_target, value)
    } else {
        Ok(value)
    };
    let retirement = runtime.retire_dynamic_root(dynamic.root);
    retirement?;
    match completion {
        Ok(value) => Ok(value),
        Err(ConstructorCompletionError::Execution(error)) => Err(error),
        Err(ConstructorCompletionError::TypeError(message)) => {
            let Some(origin) = dynamic.origin else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "host dynamic construction has no verified exception origin",
                }
                .into());
            };
            let callers = exception_caller_frames(runtime, caller_frames)?;
            let pending = PendingException {
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message,
                },
                origin,
            };
            let exception = finish_exception(runtime, pending, callers)?;
            Err(ExecutionError::Exception(exception))
        }
    }
}

enum ConstructorCompletionError {
    TypeError(JsString),
    Execution(ExecutionError),
}

impl From<ExecutionError> for ConstructorCompletionError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<EngineFault> for ConstructorCompletionError {
    fn from(error: EngineFault) -> Self {
        Self::Execution(error.into())
    }
}

impl From<JsStringError> for ConstructorCompletionError {
    fn from(error: JsStringError) -> Self {
        Self::Execution(error.into())
    }
}

fn apply_dynamic_constructor_prototype(
    runtime: &mut Runtime,
    new_target: FunctionId,
    completion: StoredValue,
) -> Result<StoredValue, ConstructorCompletionError> {
    let target = match &completion {
        StoredValue::Undefined | StoredValue::Null => {
            return Err(ConstructorCompletionError::TypeError(JsString::from_utf8(
                "not an object",
            )?));
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(completion);
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let requested =
        read_heap_property(runtime, HeapReference::Function(new_target), &prototype_key)?;
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(function),
        StoredValue::Object(object) => HeapReference::Object(object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Function(runtime.realm_function_prototype(realm)?)
        }
    };
    if !runtime.replace_prototype_checked(target, Some(prototype))? {
        return Err(ConstructorCompletionError::TypeError(JsString::from_utf8(
            "circular prototype chain",
        )?));
    }
    Ok(completion)
}

fn bytecode_function_is_constructor(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<bool, ExecutionError> {
    let bytecode = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        })?
        .bytecode()?;
    let template = code(runtime, bytecode.code)?
        .authority
        .function(bytecode.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        })?;
    Ok(template
        .function()
        .control_flow()
        .function_header()
        .flags()
        .has_prototype())
}

fn create_ordinary_constructor_receiver(
    runtime: &mut Runtime,
    new_target: FunctionId,
) -> Result<ObjectId, ExecutionError> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let requested =
        read_heap_property(runtime, HeapReference::Function(new_target), &prototype_key)?;
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(function),
        StoredValue::Object(object) => HeapReference::Object(object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_object_prototype(realm)?)
        }
    };
    runtime.allocate_ordinary_object_with_prototype(prototype)
}

fn retire_active_dynamic_roots(
    runtime: &mut Runtime,
    frames: &mut [Frame],
) -> Result<(), EngineFault> {
    let mut first_failure = None;
    for dynamic in frames
        .iter_mut()
        .rev()
        .filter_map(|frame| frame.dynamic_return.take())
    {
        if let Err(fault) = runtime.retire_dynamic_root(dynamic.root)
            && first_failure.is_none()
        {
            first_failure = Some(fault);
        }
    }
    first_failure.map_or(Ok(()), Err)
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
        parent.stack.push(value);
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
    parent.stack.push(original);
    parent.stack.push(updated);
    parent.instruction = return_to.instruction;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "verified environment validation and cumulative frame-budget planning remain one read-only transaction"
)]
fn plan_frame(
    runtime: &Runtime,
    function_id: FunctionId,
    active_frames: usize,
    active_frame_values: u64,
) -> Result<FramePlan, ExecutionError> {
    let observed_frames = usize_to_u64(active_frames).saturating_add(1);
    check_execution_limit(
        RuntimeResource::Frames,
        u64::from(runtime.limits.max_active_frames),
        observed_frames,
    )?;

    let function = runtime
        .functions
        .get(function_id)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function_id.index(),
            generation: function_id.generation(),
        })?;
    let bytecode = function.bytecode()?;
    let code_id = bytecode.code;
    let template_id = bytecode.template;

    let code = runtime
        .code
        .get(code_id)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: code_id.index(),
            generation: code_id.generation(),
        })?;
    if !runtime.contains_realm(code.realm) {
        return Err(EngineFault::StaleHeapEdge {
            edge: "realm",
            index: code.realm.index(),
            generation: code.realm.generation(),
        }
        .into());
    }
    let verified =
        code.authority
            .function(template_id)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: template_id,
            })?;
    if bytecode.environment.len() != verified.metadata().closures().len() {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: template_id,
        }
        .into());
    }
    for (binding, definition) in bytecode
        .environment
        .iter()
        .copied()
        .zip(verified.metadata().closures())
    {
        match (binding, definition.binding()) {
            (EnvironmentBinding::Captured(cell), CompilerClosureBinding::Captured(_)) => {
                if !runtime.cells.contains(cell) {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "closure cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    }
                    .into());
                }
            }
            (EnvironmentBinding::RealmGlobal(global), CompilerClosureBinding::RealmGlobal(_)) => {
                let valid = runtime
                    .global_bindings
                    .get(global)
                    .is_some_and(|binding| binding.realm == code.realm);
                if !valid {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "realm global binding",
                        index: global.index(),
                        generation: global.generation(),
                    }
                    .into());
                }
            }
            (
                EnvironmentBinding::Captured(_) | EnvironmentBinding::RealmGlobal(_),
                CompilerClosureBinding::Captured(_) | CompilerClosureBinding::RealmGlobal(_),
            ) => {
                return Err(EngineFault::InvalidClosureEnvironment {
                    function: template_id,
                }
                .into());
            }
        }
    }

    let control_flow = verified.function().control_flow();
    let domains = control_flow.domains();
    let argument_count = domains.argument_count() as usize;
    let local_count = domains.local_count() as usize;
    let stack_capacity = control_flow.computed_stack_size() as usize;
    let frame_values = argument_count
        .checked_add(local_count)
        .and_then(|value| value.checked_add(stack_capacity))
        .and_then(|value| value.checked_add(1))
        .map_or(u64::MAX, usize_to_u64);
    let observed_frame_values = active_frame_values.saturating_add(frame_values);
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        observed_frame_values,
    )?;

    let installed_index =
        usize::try_from(template_id.get()).map_err(|_| EngineFault::InvalidClosureEnvironment {
            function: template_id,
        })?;
    if code.templates.get(installed_index).is_none() {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: template_id,
        }
        .into());
    }
    let instruction = control_flow.instruction_index_at(BytecodePc::ZERO).ok_or(
        EngineFault::MissingInstruction {
            function: template_id,
            instruction: 0,
        },
    )?;
    let (strict, receiver_access) = receiver_profile(&verified);
    Ok(FramePlan {
        function: function_id,
        code: code_id,
        template: template_id,
        argument_count,
        local_count,
        stack_capacity,
        reserved_values: frame_values,
        strict,
        receiver_access,
        instruction,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "failure-atomic frame allocation and initialization remain one transaction"
)]
fn create_frame(
    runtime: &mut Runtime,
    plan: FramePlan,
    receiver: StoredValue,
    supplied: FrameArguments<'_>,
    return_to: Option<CallReturn>,
    dynamic_return: Option<DynamicFunctionReturn>,
) -> Result<Frame, ExecutionError> {
    let function = runtime
        .functions
        .get(plan.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: plan.function.index(),
            generation: plan.function.generation(),
        })?;
    let environment = copy_environment(
        &function.bytecode()?.environment,
        RuntimeResource::FrameValues,
    )?;
    let code = runtime
        .code
        .get(plan.code)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: plan.code.index(),
            generation: plan.code.generation(),
        })?;

    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(plan.argument_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.argument_count,
        })?;
    match supplied {
        FrameArguments::Public(supplied) => {
            for index in 0..plan.argument_count {
                let value = supplied
                    .get(index)
                    .map(JsValue::stored)
                    .transpose()?
                    .map_or(StoredValue::Undefined, StoredValue::duplicate);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
        }
        FrameArguments::Owned(supplied) => {
            let mut supplied = supplied.into_remaining_iter();
            for _ in 0..plan.argument_count {
                let value = supplied.next().unwrap_or(StoredValue::Undefined);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
        }
    }

    let mut locals = Vec::new();
    locals
        .try_reserve_exact(plan.local_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.local_count,
        })?;
    for _ in 0..plan.local_count {
        locals.push(FrameBinding::Direct(SlotValue::Value(
            StoredValue::Undefined,
        )));
    }

    let verified =
        code.authority
            .function(plan.template)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: plan.template,
            })?;
    let variable_count = plan.argument_count.checked_add(plan.local_count).ok_or(
        EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        },
    )?;
    if verified.metadata().variables().len() != variable_count {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        }
        .into());
    }
    for (local, definition) in verified
        .metadata()
        .variables()
        .iter()
        .skip(plan.argument_count)
        .enumerate()
    {
        if definition.policy().kind() == CompilerBindingKind::FunctionName {
            let binding = locals
                .get_mut(local)
                .ok_or(EngineFault::InvalidClosureEnvironment {
                    function: plan.template,
                })?;
            *binding = FrameBinding::Direct(SlotValue::Value(StoredValue::Function(plan.function)));
        }
    }

    let installed_index = usize::try_from(plan.template.get()).map_err(|_| {
        EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        }
    })?;
    let installed =
        code.templates
            .get(installed_index)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: plan.template,
            })?;
    let own_cell_bindings = copy_addresses(&installed.own_cell_bindings)?;
    let mut own_cells = Vec::new();
    own_cells
        .try_reserve_exact(own_cell_bindings.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: own_cell_bindings.len(),
        })?;
    own_cells.resize(own_cell_bindings.len(), None);

    let mut stack = Vec::new();
    stack
        .try_reserve_exact(plan.stack_capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.stack_capacity,
        })?;

    let receiver = normalize_receiver(runtime, code.realm, plan.receiver_access, receiver)?;

    Ok(Frame {
        function: plan.function,
        code: plan.code,
        template: plan.template,
        strict: plan.strict,
        receiver,
        instruction: plan.instruction,
        return_to,
        dynamic_return,
        native_returns: Vec::new(),
        transient_cleanup_pending: false,
        ordinary_constructor: false,
        reserved_values: plan.reserved_values,
        arguments,
        locals,
        own_cells,
        own_cell_bindings,
        environment,
        stack,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive admitted-opcode dispatcher is intentionally centralized"
)]
fn execute_one(
    runtime: &mut Runtime,
    frame: &mut Frame,
    execution_budget: &mut ExecutionBudget,
) -> Result<Step, ExecutionError> {
    let (verified_instruction, source_pc) = {
        let code = code(runtime, frame.code)?;
        let function = code.authority.function(frame.template).ok_or(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        )?;
        let instruction = function
            .function()
            .control_flow()
            .instruction(frame.instruction)
            .copied()
            .ok_or(EngineFault::MissingInstruction {
                function: frame.template,
                instruction: frame.instruction.get(),
            })?;
        (instruction, instruction.decoded().pc())
    };

    let expected_depth =
        verified_instruction
            .entry_stack_depth()
            .ok_or(EngineFault::UnreachableInstruction {
                function: frame.template,
                pc: source_pc,
            })?;
    if frame.stack.len() != expected_depth as usize {
        return Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: source_pc,
            expected: expected_depth,
            actual: frame.stack.len(),
        }
        .into());
    }

    let instruction = verified_instruction.decoded().instruction();
    let opcode = instruction.opcode();
    let operands = instruction.operands();

    match opcode {
        FinalOpcode::PushI32 => {
            let Operands::I32(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            frame.stack.push(StoredValue::Number(value.into()));
        }
        FinalOpcode::PushMinus1
        | FinalOpcode::Push0
        | FinalOpcode::Push1
        | FinalOpcode::Push2
        | FinalOpcode::Push3
        | FinalOpcode::Push4
        | FinalOpcode::Push5
        | FinalOpcode::Push6
        | FinalOpcode::Push7 => {
            let value =
                implied_integer(opcode).ok_or(EngineFault::UnsupportedDispatch { opcode })?;
            frame.stack.push(StoredValue::Number(value.into()));
        }
        FinalOpcode::PushI8 => {
            let Operands::I8(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            frame
                .stack
                .push(StoredValue::Number(i32::from(value).into()));
        }
        FinalOpcode::PushI16 => {
            let Operands::I16(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            frame
                .stack
                .push(StoredValue::Number(i32::from(value).into()));
        }
        FinalOpcode::PushConst | FinalOpcode::PushConst8 => {
            let index = constant_index(operands).ok_or(EngineFault::MissingPoolEntry {
                pool: "constant",
                index: u32::MAX,
            })?;
            frame.stack.push(materialize_constant(
                runtime,
                frame.code,
                frame.template,
                index,
            )?);
        }
        FinalOpcode::PushAtomValue => {
            let Operands::Atom(index) = operands else {
                return unsupported_dispatch(opcode);
            };
            let string = {
                let installed = installed_template(runtime, frame.code, frame.template)?;
                installed
                    .atoms
                    .get(index.get() as usize)
                    .and_then(AtomDescription::description)
                    .cloned()
                    .ok_or(EngineFault::MissingPoolEntry {
                        pool: "atom",
                        index: index.get(),
                    })?
            };
            frame.stack.push(StoredValue::String(string));
        }
        FinalOpcode::PushEmptyString => {
            frame.stack.push(StoredValue::String(JsString::empty()));
        }
        FinalOpcode::Undefined => frame.stack.push(StoredValue::Undefined),
        FinalOpcode::Null => frame.stack.push(StoredValue::Null),
        FinalOpcode::PushThis => {
            frame.stack.push(frame.receiver.duplicate());
        }
        FinalOpcode::PushFalse => frame.stack.push(StoredValue::Boolean(false)),
        FinalOpcode::PushTrue => frame.stack.push(StoredValue::Boolean(true)),
        FinalOpcode::Object => {
            let realm = code(runtime, frame.code)?.realm;
            let prototype = runtime.realm_object_prototype(realm)?;
            let object = runtime.allocate_ordinary_object(prototype)?;
            frame.stack.push(StoredValue::Object(object));
        }
        FinalOpcode::Drop => {
            pop(frame)?;
        }
        FinalOpcode::Nip => {
            let top = pop(frame)?;
            pop(frame)?;
            frame.stack.push(top);
        }
        FinalOpcode::Dup => {
            let value = frame
                .stack
                .last()
                .ok_or(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: 1,
                    actual: 0,
                })?
                .duplicate();
            frame.stack.push(value);
        }
        FinalOpcode::Insert2 => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            frame.stack.push(right.duplicate());
            frame.stack.push(left);
            frame.stack.push(right);
        }
        FinalOpcode::Insert3 => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            frame.stack.push(third.duplicate());
            frame.stack.push(first);
            frame.stack.push(second);
            frame.stack.push(third);
        }
        FinalOpcode::Swap => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            frame.stack.push(right);
            frame.stack.push(left);
        }
        FinalOpcode::Rot3l => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            frame.stack.push(second);
            frame.stack.push(third);
            frame.stack.push(first);
        }
        FinalOpcode::Call
        | FinalOpcode::Call0
        | FinalOpcode::Call1
        | FinalOpcode::Call2
        | FinalOpcode::Call3 => {
            let argument_count = direct_call_argument_count(opcode, operands)?;
            let required = argument_count.saturating_add(1);
            if frame.stack.len() < required {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::try_from(required).unwrap_or(u32::MAX),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - required;
            let StoredValue::Function(function) = &frame.stack[callee_index] else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Call {
                function: *function,
                inputs: CallInputSource::Frame {
                    argument_count,
                    kind: CallKind::Direct,
                },
                return_to,
                source_pc,
            });
        }
        FinalOpcode::CallMethod => {
            let Operands::NPop { argument_count } = operands else {
                return unsupported_dispatch(opcode);
            };
            let argument_count = usize::from(argument_count);
            let required = argument_count.saturating_add(2);
            if frame.stack.len() < required {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::try_from(required).unwrap_or(u32::MAX),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - argument_count - 1;
            let StoredValue::Function(function) = &frame.stack[callee_index] else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Call {
                function: *function,
                inputs: CallInputSource::Frame {
                    argument_count,
                    kind: CallKind::Method,
                },
                return_to,
                source_pc,
            });
        }
        FinalOpcode::CallConstructor => {
            let Operands::NPop { argument_count } = operands else {
                return unsupported_dispatch(opcode);
            };
            let argument_count = usize::from(argument_count);
            let required = argument_count.saturating_add(2);
            if frame.stack.len() < required {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::try_from(required).unwrap_or(u32::MAX),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - required;
            let new_target_index = callee_index + 1;
            let (StoredValue::Function(function), StoredValue::Function(_new_target)) =
                (&frame.stack[callee_index], &frame.stack[new_target_index])
            else {
                return Ok(Step::Abrupt(not_constructor_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Call {
                function: *function,
                inputs: CallInputSource::Frame {
                    argument_count,
                    kind: CallKind::Constructor,
                },
                return_to,
                source_pc,
            });
        }
        FinalOpcode::FClosure | FinalOpcode::FClosure8 => {
            let index = constant_index(operands).ok_or(EngineFault::MissingPoolEntry {
                pool: "function constant",
                index: u32::MAX,
            })?;
            let child = function_constant(runtime, frame.code, frame.template, index)?;
            let function = create_closure(runtime, frame, child)?;
            frame.stack.push(StoredValue::Function(function));
        }
        FinalOpcode::GetArrayEl | FinalOpcode::GetArrayEl2 => {
            let realm = code(runtime, frame.code)?.realm;
            let key = pop(frame)?;
            let base = if opcode == FinalOpcode::GetArrayEl {
                pop(frame)?
            } else {
                peek(frame)?.duplicate()
            };
            let origin = instruction_location(runtime, frame, source_pc)?;
            let nullish_failure = match base {
                StoredValue::Null => Some(PropertyFailure::ReadNull),
                StoredValue::Undefined => Some(PropertyFailure::ReadUndefined),
                StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Function(_)
                | StoredValue::Object(_) => None,
            };
            if let Some(failure) = nullish_failure {
                return Ok(Step::Abrupt(property_exception_at(origin, None, failure)?));
            }
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    key,
                    PropertyKeyTarget::Read { base, realm },
                    Some(return_to),
                    origin,
                ),
                return_to,
            );
        }
        FinalOpcode::PutArrayEl => {
            let realm = code(runtime, frame.code)?.realm;
            let value = pop(frame)?;
            let key = pop(frame)?;
            let base = pop(frame)?;
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    key,
                    PropertyKeyTarget::Write {
                        base,
                        value,
                        strict: frame.strict,
                        realm,
                    },
                    Some(return_to),
                    origin,
                ),
                return_to,
            );
        }
        FinalOpcode::ToPropKey => {
            let value = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    value,
                    PropertyKeyTarget::ToKey,
                    Some(return_to),
                    origin,
                ),
                return_to,
            );
        }
        FinalOpcode::DefineArrayEl => {
            let value = pop(frame)?;
            let key_value = pop(frame)?;
            let base = peek(frame)?.duplicate();
            let property = computed_property_operand(runtime, &key_value)?;
            if let PropertyWriteOutcome::Failed(failure) =
                define_static_property(runtime, &base, property.key, value)?
            {
                return Ok(Step::Abrupt(property_exception_at(
                    instruction_location(runtime, frame, source_pc)?,
                    Some(&property.name),
                    failure,
                )?));
            }
            frame.stack.push(key_value);
        }
        FinalOpcode::DefineMethodComputed => {
            let method = define_method_computed_operand(operands)?;
            let function = pop(frame)?;
            let key = pop(frame)?;
            let base = peek(frame)?.duplicate();
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    key,
                    PropertyKeyTarget::DefineMethod {
                        base,
                        function,
                        kind: method.kind,
                        enumerable: method.enumerable,
                    },
                    Some(return_to),
                    origin,
                ),
                return_to,
            );
        }
        FinalOpcode::GetField | FinalOpcode::GetField2 => {
            let realm = code(runtime, frame.code)?.realm;
            let property = static_property_operand(runtime, frame, operands)?;
            let base = if opcode == FinalOpcode::GetField {
                pop(frame)?
            } else {
                peek(frame)?.duplicate()
            };
            match read_static_property(runtime, realm, &base, &property.key)? {
                PropertyReadOutcome::Value(value) => frame.stack.push(value),
                PropertyReadOutcome::Getter { function, receiver } => {
                    let return_to =
                        CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                            EngineFault::InvalidSuccessor {
                                function: frame.template,
                                pc: source_pc,
                            },
                        )?);
                    return Ok(Step::Call {
                        function,
                        inputs: CallInputSource::Prepared(CallInputs {
                            receiver,
                            arguments: CallArguments::empty(),
                            new_target: None,
                        }),
                        return_to,
                        source_pc,
                    });
                }
                PropertyReadOutcome::Failed(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime,
                        frame,
                        source_pc,
                        &property.name,
                        failure,
                    )?));
                }
            }
        }
        FinalOpcode::PutField => {
            let realm = code(runtime, frame.code)?.realm;
            let property = static_property_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let base = pop(frame)?;
            match write_static_property(runtime, realm, &base, property.key, value, frame.strict)? {
                PropertyWriteOutcome::Complete => {}
                PropertyWriteOutcome::Setter {
                    function,
                    receiver,
                    value,
                } => {
                    let mut arguments = Vec::new();
                    arguments.try_reserve_exact(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: 1,
                        }
                    })?;
                    arguments.push(value);
                    let return_to = CallReturn::discard(
                        verified_instruction.successors().fallthrough().ok_or(
                            EngineFault::InvalidSuccessor {
                                function: frame.template,
                                pc: source_pc,
                            },
                        )?,
                    );
                    return Ok(Step::Call {
                        function,
                        inputs: CallInputSource::Prepared(CallInputs {
                            receiver,
                            arguments: CallArguments::from_values(arguments),
                            new_target: None,
                        }),
                        return_to,
                        source_pc,
                    });
                }
                PropertyWriteOutcome::Failed(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime,
                        frame,
                        source_pc,
                        &property.name,
                        failure,
                    )?));
                }
            }
        }
        FinalOpcode::DefineField => {
            let property = static_property_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let base = peek(frame)?.duplicate();
            if let PropertyWriteOutcome::Failed(failure) =
                define_static_property(runtime, &base, property.key, value)?
            {
                return Ok(Step::Abrupt(property_exception(
                    runtime,
                    frame,
                    source_pc,
                    &property.name,
                    failure,
                )?));
            }
        }
        FinalOpcode::DefineMethod => {
            let method = define_method_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let StoredValue::Function(function) = value else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let base = peek(frame)?.duplicate();
            match define_static_method(
                runtime,
                &base,
                method.property.key,
                &method.property.name,
                function,
                method.kind,
                method.enumerable,
            )? {
                PropertyDefinitionOutcome::Complete => {}
                PropertyDefinitionOutcome::Failed(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime,
                        frame,
                        source_pc,
                        &method.property.name,
                        failure,
                    )?));
                }
            }
        }
        FinalOpcode::GetArg
        | FinalOpcode::GetArg0
        | FinalOpcode::GetArg1
        | FinalOpcode::GetArg2
        | FinalOpcode::GetArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = duplicate_binding(runtime, frame_argument(frame, index)?, false, frame)?;
            frame.stack.push(value);
        }
        FinalOpcode::PutArg
        | FinalOpcode::PutArg0
        | FinalOpcode::PutArg1
        | FinalOpcode::PutArg2
        | FinalOpcode::PutArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = pop(frame)?;
            write_argument(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetArg
        | FinalOpcode::SetArg0
        | FinalOpcode::SetArg1
        | FinalOpcode::SetArg2
        | FinalOpcode::SetArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = peek(frame)?.duplicate();
            write_argument(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::GetVarUndef | FinalOpcode::GetVar => {
            let index = closure_index(opcode, operands)?;
            let global = global_reference_operand(runtime, frame, index)?;
            match read_realm_global(runtime, &global)? {
                RealmGlobalReadOutcome::Value(value) => frame.stack.push(value),
                RealmGlobalReadOutcome::Missing if opcode == FinalOpcode::GetVarUndef => {
                    frame.stack.push(StoredValue::Undefined);
                }
                RealmGlobalReadOutcome::Missing => {
                    return Ok(Step::Abrupt(global_not_defined_exception(
                        runtime,
                        frame,
                        &global.name,
                        source_pc,
                    )?));
                }
            }
        }
        FinalOpcode::PutVar => {
            let index = closure_index(opcode, operands)?;
            let global = global_reference_operand(runtime, frame, index)?;
            let name = global.name.clone();
            let value = pop(frame)?;
            match write_realm_global(runtime, global, value, frame.strict)? {
                RealmGlobalWriteOutcome::Complete => {}
                RealmGlobalWriteOutcome::Missing => {
                    return Ok(Step::Abrupt(global_not_defined_exception(
                        runtime, frame, &name, source_pc,
                    )?));
                }
                RealmGlobalWriteOutcome::Property(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime, frame, source_pc, &name, failure,
                    )?));
                }
            }
        }
        FinalOpcode::GetLoc
        | FinalOpcode::GetLoc8
        | FinalOpcode::GetLoc0
        | FinalOpcode::GetLoc1
        | FinalOpcode::GetLoc2
        | FinalOpcode::GetLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = duplicate_binding(runtime, frame_local(frame, index)?, false, frame)?;
            frame.stack.push(value);
        }
        FinalOpcode::PutLoc
        | FinalOpcode::PutLoc8
        | FinalOpcode::PutLoc0
        | FinalOpcode::PutLoc1
        | FinalOpcode::PutLoc2
        | FinalOpcode::PutLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = pop(frame)?;
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetLoc
        | FinalOpcode::SetLoc8
        | FinalOpcode::SetLoc0
        | FinalOpcode::SetLoc1
        | FinalOpcode::SetLoc2
        | FinalOpcode::SetLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = peek(frame)?.duplicate();
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::GetVarRef
        | FinalOpcode::GetVarRef0
        | FinalOpcode::GetVarRef1
        | FinalOpcode::GetVarRef2
        | FinalOpcode::GetVarRef3 => {
            let index = closure_index(opcode, operands)?;
            frame
                .stack
                .push(duplicate_environment(runtime, frame, index, false)?);
        }
        FinalOpcode::PutVarRef
        | FinalOpcode::PutVarRef0
        | FinalOpcode::PutVarRef1
        | FinalOpcode::PutVarRef2
        | FinalOpcode::PutVarRef3 => {
            let index = closure_index(opcode, operands)?;
            let value = pop(frame)?;
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetVarRef
        | FinalOpcode::SetVarRef0
        | FinalOpcode::SetVarRef1
        | FinalOpcode::SetVarRef2
        | FinalOpcode::SetVarRef3 => {
            let index = closure_index(opcode, operands)?;
            let value = peek(frame)?.duplicate();
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetLocUninitialized => {
            let index = local_index(opcode, operands)?;
            write_local(runtime, frame, index, SlotValue::Uninitialized)?;
        }
        FinalOpcode::GetLocCheck => {
            let index = local_index(opcode, operands)?;
            let value = match duplicate_binding(runtime, frame_local(frame, index)?, true, frame) {
                Ok(value) => value,
                Err(BindingAccessError::Uninitialized) => {
                    return Ok(Step::Abrupt(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Local(index),
                        source_pc,
                    )?));
                }
                Err(BindingAccessError::Fault(fault)) => return Err(fault.into()),
            };
            frame.stack.push(value);
        }
        FinalOpcode::PutLocCheck => {
            let index = local_index(opcode, operands)?;
            if binding_is_uninitialized(runtime, frame_local(frame, index)?)? {
                return Ok(Step::Abrupt(tdz_exception(
                    runtime,
                    frame,
                    BindingName::Local(index),
                    source_pc,
                )?));
            }
            let value = pop(frame)?;
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetLocCheck => {
            let index = local_index(opcode, operands)?;
            if binding_is_uninitialized(runtime, frame_local(frame, index)?)? {
                return Ok(Step::Abrupt(tdz_exception(
                    runtime,
                    frame,
                    BindingName::Local(index),
                    source_pc,
                )?));
            }
            let value = peek(frame)?.duplicate();
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::GetVarRefCheck => {
            let index = closure_index(opcode, operands)?;
            let value = match duplicate_environment(runtime, frame, index, true) {
                Ok(value) => value,
                Err(BindingAccessError::Uninitialized) => {
                    return Ok(Step::Abrupt(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Closure(index),
                        source_pc,
                    )?));
                }
                Err(BindingAccessError::Fault(fault)) => return Err(fault.into()),
            };
            frame.stack.push(value);
        }
        FinalOpcode::PutVarRefCheck => {
            let index = closure_index(opcode, operands)?;
            if environment_is_uninitialized(runtime, frame, index)? {
                return Ok(Step::Abrupt(tdz_exception(
                    runtime,
                    frame,
                    BindingName::Closure(index),
                    source_pc,
                )?));
            }
            let value = pop(frame)?;
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::CloseLoc => {
            let index = local_index(opcode, operands)?;
            close_local(runtime, frame, index)?;
        }
        FinalOpcode::ForInStart => {
            let work = runtime.preview_for_in_iterator_work(peek(frame)?)?;
            execution_budget.charge_instructions(work)?;
            let value = pop(frame)?;
            let realm = code(runtime, frame.code)?.realm;
            let (iterator, actual_work) = runtime.allocate_for_in_iterator(realm, value)?;
            debug_assert!(actual_work <= work);
            frame.stack.push(StoredValue::Object(iterator));
        }
        FinalOpcode::ForInNext => {
            let iterator = match frame.stack.last() {
                Some(StoredValue::Object(object)) if runtime.is_for_in_iterator(*object)? => {
                    *object
                }
                Some(
                    StoredValue::Undefined
                    | StoredValue::Null
                    | StoredValue::Boolean(_)
                    | StoredValue::Number(_)
                    | StoredValue::String(_)
                    | StoredValue::Symbol(_)
                    | StoredValue::Function(_)
                    | StoredValue::Object(_),
                ) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "verified for_in_next cursor is not a for-in iterator",
                    }
                    .into());
                }
                None => {
                    return Err(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: 1,
                        actual: 0,
                    }
                    .into());
                }
            };
            loop {
                let work = runtime.preview_for_in_advance_work(iterator)?;
                execution_budget.charge_instructions(work)?;
                let advance = runtime.advance_for_in_iterator(iterator)?;
                debug_assert!(advance.work() <= work);
                match advance {
                    ForInAdvance::Continue { .. } => {}
                    ForInAdvance::Yield { key, .. } => {
                        frame.stack.push(for_in_key_value(&key)?);
                        frame.stack.push(StoredValue::Boolean(false));
                        break;
                    }
                    ForInAdvance::Done { .. } => {
                        frame.stack.push(StoredValue::Undefined);
                        frame.stack.push(StoredValue::Boolean(true));
                        break;
                    }
                }
            }
        }
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => {
            let condition = pop(frame)?;
            frame.instruction =
                branch_successor(verified_instruction, !condition.is_truthy(), frame)?;
            return Ok(Step::Continue);
        }
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8 => {
            let condition = pop(frame)?;
            frame.instruction =
                branch_successor(verified_instruction, condition.is_truthy(), frame)?;
            return Ok(Step::Continue);
        }
        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16 => {
            frame.instruction = verified_instruction.successors().jump_target().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::Continue);
        }
        FinalOpcode::Neg
        | FinalOpcode::Plus
        | FinalOpcode::Dec
        | FinalOpcode::Inc
        | FinalOpcode::PostDec
        | FinalOpcode::PostInc
        | FinalOpcode::Not => {
            let value = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::Unary { opcode },
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::Mul
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
        | FinalOpcode::And
        | FinalOpcode::Xor
        | FinalOpcode::Or => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            let dispatch = if matches!(opcode, FinalOpcode::Eq | FinalOpcode::Neq) {
                begin_abstract_equality(
                    runtime,
                    left,
                    right,
                    opcode,
                    Some(return_to),
                    origin,
                    execution_budget,
                )
            } else {
                let hint = if opcode == FinalOpcode::Add {
                    OperatorPrimitiveHint::Default
                } else {
                    OperatorPrimitiveHint::Number
                };
                begin_operator_primitive_conversion(
                    runtime,
                    left,
                    hint,
                    OperatorPrimitiveTarget::BinaryRight {
                        opcode,
                        right,
                        hint,
                    },
                    Some(return_to),
                    origin,
                    execution_budget,
                )
            };
            return native_step(dispatch, return_to);
        }
        FinalOpcode::Lnot => {
            let value = pop(frame)?;
            frame.stack.push(StoredValue::Boolean(!value.is_truthy()));
        }
        FinalOpcode::Typeof => {
            let value = pop(frame)?;
            let name = match value {
                StoredValue::Undefined => "undefined",
                StoredValue::Null | StoredValue::Object(_) => "object",
                StoredValue::Boolean(_) => "boolean",
                StoredValue::Number(_) => "number",
                StoredValue::String(_) => "string",
                StoredValue::Symbol(_) => "symbol",
                StoredValue::Function(_) => "function",
            };
            frame
                .stack
                .push(StoredValue::String(JsString::from_utf8(name)?));
        }
        FinalOpcode::StrictEq | FinalOpcode::StrictNeq => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            let equal = left.strict_equals(&right);
            frame
                .stack
                .push(StoredValue::Boolean(if opcode == FinalOpcode::StrictEq {
                    equal
                } else {
                    !equal
                }));
        }
        FinalOpcode::IsUndefinedOrNull => {
            let value = pop(frame)?;
            frame.stack.push(StoredValue::Boolean(matches!(
                value,
                StoredValue::Undefined | StoredValue::Null
            )));
        }
        FinalOpcode::Throw => {
            let origin = instruction_location(runtime, frame, source_pc)?;
            let value = pop(frame)?;
            return Ok(Step::Abrupt(PendingException {
                payload: PendingExceptionPayload::ThrownValue(value),
                origin,
            }));
        }
        FinalOpcode::Return => return Ok(Step::Return(pop(frame)?)),
        FinalOpcode::ReturnUndef => return Ok(Step::Return(StoredValue::Undefined)),
        FinalOpcode::Nop => {}
        _ => return unsupported_dispatch(opcode),
    }

    frame.instruction =
        verified_instruction
            .successors()
            .fallthrough()
            .ok_or(EngineFault::InvalidSuccessor {
                function: frame.template,
                pc: source_pc,
            })?;
    Ok(Step::Continue)
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

trait AtomDescription {
    fn description(&self) -> Option<&JsString>;
}

impl AtomDescription for crate::Atom {
    fn description(&self) -> Option<&JsString> {
        crate::Atom::description(self)
    }
}

struct StaticPropertyOperand {
    key: PropertyKey,
    name: JsString,
}

#[derive(Clone, Copy)]
enum DefineMethodKind {
    Method,
    Getter,
    Setter,
}

struct DefineMethodOperand {
    property: StaticPropertyOperand,
    kind: DefineMethodKind,
    enumerable: bool,
}

struct DefineMethodComputedOperand {
    kind: DefineMethodKind,
    enumerable: bool,
}

struct GlobalReferenceOperand {
    binding: RealmGlobalBindingId,
    realm: RealmId,
    object: ObjectId,
    key: PropertyKey,
    name: JsString,
}

enum PropertyReadOutcome {
    Value(StoredValue),
    Getter {
        function: FunctionId,
        receiver: StoredValue,
    },
    Failed(PropertyFailure),
}

enum PropertyWriteOutcome {
    Complete,
    Setter {
        function: FunctionId,
        receiver: StoredValue,
        value: StoredValue,
    },
    Failed(PropertyFailure),
}

enum PropertyDefinitionOutcome {
    Complete,
    Failed(PropertyFailure),
}

enum RealmGlobalReadOutcome {
    Value(StoredValue),
    Missing,
}

enum RealmGlobalWriteOutcome {
    Complete,
    Missing,
    Property(PropertyFailure),
}

#[derive(Clone, Copy)]
enum PropertyFailure {
    ReadNull,
    ReadUndefined,
    WriteNull,
    WriteUndefined,
    NotObject,
    ReadOnly,
    NoSetter,
    NotConfigurable,
    NonExtensible,
}

fn static_property_operand(
    runtime: &Runtime,
    frame: &Frame,
    operands: Operands,
) -> Result<StaticPropertyOperand, EngineFault> {
    let Operands::Atom(index) = operands else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "property atom",
            index: u32::MAX,
        });
    };
    static_property_at(runtime, frame, index)
}

fn static_property_at(
    runtime: &Runtime,
    frame: &Frame,
    index: quickjs_bytecode::AtomPoolIndex,
) -> Result<StaticPropertyOperand, EngineFault> {
    let atom = installed_template(runtime, frame.code, frame.template)?
        .atoms
        .get(index.get() as usize)
        .cloned()
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "property atom",
            index: index.get(),
        })?;
    let name = atom
        .description()
        .cloned()
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "property atom description",
            index: index.get(),
        })?;
    Ok(StaticPropertyOperand {
        key: ArrayIndex::parse_property_key(&name).map_or_else(
            || PropertyKey::from_validated_atom(atom),
            PropertyKey::from_index,
        ),
        name,
    })
}

fn define_method_operand(
    runtime: &Runtime,
    frame: &Frame,
    operands: Operands,
) -> Result<DefineMethodOperand, EngineFault> {
    let Operands::AtomU8 { atom, value } = operands else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "method property atom",
            index: u32::MAX,
        });
    };
    if value & !0b111 != 0 || value & 0b11 == 0b11 {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified define_method flags are invalid",
        });
    }
    let kind = match value & 0b11 {
        0 => DefineMethodKind::Method,
        1 => DefineMethodKind::Getter,
        2 => DefineMethodKind::Setter,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified define_method kind is invalid",
            });
        }
    };
    Ok(DefineMethodOperand {
        property: static_property_at(runtime, frame, atom)?,
        kind,
        enumerable: value & 0b100 != 0,
    })
}

fn define_method_computed_operand(
    operands: Operands,
) -> Result<DefineMethodComputedOperand, EngineFault> {
    let Operands::U8(value) = operands else {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified define_method_computed operand is not u8",
        });
    };
    let kind = match value {
        4 => DefineMethodKind::Method,
        5 => DefineMethodKind::Getter,
        6 => DefineMethodKind::Setter,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified define_method_computed flags are invalid",
            });
        }
    };
    Ok(DefineMethodComputedOperand {
        kind,
        enumerable: true,
    })
}

fn global_reference_operand(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<GlobalReferenceOperand, EngineFault> {
    let binding = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "realm global environment",
            index,
        })?;
    let EnvironmentBinding::RealmGlobal(global) = binding else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    };
    let record = runtime
        .global_bindings
        .get(global)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "realm global binding",
            index: global.index(),
            generation: global.generation(),
        })?;
    let realm = code(runtime, frame.code)?.realm;
    if record.realm != realm {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    }
    let name = record
        .name
        .description()
        .cloned()
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "realm global atom description",
            index,
        })?;
    Ok(GlobalReferenceOperand {
        binding: global,
        realm,
        object: runtime.realm_global_object(realm)?,
        key: PropertyKey::from_validated_atom(record.name.clone()),
        name,
    })
}

fn read_realm_global(
    runtime: &Runtime,
    global: &GlobalReferenceOperand,
) -> Result<RealmGlobalReadOutcome, ExecutionError> {
    let binding =
        runtime
            .global_bindings
            .get(global.binding)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "realm global binding",
                index: global.binding.index(),
                generation: global.binding.generation(),
            })?;
    match binding.state {
        RealmGlobalBindingState::Unresolved | RealmGlobalBindingState::Object => {
            read_heap_property_if_present(
                runtime,
                HeapReference::Object(global.object),
                &global.key,
            )
            .map(|value| {
                value.map_or(
                    RealmGlobalReadOutcome::Missing,
                    RealmGlobalReadOutcome::Value,
                )
            })
        }
    }
}

fn write_realm_global(
    runtime: &mut Runtime,
    global: GlobalReferenceOperand,
    value: StoredValue,
    strict: bool,
) -> Result<RealmGlobalWriteOutcome, ExecutionError> {
    let state = runtime
        .global_bindings
        .get(global.binding)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "realm global binding",
            index: global.binding.index(),
            generation: global.binding.generation(),
        })?
        .state;
    match state {
        RealmGlobalBindingState::Unresolved => {
            let present = read_heap_property_if_present(
                runtime,
                HeapReference::Object(global.object),
                &global.key,
            )?
            .is_some();
            if !present && strict {
                return Ok(RealmGlobalWriteOutcome::Missing);
            }
            let base = StoredValue::Object(global.object);
            Ok(
                match write_static_property(
                    runtime,
                    global.realm,
                    &base,
                    global.key,
                    value,
                    strict,
                )? {
                    PropertyWriteOutcome::Complete => RealmGlobalWriteOutcome::Complete,
                    PropertyWriteOutcome::Setter { .. } => {
                        return Err(EngineFault::UnsupportedAccessorWrite {
                            operation: "realm-global property write",
                        }
                        .into());
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        RealmGlobalWriteOutcome::Property(failure)
                    }
                },
            )
        }
        RealmGlobalBindingState::Object => {
            let base = StoredValue::Object(global.object);
            Ok(
                match write_static_property(
                    runtime,
                    global.realm,
                    &base,
                    global.key,
                    value,
                    strict,
                )? {
                    PropertyWriteOutcome::Complete => RealmGlobalWriteOutcome::Complete,
                    PropertyWriteOutcome::Setter { .. } => {
                        return Err(EngineFault::UnsupportedAccessorWrite {
                            operation: "realm-global property write",
                        }
                        .into());
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        RealmGlobalWriteOutcome::Property(failure)
                    }
                },
            )
        }
    }
}

fn read_static_property(
    runtime: &Runtime,
    realm: RealmId,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<PropertyReadOutcome, ExecutionError> {
    Ok(match base {
        StoredValue::Undefined => PropertyReadOutcome::Failed(PropertyFailure::ReadUndefined),
        StoredValue::Null => PropertyReadOutcome::Failed(PropertyFailure::ReadNull),
        StoredValue::Boolean(_) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(runtime.realm_boolean_prototype(realm)?),
            base.duplicate(),
            key,
        )?,
        StoredValue::Number(_) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(runtime.realm_number_prototype(realm)?),
            base.duplicate(),
            key,
        )?,
        StoredValue::Symbol(atom) => {
            if property_key_has_string_name(key, "description") {
                atom.description().map_or_else(
                    || PropertyReadOutcome::Value(StoredValue::Undefined),
                    |description| {
                        PropertyReadOutcome::Value(StoredValue::String(description.clone()))
                    },
                )
            } else {
                PropertyReadOutcome::Value(StoredValue::Undefined)
            }
        }
        StoredValue::String(value) => {
            if let Some(index) = key.as_index()
                && index.get() < value.len()
            {
                PropertyReadOutcome::Value(StoredValue::String(
                    value.slice(index.get()..index.get().saturating_add(1))?,
                ))
            } else if key.as_atom().and_then(crate::Atom::predefined_atom)
                == Some(PredefinedAtom::Length)
            {
                PropertyReadOutcome::Value(StoredValue::Number(JsNumber::from_f64(f64::from(
                    value.len(),
                ))))
            } else {
                read_heap_property_for_receiver(
                    runtime,
                    HeapReference::Object(runtime.realm_string_prototype(realm)?),
                    base.duplicate(),
                    key,
                )?
            }
        }
        StoredValue::Function(function) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Function(*function),
            base.duplicate(),
            key,
        )?,
        StoredValue::Object(object) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(*object),
            base.duplicate(),
            key,
        )?,
    })
}

fn property_key_has_string_name(key: &PropertyKey, expected: &str) -> bool {
    key.as_atom().is_some_and(|atom| {
        atom.kind() == crate::AtomKind::String
            && atom
                .description()
                .is_some_and(|name| name.code_units().eq(expected.encode_utf16()))
    })
}

fn read_heap_property_for_receiver(
    runtime: &Runtime,
    reference: HeapReference,
    receiver: StoredValue,
    key: &PropertyKey,
) -> Result<PropertyReadOutcome, ExecutionError> {
    Ok(match lookup_heap_property(runtime, Some(reference), key)? {
        None => PropertyReadOutcome::Value(StoredValue::Undefined),
        Some(OwnProperty::Data { value, .. }) => PropertyReadOutcome::Value(value),
        Some(OwnProperty::Accessor {
            getter: Some(function),
            ..
        }) => PropertyReadOutcome::Getter { function, receiver },
        Some(OwnProperty::Accessor { getter: None, .. }) => {
            PropertyReadOutcome::Value(StoredValue::Undefined)
        }
    })
}

fn read_heap_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<StoredValue, ExecutionError> {
    Ok(read_heap_property_if_present(runtime, reference, key)?.unwrap_or(StoredValue::Undefined))
}

fn read_heap_property_if_present(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<StoredValue>, ExecutionError> {
    match lookup_heap_property(runtime, Some(reference), key)? {
        None => Ok(None),
        Some(OwnProperty::Data { value, .. }) => Ok(Some(value)),
        Some(OwnProperty::Accessor { .. }) => Err(EngineFault::UnsupportedAccessorRead {
            operation: "synchronous property read",
        }
        .into()),
    }
}

fn lookup_heap_property(
    runtime: &Runtime,
    mut current: Option<HeapReference>,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    while let Some(reference) = current {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "ordinary prototype chain contains a cycle",
            }
            .into());
        }
        remaining -= 1;
        if let Some(property) = string_exotic_index_property(runtime, reference, key)? {
            return Ok(Some(property));
        }
        let record = runtime.object_record(reference)?;
        if let Some(property) = record.own_property(key) {
            return Ok(Some(property));
        }
        current = record.prototype();
    }
    Ok(None)
}

fn string_exotic_index_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    let HeapReference::Object(object) = reference else {
        return Ok(None);
    };
    let Some(index) = key.as_index() else {
        return Ok(None);
    };
    let Some(unit) = runtime.boxed_string_code_unit_at(object, index.get())? else {
        return Ok(None);
    };
    Ok(Some(OwnProperty::Data {
        layout: PropertyLayout::data(false, true, false),
        value: StoredValue::String(JsString::from_code_units([unit])?),
    }))
}

fn string_exotic_index_is_present(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<bool, ExecutionError> {
    let HeapReference::Object(object) = reference else {
        return Ok(false);
    };
    let Some(index) = key.as_index() else {
        return Ok(false);
    };
    Ok(runtime
        .boxed_string_code_unit_at(object, index.get())?
        .is_some())
}

fn inherited_property(
    runtime: &Runtime,
    current: Option<HeapReference>,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    lookup_heap_property(runtime, current, key)
}

fn write_primitive_property(
    runtime: &Runtime,
    prototype: HeapReference,
    receiver: &StoredValue,
    key: &PropertyKey,
    value: StoredValue,
    strict: bool,
) -> Result<PropertyWriteOutcome, ExecutionError> {
    if let Some(inherited) = inherited_property(runtime, Some(prototype), key)? {
        match inherited {
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => PropertyWriteOutcome::Setter {
                        function,
                        receiver: receiver.duplicate(),
                        value,
                    },
                    None if strict => PropertyWriteOutcome::Failed(PropertyFailure::NoSetter),
                    None => PropertyWriteOutcome::Complete,
                });
            }
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                return Ok(if strict {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                } else {
                    PropertyWriteOutcome::Complete
                });
            }
            OwnProperty::Data { .. } => {}
        }
    }
    Ok(if strict {
        PropertyWriteOutcome::Failed(PropertyFailure::NotObject)
    } else {
        PropertyWriteOutcome::Complete
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "ordinary write semantics audit every primitive, own, inherited, accessor, and extensibility branch"
)]
fn write_static_property(
    runtime: &mut Runtime,
    realm: RealmId,
    base: &StoredValue,
    key: PropertyKey,
    value: StoredValue,
    strict: bool,
) -> Result<PropertyWriteOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Undefined => {
            return Ok(PropertyWriteOutcome::Failed(
                PropertyFailure::WriteUndefined,
            ));
        }
        StoredValue::Null => {
            return Ok(PropertyWriteOutcome::Failed(PropertyFailure::WriteNull));
        }
        StoredValue::Boolean(_) => {
            let prototype = runtime.realm_boolean_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::Number(_) => {
            let prototype = runtime.realm_number_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::String(_) => {
            let prototype = runtime.realm_string_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::Symbol(_) => {
            return Ok(if strict {
                PropertyWriteOutcome::Failed(PropertyFailure::NotObject)
            } else {
                PropertyWriteOutcome::Complete
            });
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };

    if string_exotic_index_is_present(runtime, reference, &key)? {
        return Ok(if strict {
            PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
        } else {
            PropertyWriteOutcome::Complete
        });
    }

    let (own, prototype, extensible) = {
        let record = runtime.object_record(reference)?;
        (
            record.own_property(&key),
            record.prototype(),
            record.is_extensible(),
        )
    };
    if let Some(own) = own {
        match own {
            OwnProperty::Data { layout, .. } => {
                if layout.writable() == Some(true) {
                    let replaced = runtime
                        .object_record_mut(reference)?
                        .replace_existing_data(&key, value);
                    if !replaced {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "located own data property disappeared before its update",
                        }
                        .into());
                    }
                    runtime.collection_pending = true;
                    return Ok(PropertyWriteOutcome::Complete);
                }
                return Ok(if strict {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                } else {
                    PropertyWriteOutcome::Complete
                });
            }
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => PropertyWriteOutcome::Setter {
                        function,
                        receiver: base.duplicate(),
                        value,
                    },
                    None if strict => PropertyWriteOutcome::Failed(PropertyFailure::NoSetter),
                    None => PropertyWriteOutcome::Complete,
                });
            }
        }
    }
    if let Some(inherited) = inherited_property(runtime, prototype, &key)? {
        match inherited {
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                return Ok(if strict {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                } else {
                    PropertyWriteOutcome::Complete
                });
            }
            OwnProperty::Data { .. } => {}
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => PropertyWriteOutcome::Setter {
                        function,
                        receiver: base.duplicate(),
                        value,
                    },
                    None if strict => PropertyWriteOutcome::Failed(PropertyFailure::NoSetter),
                    None => PropertyWriteOutcome::Complete,
                });
            }
        }
    }
    if !extensible {
        return Ok(if strict {
            PropertyWriteOutcome::Failed(PropertyFailure::NonExtensible)
        } else {
            PropertyWriteOutcome::Complete
        });
    }
    runtime.append_data_property(
        reference,
        key,
        PropertyLayout::data(true, true, true),
        value,
    )?;
    Ok(PropertyWriteOutcome::Complete)
}

fn define_static_property(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    value: StoredValue,
) -> Result<PropertyWriteOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(PropertyWriteOutcome::Failed(PropertyFailure::NotObject));
        }
    };
    let (exists, extensible) = {
        let record = runtime.object_record(reference)?;
        (record.own_property(&key), record.is_extensible())
    };
    if exists.is_some() {
        let replaced = runtime
            .object_record_mut(reference)?
            .replace_existing_with_data(&key, PropertyLayout::data(true, true, true), value);
        if replaced.is_none() {
            return Err(EngineFault::RuntimeInvariant {
                message: "located own property disappeared before its data definition",
            }
            .into());
        }
        runtime.collection_pending = true;
        return Ok(PropertyWriteOutcome::Complete);
    }
    if !extensible {
        return Ok(PropertyWriteOutcome::Failed(PropertyFailure::NonExtensible));
    }
    runtime.append_data_property(
        reference,
        key,
        PropertyLayout::data(true, true, true),
        value,
    )?;
    Ok(PropertyWriteOutcome::Complete)
}

#[allow(
    clippy::too_many_lines,
    reason = "method naming, descriptor merging, publication, and rollback form one failure-atomic transaction"
)]
fn define_static_method(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    name: &JsString,
    function: FunctionId,
    kind: DefineMethodKind,
    enumerable: bool,
) -> Result<PropertyDefinitionOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(PropertyDefinitionOutcome::Failed(
                PropertyFailure::NotObject,
            ));
        }
    };
    if reference == HeapReference::Function(function) {
        return Err(EngineFault::RuntimeInvariant {
            message: "define_method cannot publish a function onto itself",
        }
        .into());
    }
    if bytecode_function_is_constructor(runtime, function)? {
        return Err(EngineFault::RuntimeInvariant {
            message: "define_method received a constructable function",
        }
        .into());
    }

    let function_name = method_function_name(name, kind)?;
    let previous_name = preflight_method_function_name(runtime, function)?;
    let (existing, extensible) = {
        let record = runtime.object_record(reference)?;
        (record.own_property(&key), record.is_extensible())
    };
    if existing
        .as_ref()
        .is_some_and(|property| !property.layout().is_configurable())
    {
        return Ok(PropertyDefinitionOutcome::Failed(
            PropertyFailure::NotConfigurable,
        ));
    }
    if existing.is_none() && !extensible {
        return Ok(PropertyDefinitionOutcome::Failed(
            PropertyFailure::NonExtensible,
        ));
    }
    if existing.is_none() {
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            runtime.limits.max_object_properties,
            runtime.object_properties.saturating_add(1),
        )?;
        runtime
            .object_record_mut(reference)?
            .try_reserve_data(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
    }

    let layout = match kind {
        DefineMethodKind::Method => PropertyLayout::data(true, enumerable, true),
        DefineMethodKind::Getter | DefineMethodKind::Setter => {
            PropertyLayout::accessor(enumerable, true)
        }
    };
    set_preflighted_method_function_name(runtime, function, function_name)?;
    let definition = (|| -> Result<(), ExecutionError> {
        if let Some(existing) = existing {
            let replacement = match kind {
                DefineMethodKind::Method => runtime
                    .object_record_mut(reference)?
                    .replace_existing_with_data(&key, layout, StoredValue::Function(function)),
                DefineMethodKind::Getter => {
                    let setter = match existing {
                        OwnProperty::Accessor { setter, .. } => setter,
                        OwnProperty::Data { .. } => None,
                    };
                    runtime
                        .object_record_mut(reference)?
                        .replace_existing_with_accessor(&key, layout, Some(function), setter)
                }
                DefineMethodKind::Setter => {
                    let getter = match existing {
                        OwnProperty::Accessor { getter, .. } => getter,
                        OwnProperty::Data { .. } => None,
                    };
                    runtime
                        .object_record_mut(reference)?
                        .replace_existing_with_accessor(&key, layout, getter, Some(function))
                }
            };
            if replacement.is_none() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "located own property disappeared during define_method",
                }
                .into());
            }
        } else {
            match kind {
                DefineMethodKind::Method => runtime.append_data_property(
                    reference,
                    key,
                    layout,
                    StoredValue::Function(function),
                )?,
                DefineMethodKind::Getter => runtime.append_accessor_property(
                    reference,
                    key,
                    layout,
                    Some(function),
                    None,
                )?,
                DefineMethodKind::Setter => runtime.append_accessor_property(
                    reference,
                    key,
                    layout,
                    None,
                    Some(function),
                )?,
            }
        }
        Ok(())
    })();
    if let Err(error) = definition {
        restore_preflighted_method_function_name(runtime, function, previous_name)?;
        return Err(error);
    }
    // The function name is initialized before the target slot becomes
    // observable. Every fallible target-append resource is preflighted above;
    // the rollback remains as a defensive transaction boundary.
    if runtime
        .object_record(HeapReference::Function(function))?
        .own_property(&runtime.predefined_property_key(PredefinedAtom::Name))
        .is_none()
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "defined method lost its initialized name property",
        }
        .into());
    }
    runtime.collection_pending = true;
    Ok(PropertyDefinitionOutcome::Complete)
}

fn method_function_name(
    name: &JsString,
    kind: DefineMethodKind,
) -> Result<JsString, JsStringError> {
    match kind {
        DefineMethodKind::Method => Ok(name.clone()),
        DefineMethodKind::Getter => JsString::from_utf8("get ")?.concat(name),
        DefineMethodKind::Setter => JsString::from_utf8("set ")?.concat(name),
    }
}

fn preflight_method_function_name(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<OwnProperty, ExecutionError> {
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    let property = runtime
        .object_record(HeapReference::Function(function))?
        .own_property(&key)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "define_method function has no own name property",
        })?;
    match property {
        OwnProperty::Data { layout, .. } if layout == PropertyLayout::data(false, false, true) => {
            Ok(property)
        }
        OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => {
            Err(EngineFault::RuntimeInvariant {
                message: "define_method function has an invalid name descriptor",
            }
            .into())
        }
    }
}

fn set_preflighted_method_function_name(
    runtime: &mut Runtime,
    function: FunctionId,
    name: JsString,
) -> Result<(), ExecutionError> {
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    let replaced = runtime
        .object_record_mut(HeapReference::Function(function))?
        .replace_existing_with_data(
            &key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(name),
        );
    if replaced.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "preflighted define_method name property disappeared",
        }
        .into());
    }
    Ok(())
}

fn restore_preflighted_method_function_name(
    runtime: &mut Runtime,
    function: FunctionId,
    previous: OwnProperty,
) -> Result<(), ExecutionError> {
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    let restored = runtime
        .object_record_mut(HeapReference::Function(function))?
        .restore_existing_property(&key, previous);
    if restored.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "preflighted define_method name property disappeared during rollback",
        }
        .into());
    }
    Ok(())
}

fn code(runtime: &Runtime, id: InstalledCodeId) -> Result<&InstalledCode, EngineFault> {
    runtime.code.get(id).ok_or(EngineFault::StaleHeapEdge {
        edge: "installed code",
        index: id.index(),
        generation: id.generation(),
    })
}

fn installed_template(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
) -> Result<&InstalledTemplate, EngineFault> {
    let code = code(runtime, code_id)?;
    let index = usize::try_from(template.get())
        .map_err(|_| EngineFault::InvalidClosureEnvironment { function: template })?;
    code.templates
        .get(index)
        .ok_or(EngineFault::InvalidClosureEnvironment { function: template })
}

fn materialize_constant(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
    index: u32,
) -> Result<StoredValue, ExecutionError> {
    match installed_template(runtime, code_id, template)?
        .constants
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "constant",
            index,
        })? {
        InstalledConstant::Number(value) => Ok(StoredValue::Number(*value)),
        InstalledConstant::String(value) => Ok(StoredValue::String(value.clone())),
        InstalledConstant::Function(_) => Err(EngineFault::MissingPoolEntry {
            pool: "ordinary value constant",
            index,
        }
        .into()),
    }
}

fn function_constant(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
    index: u32,
) -> Result<FunctionTemplateId, ExecutionError> {
    match installed_template(runtime, code_id, template)?
        .constants
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "function constant",
            index,
        })? {
        InstalledConstant::Function(function) => Ok(*function),
        InstalledConstant::Number(_) | InstalledConstant::String(_) => {
            Err(EngineFault::MissingPoolEntry {
                pool: "function constant",
                index,
            }
            .into())
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "closure validation, capture materialization, and publication are one transaction"
)]
fn create_closure(
    runtime: &mut Runtime,
    frame: &mut Frame,
    child: FunctionTemplateId,
) -> Result<FunctionId, ExecutionError> {
    let parent = runtime
        .functions
        .get(frame.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "active function",
            index: frame.function.index(),
            generation: frame.function.generation(),
        })?;
    let parent = parent.bytecode()?;
    if parent.code != frame.code
        || parent.template != frame.template
        || parent.environment.as_slice() != frame.environment.as_slice()
    {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
        .into());
    }
    let (sources, expected, realm, function_name, defined_argument_count, has_prototype) = {
        let code = code(runtime, frame.code)?;
        let function = code
            .authority
            .function(child)
            .ok_or(EngineFault::InvalidClosureEnvironment { function: child })?;
        let source = function.function().closure_sources();
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(source.len())
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: source.len(),
            })?;
        copied.extend_from_slice(source);
        let installed_index = usize::try_from(child.get())
            .map_err(|_| EngineFault::InvalidClosureEnvironment { function: child })?;
        let installed = code
            .templates
            .get(installed_index)
            .ok_or(EngineFault::InvalidClosureEnvironment { function: child })?;
        let function_name = function.metadata().function_name().map_or_else(
            || Ok(JsString::empty()),
            |index| {
                installed
                    .atoms
                    .get(index.get() as usize)
                    .and_then(AtomDescription::description)
                    .cloned()
                    .ok_or(EngineFault::MissingPoolEntry {
                        pool: "function name atom",
                        index: index.get(),
                    })
            },
        )?;
        let header = function.function().control_flow().function_header();
        (
            copied,
            function.metadata().closures().len(),
            code.realm,
            function_name,
            header.defined_argument_count(),
            header.flags().has_prototype(),
        )
    };
    let function_prototype = runtime.realm_function_prototype(realm)?;
    let object_prototype = has_prototype
        .then(|| runtime.realm_object_prototype(realm))
        .transpose()?;
    if sources.len() != expected {
        return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
    }

    let mut capture_plans = Vec::new();
    capture_plans
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: sources.len(),
        })?;
    let mut pending_by_own = Vec::new();
    pending_by_own
        .try_reserve_exact(frame.own_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: frame.own_cells.len(),
        })?;
    pending_by_own.resize(frame.own_cells.len(), None);
    let mut pending_cells = Vec::new();
    pending_cells
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: sources.len(),
        })?;

    for source in sources {
        match source {
            CompilerClosureSource::ParentVariableReference(index) => {
                let own_index = index as usize;
                let own_cell = frame.own_cells.get(own_index).copied().ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "own variable-reference",
                        index,
                    },
                )?;
                let address = *frame.own_cell_bindings.get(own_index).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "own captured binding",
                        index,
                    },
                )?;
                if let Some(cell) = own_cell {
                    if !runtime.cells.contains(cell) {
                        return Err(EngineFault::StaleHeapEdge {
                            edge: "binding cell",
                            index: cell.index(),
                            generation: cell.generation(),
                        }
                        .into());
                    }
                    capture_plans.push(ClosureCapturePlan::Existing(EnvironmentBinding::Captured(
                        cell,
                    )));
                    continue;
                }

                if let Some(pending) = pending_by_own[own_index] {
                    capture_plans.push(ClosureCapturePlan::New(pending));
                    continue;
                }

                let binding = match address {
                    FrameBindingAddress::Argument(binding) => frame_argument(frame, binding)?,
                    FrameBindingAddress::Local(binding) => frame_local(frame, binding)?,
                };
                let FrameBinding::Direct(value) = binding else {
                    return Err(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    }
                    .into());
                };
                let pending = pending_cells.len();
                pending_cells.push(PendingOwnCell {
                    own_index,
                    address,
                    value: value.duplicate(),
                });
                pending_by_own[own_index] = Some(pending);
                capture_plans.push(ClosureCapturePlan::New(pending));
            }
            CompilerClosureSource::ParentClosure(index) => {
                let binding = *frame.environment.get(index as usize).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "parent closure",
                        index,
                    },
                )?;
                match binding {
                    EnvironmentBinding::Captured(cell) => {
                        if !runtime.cells.contains(cell) {
                            return Err(EngineFault::StaleHeapEdge {
                                edge: "closure cell",
                                index: cell.index(),
                                generation: cell.generation(),
                            }
                            .into());
                        }
                    }
                    EnvironmentBinding::RealmGlobal(global) => {
                        let valid = runtime
                            .global_bindings
                            .get(global)
                            .is_some_and(|binding| binding.realm == realm);
                        if !valid {
                            return Err(EngineFault::StaleHeapEdge {
                                edge: "realm global binding",
                                index: global.index(),
                                generation: global.generation(),
                            }
                            .into());
                        }
                    }
                }
                capture_plans.push(ClosureCapturePlan::Existing(binding));
            }
            CompilerClosureSource::ConstructorRealmGlobal(_) => {
                return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
            }
        }
    }

    check_execution_limit(
        RuntimeResource::HeapFunctions,
        runtime.limits.max_heap_functions,
        usize_to_u64(runtime.functions.len()).saturating_add(1),
    )?;
    let function_property_count = 2_usize + usize::from(has_prototype);
    let prototype_property_count = usize::from(has_prototype);
    let new_property_count = function_property_count.saturating_add(prototype_property_count);
    check_execution_limit(
        RuntimeResource::HeapObjects,
        runtime.limits.max_heap_objects,
        usize_to_u64(runtime.objects.len()).saturating_add(usize::from(has_prototype) as u64),
    )?;
    check_execution_limit(
        RuntimeResource::ObjectProperties,
        runtime.limits.max_object_properties,
        runtime
            .object_properties
            .saturating_add(usize_to_u64(new_property_count)),
    )?;
    check_execution_limit(
        RuntimeResource::BindingCells,
        runtime.limits.max_binding_cells,
        usize_to_u64(runtime.cells.len()).saturating_add(usize_to_u64(pending_cells.len())),
    )?;
    runtime
        .functions
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        })?;
    runtime
        .objects
        .try_reserve(usize::from(has_prototype))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: usize::from(has_prototype),
        })?;
    runtime
        .cells
        .try_reserve(pending_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_cells.len(),
        })?;
    let mut environment = Vec::new();
    environment
        .try_reserve_exact(capture_plans.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: capture_plans.len(),
        })?;

    let mut new_cells = Vec::new();
    new_cells
        .try_reserve_exact(pending_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_cells.len(),
        })?;

    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    let mut function_record =
        crate::object::ObjectRecord::empty(Some(HeapReference::Function(function_prototype)));
    function_record
        .try_reserve_data(function_property_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: function_property_count,
        })?;
    function_record
        .append_data(
            length_key,
            PropertyLayout::data(false, false, true),
            StoredValue::Number(JsNumber::from_f64(f64::from(defined_argument_count))),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    function_record
        .append_data(
            name_key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(function_name),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    let mut prototype_record = object_prototype.map(|object_prototype| {
        crate::object::ObjectRecord::empty(Some(HeapReference::Object(object_prototype)))
    });
    if let Some(record) = prototype_record.as_mut() {
        record
            .try_reserve_data(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                constructor_key.clone(),
                PropertyLayout::data(true, false, true),
                StoredValue::Undefined,
            )
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
    }

    for pending in &pending_cells {
        if let Ok(cell) = runtime.cells.try_insert(BindingCell {
            value: pending.value.duplicate(),
        }) {
            new_cells.push(cell);
        } else {
            for cell in new_cells {
                let removed = runtime.cells.remove(cell);
                debug_assert!(removed.is_some());
            }
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: 1,
            });
        }
    }

    for capture in capture_plans {
        let binding = match capture {
            ClosureCapturePlan::Existing(binding) => binding,
            ClosureCapturePlan::New(index) => {
                let Some(cell) = new_cells.get(index).copied() else {
                    rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
                    return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
                };
                EnvironmentBinding::Captured(cell)
            }
        };
        environment.push(binding);
    }

    for (pending, cell) in pending_cells.iter().zip(new_cells.iter().copied()) {
        let binding = match pending.address {
            FrameBindingAddress::Argument(index) => frame_argument_mut(frame, index),
            FrameBindingAddress::Local(index) => frame_local_mut(frame, index),
        };
        let binding = match binding {
            Ok(binding) => binding,
            Err(fault) => {
                rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
                return Err(fault.into());
            }
        };
        *binding = FrameBinding::Captured(cell);
        frame.own_cells[pending.own_index] = Some(cell);
    }

    let prototype_object = if let Some(record) = prototype_record {
        let Ok(object) = runtime
            .objects
            .try_insert(crate::object::HeapObject::ordinary(record))
        else {
            rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        if function_record
            .append_data(
                prototype_key,
                PropertyLayout::data(true, false, false),
                StoredValue::Object(object),
            )
            .is_err()
        {
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
            rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            });
        }
        Some(object)
    } else {
        None
    };

    let Ok(function) = runtime.functions.try_insert(HeapFunction {
        implementation: FunctionImplementation::Bytecode(BytecodeFunction {
            code: frame.code,
            template: child,
            environment,
        }),
        object: function_record,
        public_roots: 0,
    }) else {
        if let Some(object) = prototype_object {
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
        }
        rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
        return Err(ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        });
    };
    if let Some(object) = prototype_object {
        let updated = runtime.objects.get_mut(object).is_some_and(|prototype| {
            prototype
                .record
                .replace_existing_data(&constructor_key, StoredValue::Function(function))
        });
        if !updated {
            let removed = runtime.functions.remove(function);
            debug_assert!(removed.is_some());
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
            rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
            return Err(EngineFault::RuntimeInvariant {
                message: "new ordinary function prototype lost its constructor property",
            }
            .into());
        }
    }
    let Some(code) = runtime.code.get_mut(frame.code) else {
        let removed = runtime.functions.remove(function);
        debug_assert!(removed.is_some());
        if let Some(object) = prototype_object {
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
        }
        rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
        return Err(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: frame.code.index(),
            generation: frame.code.generation(),
        }
        .into());
    };
    code.live_functions = code.live_functions.saturating_add(1);
    runtime.object_properties = runtime
        .object_properties
        .saturating_add(usize_to_u64(new_property_count));
    runtime.collection_pending = true;
    Ok(function)
}

fn rollback_new_cells(
    runtime: &mut Runtime,
    frame: &mut Frame,
    pending_cells: &[PendingOwnCell],
    new_cells: &[BindingCellId],
) {
    for (pending, cell) in pending_cells.iter().zip(new_cells.iter().copied()) {
        let binding = match pending.address {
            FrameBindingAddress::Argument(index) => frame.arguments.get_mut(index as usize),
            FrameBindingAddress::Local(index) => frame.locals.get_mut(index as usize),
        };
        if let Some(binding) = binding {
            *binding = FrameBinding::Direct(pending.value.duplicate());
        }
        if let Some(own_cell) = frame.own_cells.get_mut(pending.own_index) {
            *own_cell = None;
        }
        let removed = runtime.cells.remove(cell);
        debug_assert!(removed.is_some());
    }
}

fn close_local(runtime: &Runtime, frame: &mut Frame, local: u32) -> Result<(), ExecutionError> {
    let Some(index) = frame.own_cell_bindings.iter().position(
        |address| matches!(address, FrameBindingAddress::Local(index) if *index == local),
    ) else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "captured local",
            index: local,
        }
        .into());
    };
    let Some(cell) = frame.own_cells[index] else {
        return Ok(());
    };
    let value = runtime
        .cells
        .get(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value
        .duplicate();
    *frame_local_mut(frame, local)? = FrameBinding::Direct(value);
    frame.own_cells[index] = None;
    Ok(())
}

enum BindingAccessError {
    Uninitialized,
    Fault(EngineFault),
}

impl From<BindingAccessError> for ExecutionError {
    fn from(error: BindingAccessError) -> Self {
        match error {
            BindingAccessError::Uninitialized => EngineFault::UnexpectedUninitialized {
                function: FunctionTemplateId::new(u32::MAX),
            }
            .into(),
            BindingAccessError::Fault(fault) => fault.into(),
        }
    }
}

fn duplicate_binding(
    runtime: &Runtime,
    binding: &FrameBinding,
    checked: bool,
    frame: &Frame,
) -> Result<StoredValue, BindingAccessError> {
    let value = match binding {
        FrameBinding::Direct(value) => value,
        FrameBinding::Captured(cell) => {
            &runtime
                .cells
                .get(*cell)
                .ok_or_else(|| {
                    BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                        edge: "binding cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    })
                })?
                .value
        }
    };
    match value {
        SlotValue::Uninitialized if checked => Err(BindingAccessError::Uninitialized),
        SlotValue::Uninitialized => Err(BindingAccessError::Fault(
            EngineFault::UnexpectedUninitialized {
                function: frame.template,
            },
        )),
        SlotValue::Value(value) => Ok(value.duplicate()),
    }
}

fn binding_is_uninitialized(
    runtime: &Runtime,
    binding: &FrameBinding,
) -> Result<bool, EngineFault> {
    Ok(match binding {
        FrameBinding::Direct(value) => matches!(value, SlotValue::Uninitialized),
        FrameBinding::Captured(cell) => matches!(
            runtime
                .cells
                .get(*cell)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "binding cell",
                    index: cell.index(),
                    generation: cell.generation(),
                })?
                .value,
            SlotValue::Uninitialized
        ),
    })
}

fn write_argument(
    runtime: &mut Runtime,
    frame: &mut Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    write_binding(runtime, frame_argument_mut(frame, index)?, value)
}

fn write_local(
    runtime: &mut Runtime,
    frame: &mut Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    write_binding(runtime, frame_local_mut(frame, index)?, value)
}

fn write_binding(
    runtime: &mut Runtime,
    binding: &mut FrameBinding,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    match binding {
        FrameBinding::Direct(current) => *current = value,
        FrameBinding::Captured(cell) => {
            runtime
                .cells
                .get_mut(*cell)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "binding cell",
                    index: cell.index(),
                    generation: cell.generation(),
                })?
                .value = value;
            runtime.collection_pending = true;
        }
    }
    Ok(())
}

fn duplicate_environment(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
    checked: bool,
) -> Result<StoredValue, BindingAccessError> {
    let binding = *frame.environment.get(index as usize).ok_or({
        BindingAccessError::Fault(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })
    })?;
    let EnvironmentBinding::Captured(cell) = binding else {
        return Err(BindingAccessError::Fault(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        ));
    };
    let value = &runtime
        .cells
        .get(cell)
        .ok_or_else(|| {
            BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })
        })?
        .value;
    match value {
        SlotValue::Uninitialized if checked => Err(BindingAccessError::Uninitialized),
        SlotValue::Uninitialized => Err(BindingAccessError::Fault(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        )),
        SlotValue::Value(value) => Ok(value.duplicate()),
    }
}

fn environment_is_uninitialized(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<bool, EngineFault> {
    let binding = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })?;
    let EnvironmentBinding::Captured(cell) = binding else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    };
    Ok(matches!(
        runtime
            .cells
            .get(cell)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })?
            .value,
        SlotValue::Uninitialized
    ))
}

fn write_environment(
    runtime: &mut Runtime,
    frame: &Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    let binding = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })?;
    let EnvironmentBinding::Captured(cell) = binding else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
        .into());
    };
    runtime
        .cells
        .get_mut(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value = value;
    runtime.collection_pending = true;
    Ok(())
}

fn tdz_exception(
    runtime: &Runtime,
    frame: &Frame,
    binding: BindingName,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    let code = code(runtime, frame.code)?;
    let function =
        code.authority
            .function(frame.template)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            })?;
    let installed = installed_template(runtime, frame.code, frame.template)?;
    let domains = function.function().control_flow().domains();
    let atom = match binding {
        BindingName::Local(index) => function
            .metadata()
            .variables()
            .get(domains.argument_count() as usize + index as usize)
            .and_then(quickjs_bytecode::VariableDefinition::name),
        BindingName::Closure(index) => function
            .metadata()
            .closures()
            .get(index as usize)
            .and_then(quickjs_bytecode::ClosureVariableDefinition::name),
    };
    let message = if let Some(atom) = atom
        && let Some(name) = installed
            .atoms
            .get(atom.get() as usize)
            .and_then(AtomDescription::description)
    {
        name.concat(&JsString::from_utf8(" is not initialized")?)?
    } else {
        JsString::from_utf8("lexical variable is not initialized")?
    };
    Ok(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::ReferenceError,
            message,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

fn global_not_defined_exception(
    runtime: &Runtime,
    frame: &Frame,
    name: &JsString,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    Ok(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::ReferenceError,
            message: named_property_message("'", name, "' is not defined")?,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

fn not_callable_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    Ok(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a function")?,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

fn not_constructor_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    Ok(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a constructor")?,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

fn function_not_constructor_message(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<JsString, ExecutionError> {
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let name = match runtime
        .object_record(HeapReference::Function(function))?
        .own_property(&name_key)
    {
        Some(OwnProperty::Data {
            value: StoredValue::String(name),
            ..
        }) => name,
        Some(OwnProperty::Data { .. } | OwnProperty::Accessor { .. }) | None => JsString::empty(),
    };
    if name.is_empty() {
        return Ok(JsString::from_utf8("not a constructor")?);
    }
    Ok(name.concat(&JsString::from_utf8(" is not a constructor")?)?)
}

fn property_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
    name: &JsString,
    failure: PropertyFailure,
) -> Result<PendingException, ExecutionError> {
    property_exception_at(
        instruction_location(runtime, frame, pc)?,
        Some(name),
        failure,
    )
}

fn property_exception_at(
    origin: JsStackFrame,
    name: Option<&JsString>,
    failure: PropertyFailure,
) -> Result<PendingException, ExecutionError> {
    let message = match failure {
        PropertyFailure::ReadNull => match name {
            Some(name) => named_property_message("cannot read property '", name, "' of null")?,
            None => JsString::from_utf8("cannot read property of null")?,
        },
        PropertyFailure::ReadUndefined => match name {
            Some(name) => named_property_message("cannot read property '", name, "' of undefined")?,
            None => JsString::from_utf8("cannot read property of undefined")?,
        },
        PropertyFailure::WriteNull => named_property_message(
            "cannot set property '",
            required_property_name(name)?,
            "' of null",
        )?,
        PropertyFailure::WriteUndefined => named_property_message(
            "cannot set property '",
            required_property_name(name)?,
            "' of undefined",
        )?,
        PropertyFailure::NotObject => JsString::from_utf8("not an object")?,
        PropertyFailure::ReadOnly => {
            named_property_message("'", required_property_name(name)?, "' is read-only")?
        }
        PropertyFailure::NoSetter => JsString::from_utf8("no setter for property")?,
        PropertyFailure::NotConfigurable => JsString::from_utf8("property is not configurable")?,
        PropertyFailure::NonExtensible => JsString::from_utf8("object is not extensible")?,
    };
    Ok(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message,
        },
        origin,
    })
}

fn required_property_name(name: Option<&JsString>) -> Result<&JsString, EngineFault> {
    name.ok_or(EngineFault::RuntimeInvariant {
        message: "property failure requiring a name did not retain one",
    })
}

fn named_property_message(
    prefix: &str,
    name: &JsString,
    suffix: &str,
) -> Result<JsString, JsStringError> {
    JsString::from_utf8(prefix)?
        .concat(name)?
        .concat(&JsString::from_utf8(suffix)?)
}

fn instruction_location(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<JsStackFrame, EngineFault> {
    let function = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
    let mapping = function
        .metadata()
        .source()
        .mappings()
        .get(frame.instruction.get() as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "source mapping",
            index: frame.instruction.get(),
        })?;
    Ok(JsStackFrame::new(
        frame.template,
        pc,
        function.metadata().source().display_name_arc(),
        function.metadata().source().text_arc(),
        mapping.span(),
    ))
}

fn exception_caller_frames(
    runtime: &Runtime,
    frames: &[Frame],
) -> Result<Vec<JsStackFrame>, ExecutionError> {
    let caller_count = frames.len().saturating_sub(1);
    let mut caller_frames = Vec::new();
    caller_frames.try_reserve_exact(caller_count).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::ExceptionFrames,
            additional: caller_count,
        }
    })?;
    for caller in frames[..caller_count].iter().rev() {
        let instruction = code(runtime, caller.code)?
            .authority
            .function(caller.template)
            .and_then(|function| {
                function
                    .function()
                    .control_flow()
                    .instruction(caller.instruction)
            })
            .ok_or(EngineFault::MissingInstruction {
                function: caller.template,
                instruction: caller.instruction.get(),
            })?;
        if !matches!(
            instruction.decoded().instruction().opcode(),
            FinalOpcode::Call
                | FinalOpcode::Call0
                | FinalOpcode::Call1
                | FinalOpcode::Call2
                | FinalOpcode::Call3
                | FinalOpcode::CallMethod
                | FinalOpcode::CallConstructor
                | FinalOpcode::GetField
                | FinalOpcode::GetField2
                | FinalOpcode::PutField
                | FinalOpcode::GetArrayEl
                | FinalOpcode::GetArrayEl2
                | FinalOpcode::PutArrayEl
                | FinalOpcode::ToPropKey
                | FinalOpcode::DefineMethodComputed
                | FinalOpcode::Neg
                | FinalOpcode::Plus
                | FinalOpcode::Dec
                | FinalOpcode::Inc
                | FinalOpcode::PostDec
                | FinalOpcode::PostInc
                | FinalOpcode::Not
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
                | FinalOpcode::And
                | FinalOpcode::Xor
                | FinalOpcode::Or
        ) {
            return Err(EngineFault::RuntimeInvariant {
                message: "exception caller is not parked at a call or abstract operation",
            }
            .into());
        }
        caller_frames.push(instruction_location(
            runtime,
            caller,
            instruction.decoded().pc(),
        )?);
    }
    Ok(caller_frames)
}

fn finish_exception(
    runtime: &mut Runtime,
    pending: PendingException,
    caller_frames: Vec<JsStackFrame>,
) -> Result<JsException, ExecutionError> {
    let PendingException { payload, origin } = pending;
    Ok(match payload {
        PendingExceptionPayload::EngineError { kind, message } => {
            JsException::engine_error(kind, message, origin, caller_frames)
        }
        PendingExceptionPayload::ThrownValue(value) => {
            JsException::explicit_throw(runtime.public_value(value)?, origin, caller_frames)
        }
    })
}

fn frame_argument(frame: &Frame, index: u32) -> Result<&FrameBinding, EngineFault> {
    frame
        .arguments
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "argument",
            index,
        })
}

fn frame_argument_mut(frame: &mut Frame, index: u32) -> Result<&mut FrameBinding, EngineFault> {
    frame
        .arguments
        .get_mut(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "argument",
            index,
        })
}

fn frame_local(frame: &Frame, index: u32) -> Result<&FrameBinding, EngineFault> {
    frame
        .locals
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local",
            index,
        })
}

fn frame_local_mut(frame: &mut Frame, index: u32) -> Result<&mut FrameBinding, EngineFault> {
    frame
        .locals
        .get_mut(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local",
            index,
        })
}

struct CallInputs {
    receiver: StoredValue,
    arguments: CallArguments,
    new_target: Option<FunctionId>,
}

fn take_call_inputs(
    frame: &mut Frame,
    expected_function: FunctionId,
    source: CallInputSource,
) -> Result<CallInputs, ExecutionError> {
    let (argument_count, kind) = match source {
        CallInputSource::Frame {
            argument_count,
            kind,
        } => (argument_count, kind),
        CallInputSource::Prepared(inputs) => return Ok(inputs),
    };
    let required = argument_count.saturating_add(match kind {
        CallKind::Direct => 1,
        CallKind::Method | CallKind::Constructor => 2,
    });
    if frame.stack.len() < required {
        return Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: u32::try_from(required).unwrap_or(u32::MAX),
            actual: frame.stack.len(),
        }
        .into());
    }
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(argument_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: argument_count,
        })?;
    for _ in 0..argument_count {
        arguments.push(pop(frame)?);
    }
    arguments.reverse();
    let new_target = if matches!(kind, CallKind::Constructor) {
        match pop(frame)? {
            StoredValue::Function(function) => Some(function),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "validated constructor new target changed value kind",
                }
                .into());
            }
        }
    } else {
        None
    };
    match pop(frame)? {
        StoredValue::Function(actual) if actual == expected_function => {}
        StoredValue::Function(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "parked ordinary-call callee changed before frame creation",
            }
            .into());
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "validated ordinary-call callee changed value kind",
            }
            .into());
        }
    }
    let receiver = match kind {
        CallKind::Method => pop(frame)?,
        CallKind::Direct | CallKind::Constructor => StoredValue::Undefined,
    };
    Ok(CallInputs {
        receiver,
        arguments: CallArguments::from_values(arguments),
        new_target,
    })
}

fn pop(frame: &mut Frame) -> Result<StoredValue, EngineFault> {
    frame.stack.pop().ok_or(EngineFault::StackDepthMismatch {
        function: frame.template,
        pc: BytecodePc::ZERO,
        expected: 1,
        actual: 0,
    })
}

fn peek(frame: &Frame) -> Result<&StoredValue, EngineFault> {
    frame.stack.last().ok_or(EngineFault::StackDepthMismatch {
        function: frame.template,
        pc: BytecodePc::ZERO,
        expected: 1,
        actual: 0,
    })
}

fn branch_successor(
    instruction: quickjs_bytecode::VerifiedInstruction,
    taken: bool,
    frame: &Frame,
) -> Result<InstructionIndex, EngineFault> {
    let successors = instruction.successors();
    if successors.kind() != VerifiedSuccessorKind::Branch {
        return Err(EngineFault::InvalidSuccessor {
            function: frame.template,
            pc: instruction.decoded().pc(),
        });
    }
    if taken {
        successors.branch_target()
    } else {
        successors.fallthrough()
    }
    .ok_or(EngineFault::InvalidSuccessor {
        function: frame.template,
        pc: instruction.decoded().pc(),
    })
}

fn constant_index(operands: Operands) -> Option<u32> {
    match operands {
        Operands::Const(index) => Some(index),
        Operands::Const8(index) => Some(u32::from(index)),
        _ => None,
    }
}

fn direct_call_argument_count(
    opcode: FinalOpcode,
    operands: Operands,
) -> Result<usize, EngineFault> {
    match (opcode, operands) {
        (FinalOpcode::Call, Operands::NPop { argument_count }) => Ok(usize::from(argument_count)),
        (FinalOpcode::Call0, Operands::NPopX) => Ok(0),
        (FinalOpcode::Call1, Operands::NPopX) => Ok(1),
        (FinalOpcode::Call2, Operands::NPopX) => Ok(2),
        (FinalOpcode::Call3, Operands::NPopX) => Ok(3),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

fn argument_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::Arg(index) => Ok(u32::from(index)),
        Operands::NoneArg => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied argument",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

fn local_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::Loc(index) => Ok(u32::from(index)),
        Operands::Loc8(index) => Ok(u32::from(index)),
        Operands::NoneLoc => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied local",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

fn closure_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::VarRef(index) => Ok(u32::from(index)),
        Operands::NoneVarRef => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied closure",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

const fn implied_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetLoc0
        | FinalOpcode::PutLoc0
        | FinalOpcode::SetLoc0
        | FinalOpcode::GetArg0
        | FinalOpcode::PutArg0
        | FinalOpcode::SetArg0
        | FinalOpcode::GetVarRef0
        | FinalOpcode::PutVarRef0
        | FinalOpcode::SetVarRef0 => Some(0),
        FinalOpcode::GetLoc1
        | FinalOpcode::PutLoc1
        | FinalOpcode::SetLoc1
        | FinalOpcode::GetArg1
        | FinalOpcode::PutArg1
        | FinalOpcode::SetArg1
        | FinalOpcode::GetVarRef1
        | FinalOpcode::PutVarRef1
        | FinalOpcode::SetVarRef1 => Some(1),
        FinalOpcode::GetLoc2
        | FinalOpcode::PutLoc2
        | FinalOpcode::SetLoc2
        | FinalOpcode::GetArg2
        | FinalOpcode::PutArg2
        | FinalOpcode::SetArg2
        | FinalOpcode::GetVarRef2
        | FinalOpcode::PutVarRef2
        | FinalOpcode::SetVarRef2 => Some(2),
        FinalOpcode::GetLoc3
        | FinalOpcode::PutLoc3
        | FinalOpcode::SetLoc3
        | FinalOpcode::GetArg3
        | FinalOpcode::PutArg3
        | FinalOpcode::SetArg3
        | FinalOpcode::GetVarRef3
        | FinalOpcode::PutVarRef3
        | FinalOpcode::SetVarRef3 => Some(3),
        _ => None,
    }
}

const fn implied_integer(opcode: FinalOpcode) -> Option<i32> {
    match opcode {
        FinalOpcode::PushMinus1 => Some(-1),
        FinalOpcode::Push0 => Some(0),
        FinalOpcode::Push1 => Some(1),
        FinalOpcode::Push2 => Some(2),
        FinalOpcode::Push3 => Some(3),
        FinalOpcode::Push4 => Some(4),
        FinalOpcode::Push5 => Some(5),
        FinalOpcode::Push6 => Some(6),
        FinalOpcode::Push7 => Some(7),
        _ => None,
    }
}

fn copy_environment(
    values: &[EnvironmentBinding],
    resource: RuntimeResource,
) -> Result<Vec<EnvironmentBinding>, ExecutionError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource,
            additional: values.len(),
        })?;
    copied.extend_from_slice(values);
    Ok(copied)
}

fn copy_addresses(
    values: &[FrameBindingAddress],
) -> Result<Vec<FrameBindingAddress>, ExecutionError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: values.len(),
        })?;
    copied.extend_from_slice(values);
    Ok(copied)
}

fn for_in_key_value(key: &PropertyKey) -> Result<StoredValue, ExecutionError> {
    if let Some(index) = key.as_index() {
        return Ok(StoredValue::String(index.to_js_string()?));
    }
    let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
        message: "for-in candidate is neither an array index nor an atom",
    })?;
    if atom.kind() != crate::AtomKind::String {
        return Err(EngineFault::RuntimeInvariant {
            message: "for-in candidate exposed a non-string atom",
        }
        .into());
    }
    let name = atom
        .description()
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "for-in string atom has no description",
        })?;
    Ok(StoredValue::String(name))
}

fn unsupported_dispatch<T>(opcode: FinalOpcode) -> Result<T, ExecutionError> {
    Err(EngineFault::UnsupportedDispatch { opcode }.into())
}

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;
