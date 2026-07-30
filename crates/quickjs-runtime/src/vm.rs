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
    Context, DynamicFunctionCompileFailure, EngineFault, ExceptionKind, ExecutionError, Function,
    HandleError, HandleKind, JsException, JsNumber, JsStackFrame, JsString, JsValue,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, PredefinedAtom, PropertyKey,
    PropertyLayout, Runtime, RuntimeResource,
    ids::{BindingCellId, FunctionId, InstalledCodeId, ObjectId, RealmGlobalBindingId},
    object::OwnProperty,
    runtime::{
        BindingCell, BytecodeFunction, EnvironmentBinding, FrameBindingAddress,
        FunctionImplementation, HeapFunction, InstalledCode, InstalledConstant, InstalledRoot,
        InstalledTemplate, NativeFunction, NativeFunctionKind, RealmGlobalBindingState,
        check_execution_limit, global_declaration_error, usize_to_u64,
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
    /// Replaces the maximum number of completed bytecode instructions.
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

struct DynamicCompilationBudget {
    compilation_limit: u64,
    source_code_unit_limit: u64,
    compilations: u64,
    source_code_units: u64,
}

impl DynamicCompilationBudget {
    const fn new(limits: ExecutionLimits) -> Self {
        Self {
            compilation_limit: limits.dynamic_compilations,
            source_code_unit_limit: limits.dynamic_source_code_units,
            compilations: 0,
            source_code_units: 0,
        }
    }

    fn charge(&mut self, source: &OrdinaryDynamicFunctionSource) -> Result<(), ExecutionError> {
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
    receiver_access: ReceiverAccess,
    receiver: StoredValue,
    instruction: InstructionIndex,
    return_to: Option<InstructionIndex>,
    dynamic_return: Option<DynamicFunctionReturn>,
    native_returns: Vec<NativeContinuation>,
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
}

impl NativeContinuation {
    fn retained_values(&self) -> u64 {
        match self {
            Self::FunctionSource(state) => usize_to_u64(state.arguments.len())
                .saturating_add(u64::from(state.construction.is_some())),
        }
    }
}

struct FunctionSourceContinuation {
    native: NativeFunction,
    arguments: Vec<StoredValue>,
    index: usize,
    stage: FunctionSourceStage,
    construction: Option<FunctionId>,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum FunctionSourceStage {
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
enum FunctionSourceProperty {
    Exotic,
    ToString,
    ValueOf,
}

enum FunctionSourcePropertyAction {
    Continue,
    Call {
        function: FunctionId,
        arguments: Vec<StoredValue>,
    },
}

enum FunctionSourcePropertyLookup {
    Value(StoredValue),
    Getter(FunctionId),
}

struct NativeCall {
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    return_to: Option<InstructionIndex>,
    origin: JsStackFrame,
    continuations: Vec<NativeContinuation>,
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
    DeferredSloppy,
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
            Self::DeferredSloppy
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
    runtime: &Runtime,
    realm: crate::ids::RealmId,
    access: ReceiverAccess,
    receiver: StoredValue,
) -> Result<StoredValue, EngineFault> {
    if matches!(access, ReceiverAccess::Direct) {
        return Ok(receiver);
    }
    match receiver {
        StoredValue::Undefined | StoredValue::Null => {
            runtime.realm_global_object(realm).map(StoredValue::Object)
        }
        StoredValue::Function(_) | StoredValue::Object(_) => Ok(receiver),
        StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_) => {
            Err(EngineFault::RuntimeInvariant {
                message: "primitive sloppy receiver reached the pre-wrapper object profile",
            })
        }
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

enum Step {
    Continue,
    Call {
        function: FunctionId,
        inputs: CallInputSource,
        return_to: InstructionIndex,
        source_pc: BytecodePc,
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
    Owned(Vec<StoredValue>),
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
            let value =
                execute_native_entry(self.runtime, native, owned_arguments, limits, compiler)?;
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
            FrameArguments::Owned(Vec::new()),
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
    let mut dynamic_budget = DynamicCompilationBudget::new(limits);
    execute_frames_with_dynamic_budget(
        runtime,
        initial,
        limits,
        compiler,
        unstarted_dynamic_root,
        &mut dynamic_budget,
    )
}

fn execute_frames_with_dynamic_budget(
    runtime: &mut Runtime,
    initial: Frame,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    unstarted_dynamic_root: Option<&mut InstalledRoot>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<StoredValue, ExecutionError> {
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    frames.push(initial);
    execute_prepared_frames_with_dynamic_budget(
        runtime,
        frames,
        limits,
        compiler,
        unstarted_dynamic_root,
        dynamic_budget,
    )
}

fn execute_prepared_frames_with_dynamic_budget(
    runtime: &mut Runtime,
    mut frames: Vec<Frame>,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    unstarted_dynamic_root: Option<&mut InstalledRoot>,
    dynamic_budget: &mut DynamicCompilationBudget,
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
        limits,
        compiler,
        dynamic_budget,
    );
    let cleanup = retire_active_dynamic_roots(runtime, &mut frames);
    match cleanup {
        Ok(()) => result,
        Err(fault) => Err(fault.into()),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the iterative bytecode/native transition loop remains centralized so every abrupt path shares one cleanup boundary"
)]
fn execute_frame_loop(
    runtime: &mut Runtime,
    frames: &mut Vec<Frame>,
    active_frame_values: &mut u64,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<StoredValue, ExecutionError> {
    let mut executed = 0_u64;
    loop {
        if executed == limits.instruction_fuel {
            return Err(ExecutionError::InstructionLimitExceeded {
                limit: limits.instruction_fuel,
                executed,
            });
        }
        let frame = frames.last_mut().ok_or(EngineFault::MissingInstruction {
            function: FunctionTemplateId::new(0),
            instruction: 0,
        })?;
        executed += 1;
        let step = execute_one(runtime, frame)?;
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
                        native,
                        inputs,
                        Some(return_to),
                        Some(origin),
                        active_frames,
                        *active_frame_values,
                        compiler,
                        dynamic_budget,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            dynamic_budget,
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
                                message: "native dispatch resolver returned an unresolved call",
                            }
                            .into());
                        }
                        Err(NativeFailure::Abrupt(pending)) => {
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
                            message: JsString::from_utf8("not a constructor")?,
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
                        | StoredValue::String(_) => finished.receiver,
                    }
                } else {
                    value
                };
                let native_returns = std::mem::take(&mut finished.native_returns);
                if !native_returns.is_empty() {
                    frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    let active_frames = active_execution_frames(frames);
                    let dispatch = resume_native_continuations(
                        runtime,
                        native_returns,
                        value,
                        return_to,
                        active_frames,
                        *active_frame_values,
                        compiler,
                        dynamic_budget,
                    );
                    let dispatch = match dispatch {
                        Ok(dispatch) => resolve_native_dispatch(
                            runtime,
                            dispatch,
                            active_frames,
                            *active_frame_values,
                            compiler,
                            dynamic_budget,
                        ),
                        Err(error) => Err(error),
                    };
                    match dispatch {
                        Ok(NativeDispatch::Immediate(completion)) => value = completion,
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
    Frame(Frame),
    Call(NativeCall),
}

enum NativeFailure {
    Abrupt(PendingException),
    Execution(ExecutionError),
}

impl From<ExecutionError> for NativeFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<crate::JsStringError> for NativeFailure {
    fn from(error: crate::JsStringError) -> Self {
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
    return_to: Option<InstructionIndex>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    dynamic_budget: &mut DynamicCompilationBudget,
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
                    dynamic_budget,
                )?
            }
        };
        match dispatch {
            NativeDispatch::Immediate(next) => value = next,
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
    mut dispatch: NativeDispatch,
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let NativeDispatch::Call(call) = dispatch else {
            return Ok(dispatch);
        };
        let suspended_frames = active_frames.saturating_add(call.continuations.len());
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
                dynamic_budget,
            )?;
            dispatch = match outcome {
                NativeDispatch::Immediate(value) => resume_native_continuations(
                    runtime,
                    call.continuations,
                    value,
                    call.return_to,
                    active_frames,
                    active_frame_values,
                    compiler,
                    dynamic_budget,
                )?,
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
    native: NativeFunction,
    arguments: Vec<StoredValue>,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<StoredValue, ExecutionError> {
    let mut dynamic_budget = DynamicCompilationBudget::new(limits);
    let mut prepared_frames = Vec::new();
    prepared_frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    let inputs = CallInputs {
        receiver: StoredValue::Undefined,
        arguments,
        new_target: None,
    };
    let dispatch = dispatch_native_call(
        runtime,
        native,
        inputs,
        None,
        None,
        0,
        0,
        compiler,
        &mut dynamic_budget,
    );
    let dispatch = match dispatch {
        Ok(dispatch) => {
            resolve_native_dispatch(runtime, dispatch, 0, 0, compiler, &mut dynamic_budget)
        }
        Err(error) => Err(error),
    };
    match dispatch {
        Ok(NativeDispatch::Immediate(value)) => Ok(value),
        Ok(NativeDispatch::Frame(frame)) => {
            prepared_frames.push(frame);
            execute_prepared_frames_with_dynamic_budget(
                runtime,
                prepared_frames,
                limits,
                compiler,
                None,
                &mut dynamic_budget,
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
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "native invocation, compilation, installation, and rollback remain one explicit audited boundary"
)]
fn dispatch_native_call(
    runtime: &mut Runtime,
    native: NativeFunction,
    inputs: CallInputs,
    return_to: Option<InstructionIndex>,
    origin: Option<JsStackFrame>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if inputs.new_target.is_some() && native.kind != NativeFunctionKind::OrdinaryFunctionConstructor
    {
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
                message: JsString::from_utf8("not a constructor")?,
            },
            origin,
        }));
    }
    match native.kind {
        NativeFunctionKind::FunctionPrototype => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
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
                inputs.arguments,
                inputs.new_target,
                return_to,
                origin,
                active_frames,
                active_frame_values,
                compiler,
                dynamic_budget,
            )
        }
        NativeFunctionKind::ObjectPrototypeToString => {
            let value = object_prototype_to_string(runtime, &inputs.receiver)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(value)))
        }
        NativeFunctionKind::ObjectPrototypeValueOf => match inputs.receiver {
            value @ (StoredValue::Function(_) | StoredValue::Object(_)) => {
                Ok(NativeDispatch::Immediate(value))
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
            StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_) => {
                Err(NativeFailure::Execution(
                    EngineFault::RuntimeInvariant {
                        message: "Object.prototype.valueOf primitive boxing is not implemented",
                    }
                    .into(),
                ))
            }
        },
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
                function_to_string(runtime, function)?,
            )))
        }
    }
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
    return_to: Option<InstructionIndex>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<NativeDispatch, NativeFailure> {
    advance_function_source_conversion(
        runtime,
        FunctionSourceContinuation {
            native,
            arguments,
            index: 0,
            stage: FunctionSourceStage::Start,
            construction,
            origin,
        },
        None,
        return_to,
        active_frames,
        active_frame_values,
        compiler,
        dynamic_budget,
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
    return_to: Option<InstructionIndex>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            FunctionSourceStage::AwaitExoticProperty
            | FunctionSourceStage::AwaitToStringProperty
            | FunctionSourceStage::AwaitValueOfProperty => {
                let property = match state.stage {
                    FunctionSourceStage::AwaitExoticProperty => FunctionSourceProperty::Exotic,
                    FunctionSourceStage::AwaitToStringProperty => FunctionSourceProperty::ToString,
                    FunctionSourceStage::AwaitValueOfProperty => FunctionSourceProperty::ValueOf,
                    FunctionSourceStage::Start
                    | FunctionSourceStage::ToString
                    | FunctionSourceStage::ValueOf
                    | FunctionSourceStage::AwaitExotic
                    | FunctionSourceStage::AwaitToString
                    | FunctionSourceStage::AwaitValueOf => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "dynamic Function property stage changed while resuming",
                        }
                        .into());
                    }
                };
                match use_function_source_property(&mut state, property, &value)? {
                    FunctionSourcePropertyAction::Continue => {}
                    FunctionSourcePropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return function_source_method_call(state, function, arguments, return_to);
                    }
                }
            }
            FunctionSourceStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                state.stage = FunctionSourceStage::ValueOf;
            }
            FunctionSourceStage::AwaitExotic | FunctionSourceStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(function_source_type_error(&state, "toPrimitive")?);
            }
            FunctionSourceStage::AwaitExotic
            | FunctionSourceStage::AwaitToString
            | FunctionSourceStage::AwaitValueOf => {
                let converted = dynamic_source_primitive_to_string(value)?;
                let argument =
                    state
                        .arguments
                        .get_mut(state.index)
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "dynamic Function source conversion lost its current argument",
                        })?;
                *argument = StoredValue::String(converted);
                state.index = state.index.saturating_add(1);
                state.stage = FunctionSourceStage::Start;
            }
            FunctionSourceStage::Start
            | FunctionSourceStage::ToString
            | FunctionSourceStage::ValueOf => {
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
                dynamic_budget,
            );
        }

        let current = state
            .arguments
            .get(state.index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "dynamic Function source conversion index escaped its arguments",
            })?;
        if !matches!(current, StoredValue::Function(_) | StoredValue::Object(_)) {
            let converted = dynamic_source_primitive_to_string(current.duplicate())?;
            let current =
                state
                    .arguments
                    .get_mut(state.index)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "dynamic Function primitive conversion lost its argument",
                    })?;
            *current = StoredValue::String(converted);
            state.index = state.index.saturating_add(1);
            state.stage = FunctionSourceStage::Start;
            continue;
        }

        let reference = current
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued dynamic Function source has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            FunctionSourceStage::Start => (
                FunctionSourceProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                FunctionSourceStage::AwaitExoticProperty,
            ),
            FunctionSourceStage::ToString => (
                FunctionSourceProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                FunctionSourceStage::AwaitToStringProperty,
            ),
            FunctionSourceStage::ValueOf => (
                FunctionSourceProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                FunctionSourceStage::AwaitValueOfProperty,
            ),
            FunctionSourceStage::AwaitExoticProperty
            | FunctionSourceStage::AwaitToStringProperty
            | FunctionSourceStage::AwaitValueOfProperty
            | FunctionSourceStage::AwaitExotic
            | FunctionSourceStage::AwaitToString
            | FunctionSourceStage::AwaitValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "dynamic Function source conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_function_source_property(runtime, reference, &key)? {
            FunctionSourcePropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return function_source_method_call(state, function, Vec::new(), return_to);
            }
            FunctionSourcePropertyLookup::Value(value) => {
                match use_function_source_property(&mut state, property, &value)? {
                    FunctionSourcePropertyAction::Continue => {}
                    FunctionSourcePropertyAction::Call {
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

fn lookup_function_source_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<FunctionSourcePropertyLookup, NativeFailure> {
    Ok(match lookup_heap_property(runtime, Some(reference), key)? {
        None => FunctionSourcePropertyLookup::Value(StoredValue::Undefined),
        Some(OwnProperty::Data { value, .. }) => FunctionSourcePropertyLookup::Value(value),
        Some(OwnProperty::Accessor {
            getter: Some(function),
            ..
        }) => FunctionSourcePropertyLookup::Getter(function),
        Some(OwnProperty::Accessor { getter: None, .. }) => {
            FunctionSourcePropertyLookup::Value(StoredValue::Undefined)
        }
    })
}

fn use_function_source_property(
    state: &mut FunctionSourceContinuation,
    property: FunctionSourceProperty,
    value: &StoredValue,
) -> Result<FunctionSourcePropertyAction, NativeFailure> {
    match property {
        FunctionSourceProperty::Exotic => match value {
            StoredValue::Undefined | StoredValue::Null => {
                state.stage = FunctionSourceStage::ToString;
                Ok(FunctionSourcePropertyAction::Continue)
            }
            StoredValue::Function(function) => {
                state.stage = FunctionSourceStage::AwaitExotic;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(StoredValue::String(JsString::from_utf8("string")?));
                Ok(FunctionSourcePropertyAction::Call {
                    function: *function,
                    arguments,
                })
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Object(_) => Err(function_source_type_error(state, "not a function")?),
        },
        FunctionSourceProperty::ToString => match value {
            StoredValue::Function(function) => {
                state.stage = FunctionSourceStage::AwaitToString;
                Ok(FunctionSourcePropertyAction::Call {
                    function: *function,
                    arguments: Vec::new(),
                })
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Object(_) => {
                state.stage = FunctionSourceStage::ValueOf;
                Ok(FunctionSourcePropertyAction::Continue)
            }
        },
        FunctionSourceProperty::ValueOf => match value {
            StoredValue::Function(function) => {
                state.stage = FunctionSourceStage::AwaitValueOf;
                Ok(FunctionSourcePropertyAction::Call {
                    function: *function,
                    arguments: Vec::new(),
                })
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::String(_)
            | StoredValue::Object(_) => Err(function_source_type_error(state, "toPrimitive")?),
        },
    }
}

fn function_source_method_call(
    state: FunctionSourceContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<InstructionIndex>,
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
        arguments,
        return_to,
        origin,
        continuations,
    }))
}

fn function_source_type_error(
    state: &FunctionSourceContinuation,
    message: &str,
) -> Result<NativeFailure, crate::JsStringError> {
    Ok(NativeFailure::Abrupt(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: state.origin.clone(),
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
    return_to: Option<InstructionIndex>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    dynamic_budget: &mut DynamicCompilationBudget,
) -> Result<NativeDispatch, NativeFailure> {
    dynamic_budget.charge(&source)?;
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
        FrameArguments::Owned(Vec::new()),
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

fn object_prototype_to_string(
    runtime: &Runtime,
    receiver: &StoredValue,
) -> Result<JsString, NativeFailure> {
    let (reference, default_tag) = match receiver {
        StoredValue::Undefined => return Ok(JsString::from_utf8("[object Undefined]")?),
        StoredValue::Null => return Ok(JsString::from_utf8("[object Null]")?),
        StoredValue::Function(function) => (HeapReference::Function(*function), "Function"),
        StoredValue::Object(object) => (HeapReference::Object(*object), "Object"),
        StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_) => {
            return Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "Object.prototype.toString primitive boxing is not implemented",
                }
                .into(),
            ));
        }
    };
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let tag = match read_heap_property(runtime, reference, &to_string_tag)? {
        StoredValue::String(tag) => tag,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => JsString::from_utf8(default_tag)?,
    };
    Ok(JsString::from_utf8("[object ")?
        .concat(&tag)?
        .concat(&JsString::from_utf8("]")?)?)
}

fn function_to_string(runtime: &Runtime, function: FunctionId) -> Result<JsString, NativeFailure> {
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
    let name = native_function_name_to_string(read_heap_property(
        runtime,
        HeapReference::Function(function),
        &name_key,
    )?)?;
    Ok(JsString::from_utf8("function ")?
        .concat(&name)?
        .concat(&JsString::from_utf8("() {\n    [native code]\n}")?)?)
}

fn native_function_name_to_string(value: StoredValue) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::empty()),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(JsString::from_utf8(&value.to_javascript_string())?),
        StoredValue::String(value) => Ok(value),
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

fn dynamic_source_primitive_to_string(value: StoredValue) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::from_utf8("undefined")?),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(JsString::from_utf8(&value.to_javascript_string())?),
        StoredValue::String(value) => Ok(value),
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

impl From<crate::JsStringError> for ConstructorCompletionError {
    fn from(error: crate::JsStringError) -> Self {
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
        StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_) => {
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
        | StoredValue::String(_) => {
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
        | StoredValue::String(_) => {
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
    return_to: InstructionIndex,
) -> Result<(), ExecutionError> {
    if parent.stack.len() == parent.stack.capacity() {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified call result exceeds frame stack capacity",
        }
        .into());
    }
    parent.stack.push(value);
    parent.instruction = return_to;
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
    runtime: &Runtime,
    plan: FramePlan,
    receiver: StoredValue,
    supplied: FrameArguments<'_>,
    return_to: Option<InstructionIndex>,
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
            let mut supplied = supplied.into_iter();
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

    Ok(Frame {
        function: plan.function,
        code: plan.code,
        template: plan.template,
        strict: plan.strict,
        receiver_access: plan.receiver_access,
        receiver,
        instruction: plan.instruction,
        return_to,
        dynamic_return,
        native_returns: Vec::new(),
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
fn execute_one(runtime: &mut Runtime, frame: &mut Frame) -> Result<Step, ExecutionError> {
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
            let realm = code(runtime, frame.code)?.realm;
            frame.stack.push(normalize_receiver(
                runtime,
                realm,
                frame.receiver_access,
                frame.receiver.duplicate(),
            )?);
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
            let return_to = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
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
            let return_to = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
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
            let return_to = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
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
        FinalOpcode::GetField | FinalOpcode::GetField2 => {
            let property = static_property_operand(runtime, frame, operands)?;
            let base = if opcode == FinalOpcode::GetField {
                pop(frame)?
            } else {
                peek(frame)?.duplicate()
            };
            match read_static_property(runtime, &base, &property.key)? {
                PropertyReadOutcome::Value(value) => frame.stack.push(value),
                PropertyReadOutcome::Getter { function, receiver } => {
                    let return_to = verified_instruction.successors().fallthrough().ok_or(
                        EngineFault::InvalidSuccessor {
                            function: frame.template,
                            pc: source_pc,
                        },
                    )?;
                    return Ok(Step::Call {
                        function,
                        inputs: CallInputSource::Prepared(CallInputs {
                            receiver,
                            arguments: Vec::new(),
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
            let property = static_property_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let base = pop(frame)?;
            if let PropertyWriteOutcome::Failed(failure) =
                write_static_property(runtime, &base, property.key, value, frame.strict)?
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

struct GlobalReferenceOperand {
    binding: RealmGlobalBindingId,
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
        key: PropertyKey::from_validated_atom(atom),
        name,
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
                match write_static_property(runtime, &base, global.key, value, strict)? {
                    PropertyWriteOutcome::Complete => RealmGlobalWriteOutcome::Complete,
                    PropertyWriteOutcome::Failed(failure) => {
                        RealmGlobalWriteOutcome::Property(failure)
                    }
                },
            )
        }
        RealmGlobalBindingState::Object => {
            let base = StoredValue::Object(global.object);
            Ok(
                match write_static_property(runtime, &base, global.key, value, strict)? {
                    PropertyWriteOutcome::Complete => RealmGlobalWriteOutcome::Complete,
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
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<PropertyReadOutcome, ExecutionError> {
    Ok(match base {
        StoredValue::Undefined => PropertyReadOutcome::Failed(PropertyFailure::ReadUndefined),
        StoredValue::Null => PropertyReadOutcome::Failed(PropertyFailure::ReadNull),
        StoredValue::Boolean(_) | StoredValue::Number(_) => {
            PropertyReadOutcome::Value(StoredValue::Undefined)
        }
        StoredValue::String(value) => {
            if key.as_atom().and_then(crate::Atom::predefined_atom) == Some(PredefinedAtom::Length)
            {
                PropertyReadOutcome::Value(StoredValue::Number(JsNumber::from_f64(f64::from(
                    value.len(),
                ))))
            } else {
                PropertyReadOutcome::Value(StoredValue::Undefined)
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
        let record = runtime.object_record(reference)?;
        if let Some(property) = record.own_property(key) {
            return Ok(Some(property));
        }
        current = record.prototype();
    }
    Ok(None)
}

fn inherited_property(
    runtime: &Runtime,
    current: Option<HeapReference>,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    lookup_heap_property(runtime, current, key)
}

fn write_static_property(
    runtime: &mut Runtime,
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
        StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_) => {
            return Ok(if strict {
                PropertyWriteOutcome::Failed(PropertyFailure::NotObject)
            } else {
                PropertyWriteOutcome::Complete
            });
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };

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
            OwnProperty::Accessor { .. } => {
                return Err(EngineFault::UnsupportedAccessorWrite {
                    operation: "static property write",
                }
                .into());
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
            OwnProperty::Accessor { .. } => {
                return Err(EngineFault::UnsupportedAccessorWrite {
                    operation: "inherited static property write",
                }
                .into());
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
        | StoredValue::String(_) => {
            return Ok(PropertyWriteOutcome::Failed(PropertyFailure::NotObject));
        }
    };
    let (exists, extensible) = {
        let record = runtime.object_record(reference)?;
        (record.own_property(&key), record.is_extensible())
    };
    if let Some(existing) = exists {
        match existing {
            OwnProperty::Data { .. } => {
                let replaced = runtime
                    .object_record_mut(reference)?
                    .replace_existing_data(&key, value);
                if !replaced {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "located own data property disappeared before its definition",
                    }
                    .into());
                }
                runtime.collection_pending = true;
                return Ok(PropertyWriteOutcome::Complete);
            }
            OwnProperty::Accessor { .. } => {
                return Err(EngineFault::UnsupportedAccessorWrite {
                    operation: "static property definition",
                }
                .into());
            }
        }
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
        let Ok(object) = runtime.objects.try_insert(crate::object::HeapObject {
            record,
            public_roots: 0,
        }) else {
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

fn property_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
    name: &JsString,
    failure: PropertyFailure,
) -> Result<PendingException, ExecutionError> {
    let message = match failure {
        PropertyFailure::ReadNull => {
            named_property_message("cannot read property '", name, "' of null")?
        }
        PropertyFailure::ReadUndefined => {
            named_property_message("cannot read property '", name, "' of undefined")?
        }
        PropertyFailure::WriteNull => {
            named_property_message("cannot set property '", name, "' of null")?
        }
        PropertyFailure::WriteUndefined => {
            named_property_message("cannot set property '", name, "' of undefined")?
        }
        PropertyFailure::NotObject => JsString::from_utf8("not an object")?,
        PropertyFailure::ReadOnly => named_property_message("'", name, "' is read-only")?,
        PropertyFailure::NonExtensible => JsString::from_utf8("object is not extensible")?,
    };
    Ok(PendingException {
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

fn named_property_message(
    prefix: &str,
    name: &JsString,
    suffix: &str,
) -> Result<JsString, crate::JsStringError> {
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
        ) {
            return Err(EngineFault::RuntimeInvariant {
                message: "exception caller is not parked at an ordinary call or accessor read",
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
    arguments: Vec<StoredValue>,
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
        arguments,
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

fn unsupported_dispatch<T>(opcode: FinalOpcode) -> Result<T, ExecutionError> {
    Err(EngineFault::UnsupportedDispatch { opcode }.into())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::{RuntimeLimits, value::StoredValue};
    use quickjs_compiler::CompilationContext;
    use quickjs_frontend::{
        CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits,
        FrontendOptions, GlobalScriptGoal, SourceFragment, with_dynamic_function_source,
        with_parsed_program,
    };

    struct NeverCompiler;

    impl OrdinaryDynamicFunctionCompiler for NeverCompiler {
        fn compile(
            &self,
            _source: OrdinaryDynamicFunctionSource,
        ) -> Result<Arc<quickjs_bytecode::VerifiedBytecode>, DynamicFunctionCompileFailure>
        {
            panic!("the coercion regression must fail or suspend before compilation")
        }
    }

    struct OxcDynamicCompiler;

    impl OrdinaryDynamicFunctionCompiler for OxcDynamicCompiler {
        fn compile(
            &self,
            source: OrdinaryDynamicFunctionSource,
        ) -> Result<Arc<quickjs_bytecode::VerifiedBytecode>, DynamicFunctionCompileFailure>
        {
            let parameter_text = source
                .parameters()
                .iter()
                .map(JsString::to_utf8_lossy)
                .collect::<Result<Vec<_>, _>>()
                .map_err(test_engine_failure)?;
            let body_text = source.body().to_utf8_lossy().map_err(test_engine_failure)?;
            let parameters = parameter_text
                .iter()
                .map(|parameter| SourceFragment::new(parameter.as_str()))
                .collect::<Vec<_>>();
            let dynamic_source = DynamicFunctionSource::new(
                DynamicFunctionKind::Function,
                &parameters,
                SourceFragment::new(&body_text),
            );
            with_dynamic_function_source(
                dynamic_source,
                FrontendLimits::default(),
                |unit, _prepared| {
                    let context = CompilationContext::new_with_source_name(
                        unit,
                        Arc::from("<vm accessor Function>"),
                    )
                    .map_err(test_engine_failure)?;
                    context
                        .compile_dynamic_function_script(
                            quickjs_bytecode::VerificationLimits::default(),
                        )
                        .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                        .map_err(test_engine_failure)
                },
            )
            .map_err(test_engine_failure)?
        }
    }

    fn test_engine_failure(
        error: impl Error + Send + Sync + 'static,
    ) -> DynamicFunctionCompileFailure {
        DynamicFunctionCompileFailure::Engine {
            source: Arc::new(error),
        }
    }

    #[test]
    fn symbol_to_primitive_precedes_ordinary_methods_and_receives_string_hint() {
        let (mut runtime, realm, constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Function(constructor),
            )
            .expect("symbol method");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());

        let Ok(dispatch) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("conversion must suspend at Symbol.toPrimitive");
        };
        let NativeDispatch::Call(call) = dispatch else {
            panic!("Symbol.toPrimitive must be called first");
        };

        assert_eq!(call.function, constructor);
        assert!(matches!(call.receiver, StoredValue::Object(id) if id == object));
        assert_eq!(call.arguments.len(), 1);
        let StoredValue::String(hint) = &call.arguments[0] else {
            panic!("hint must be a string");
        };
        assert_eq!(hint.to_utf8_lossy().expect("UTF-8"), "string");
        assert_eq!(call.continuations.len(), 1);
    }

    #[test]
    fn noncallable_symbol_to_primitive_throws_exact_type_error() {
        let (mut runtime, realm, _constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Number(JsNumber::from_i32(1)),
            )
            .expect("symbol value");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());

        let Err(error) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("noncallable exotic converter must fail");
        };

        assert_native_type_error(error, "not a function");
    }

    #[test]
    fn object_symbol_to_primitive_result_throws_before_ordinary_fallback() {
        let (mut runtime, realm, constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let result = source_object(&mut runtime, realm);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Function(constructor),
            )
            .expect("symbol method");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
        let Ok(dispatch) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("conversion must suspend at Symbol.toPrimitive");
        };
        let NativeDispatch::Call(call) = dispatch else {
            panic!("expected exotic conversion call");
        };

        let Err(error) = resume_native_continuations(
            &mut runtime,
            call.continuations,
            StoredValue::Object(result),
            call.return_to,
            0,
            0,
            Some(&compiler),
            &mut budget,
        ) else {
            panic!("object exotic result must fail");
        };

        assert_native_type_error(error, "toPrimitive");
    }

    #[test]
    fn constructor_source_continuation_charges_its_new_target_heap_edge() {
        let (_runtime, _realm, constructor, native) = runtime_with_function_constructor();
        let continuation = NativeContinuation::FunctionSource(FunctionSourceContinuation {
            native,
            arguments: vec![StoredValue::Undefined],
            index: 0,
            stage: FunctionSourceStage::Start,
            construction: Some(constructor),
            origin: native_function_host_origin(),
        });

        assert_eq!(continuation.retained_values(), 2);
    }

    #[test]
    fn synchronous_internal_read_rejects_an_accessor_instead_of_skipping_it() {
        let (mut runtime, realm, constructor, _native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key.clone(),
                PropertyLayout::accessor(false, true),
                Some(constructor),
                None,
            )
            .expect("accessor");

        let error = read_heap_property(&runtime, HeapReference::Object(object), &key)
            .expect_err("synchronous accessor read must fail closed");

        assert!(matches!(error, ExecutionError::EngineFault(_)));
    }

    #[test]
    fn get_field_executes_an_own_bytecode_getter() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let getter_authority = compile_test_function("function getter(){return 23;}", "getter");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (reader, getter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(reader_authority).expect("reader"),
                context.instantiate(getter_authority).expect("getter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[object], ExecutionLimits::default())
            .expect("getter read");
        let number = result
            .as_number()
            .expect("live result")
            .expect("number result");

        assert!(number.strict_equals(JsNumber::from_i32(23)));
    }

    #[test]
    fn inherited_getter_receives_the_original_object() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let reader = runtime
            .context(&realm)
            .expect("context")
            .instantiate(reader_authority)
            .expect("reader");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object_prototype = runtime
            .realm_object_prototype(realm_id)
            .expect("Object.prototype");
        let StoredValue::Function(getter) = read_heap_property(
            &runtime,
            HeapReference::Object(object_prototype),
            &runtime.predefined_property_key(PredefinedAtom::ValueOf),
        )
        .expect("Object.prototype.valueOf") else {
            panic!("Object.prototype.valueOf must be callable");
        };
        let prototype = source_object(&mut runtime, realm_id);
        let object = runtime
            .allocate_ordinary_object(prototype)
            .expect("child object");
        runtime
            .append_accessor_property(
                HeapReference::Object(prototype),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter),
                None,
            )
            .expect("inherited accessor");
        let public_object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[public_object], ExecutionLimits::default())
            .expect("inherited getter");

        assert_eq!(result.object_id().expect("object result"), object);
    }

    #[test]
    fn missing_getter_returns_undefined_and_shadows_the_prototype() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let reader = runtime
            .context(&realm)
            .expect("context")
            .instantiate(reader_authority)
            .expect("reader");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let prototype = source_object(&mut runtime, realm_id);
        let object = runtime
            .allocate_ordinary_object(prototype)
            .expect("child object");
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_data_property(
                HeapReference::Object(prototype),
                key.clone(),
                PropertyLayout::data(true, true, true),
                StoredValue::Number(JsNumber::from_i32(44)),
            )
            .expect("prototype value");
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::accessor(false, true),
                None,
                None,
            )
            .expect("getterless accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[object], ExecutionLimits::default())
            .expect("getterless read");

        assert_eq!(
            result.kind().expect("live result"),
            crate::ValueKind::Undefined
        );
    }

    #[test]
    fn get_field2_keeps_the_original_base_for_the_returned_method() {
        let invoke_authority = compile_test_function(
            "function invoke(object){return object.toString();}",
            "invoke",
        );
        let maker_authority = compile_test_function(
            "function make(method){\
                 return function getter(){return method;};\
             }",
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object_prototype = runtime
            .realm_object_prototype(realm_id)
            .expect("Object.prototype");
        let StoredValue::Function(value_of) = read_heap_property(
            &runtime,
            HeapReference::Object(object_prototype),
            &runtime.predefined_property_key(PredefinedAtom::ValueOf),
        )
        .expect("Object.prototype.valueOf") else {
            panic!("Object.prototype.valueOf must be callable");
        };
        let value_of = runtime
            .public_value(StoredValue::Function(value_of))
            .expect("valueOf root")
            .into_function()
            .expect("valueOf function");
        let (invoke, getter) = {
            let mut context = runtime.context(&realm).expect("context");
            let invoke = context.instantiate(invoke_authority).expect("invoke");
            let maker = context.instantiate(maker_authority).expect("maker");
            let getter = context
                .call(&maker, &[value_of.as_value()], ExecutionLimits::default())
                .expect("getter closure")
                .into_function()
                .expect("getter");
            (invoke, getter)
        };
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let public_object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&invoke, &[public_object], ExecutionLimits::default())
            .expect("accessor method call");

        assert_eq!(result.object_id().expect("object result"), object);
    }

    #[test]
    fn throwing_getter_preserves_getter_origin_and_property_caller() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let getter_authority = compile_test_function("function getter(){throw 37;}", "getter");
        let constant_authority =
            compile_test_function("function constant(){return 1;}", "constant");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (reader, getter, constant) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(reader_authority).expect("reader"),
                context.instantiate(getter_authority).expect("getter"),
                context.instantiate(constant_authority).expect("constant"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let error = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[object], ExecutionLimits::default())
            .expect_err("getter throw");
        let ExecutionError::Exception(exception) = error else {
            panic!("getter throw must remain a JavaScript exception");
        };
        let thrown = exception
            .thrown_value()
            .expect("explicit throw")
            .as_number()
            .expect("live throw")
            .expect("number throw");

        assert!(thrown.strict_equals(JsNumber::from_i32(37)));
        assert_eq!(exception.caller_frames().len(), 1);
        assert!(
            exception.caller_frames()[0]
                .source_text()
                .contains("object.toString")
        );
        runtime
            .context(&realm)
            .expect("context")
            .call(&constant, &[], ExecutionLimits::default())
            .expect("runtime remains reusable");
    }

    #[test]
    fn bytecode_getter_obeys_the_active_frame_limit() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let getter_authority = compile_test_function("function getter(){return 1;}", "getter");
        let mut runtime =
            Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (reader, getter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(reader_authority).expect("reader"),
                context.instantiate(getter_authority).expect("getter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");
        let baseline = runtime.usage();

        for _ in 0..2 {
            let error = runtime
                .context(&realm)
                .expect("context")
                .call(
                    &reader,
                    std::slice::from_ref(&object),
                    ExecutionLimits::default(),
                )
                .expect_err("getter frame exceeds limit");
            assert!(matches!(
                error,
                ExecutionError::LimitExceeded {
                    resource: RuntimeResource::Frames,
                    limit: 1,
                    observed: 2,
                }
            ));
            assert_eq!(runtime.usage(), baseline);
        }
    }

    #[test]
    fn dynamic_function_calls_an_accessor_before_using_its_to_string_value() {
        let (mut runtime, realm, constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::accessor(false, true),
                Some(constructor),
                None,
            )
            .expect("accessor");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());

        let Ok(dispatch) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("conversion must suspend at the accessor getter");
        };
        let NativeDispatch::Call(call) = dispatch else {
            panic!("accessor getter must be called");
        };

        assert_eq!(call.function, constructor);
        assert!(call.arguments.is_empty());
        assert!(matches!(call.receiver, StoredValue::Object(id) if id == object));
    }

    #[test]
    fn global_function_executes_an_accessor_getter_and_its_bytecode_conversion_method() {
        let method_authority = compile_test_function(
            "function sourceString(){return 'return 29;';}",
            "sourceString",
        );
        let maker_authority = compile_test_function(
            "function makeGetter(method){\
                 return function sourceGetter(){return method;};\
             }",
            "makeGetter",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let getter = {
            let mut context = runtime.context(&realm).expect("context");
            let method = context
                .instantiate(method_authority)
                .expect("conversion method");
            let maker = context.instantiate(maker_authority).expect("getter maker");
            context
                .call(&maker, &[method.as_value()], ExecutionLimits::default())
                .expect("getter closure")
                .into_function()
                .expect("getter function")
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let source = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(source),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("source accessor");
        let source = runtime
            .public_value(StoredValue::Object(source))
            .expect("source root");
        let global = runtime
            .realm_global_object(realm_id)
            .expect("global object");
        let StoredValue::Function(constructor) = read_heap_property(
            &runtime,
            HeapReference::Object(global),
            &runtime.predefined_property_key(PredefinedAtom::Function),
        )
        .expect("global Function") else {
            panic!("global Function must be callable");
        };
        let constructor = runtime
            .public_value(StoredValue::Function(constructor))
            .expect("Function root")
            .into_function()
            .expect("Function value");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(OxcDynamicCompiler);
        let generated = runtime
            .context(&realm)
            .expect("context")
            .call_with_dynamic_function_compiler(
                &constructor,
                &[source],
                ExecutionLimits::default(),
                &compiler,
            )
            .expect("accessor-backed Function source")
            .into_function()
            .expect("generated function");
        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&generated, &[], ExecutionLimits::default())
            .expect("generated function result");
        let number = result
            .as_number()
            .expect("live result")
            .expect("number result");

        assert!(number.strict_equals(JsNumber::from_i32(29)));
    }

    #[test]
    fn global_function_accessor_throw_prevents_dynamic_compilation() {
        let getter_authority =
            compile_test_function("function sourceGetter(){throw 53;}", "sourceGetter");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let getter = runtime
            .context(&realm)
            .expect("context")
            .instantiate(getter_authority)
            .expect("throwing getter");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let source = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(source),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("source accessor");
        let source = runtime
            .public_value(StoredValue::Object(source))
            .expect("source root");
        let global = runtime
            .realm_global_object(realm_id)
            .expect("global object");
        let StoredValue::Function(constructor) = read_heap_property(
            &runtime,
            HeapReference::Object(global),
            &runtime.predefined_property_key(PredefinedAtom::Function),
        )
        .expect("global Function") else {
            panic!("global Function must be callable");
        };
        let constructor = runtime
            .public_value(StoredValue::Function(constructor))
            .expect("Function root")
            .into_function()
            .expect("Function value");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);

        let error = runtime
            .context(&realm)
            .expect("context")
            .call_with_dynamic_function_compiler(
                &constructor,
                &[source],
                ExecutionLimits::default(),
                &compiler,
            )
            .expect_err("getter throw must escape before compilation");
        let ExecutionError::Exception(exception) = error else {
            panic!("getter throw must remain a JavaScript exception");
        };
        let thrown = exception
            .thrown_value()
            .expect("explicit throw")
            .as_number()
            .expect("live throw")
            .expect("number throw");

        assert!(thrown.strict_equals(JsNumber::from_i32(53)));
    }

    fn compile_test_function(source: &str, name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<vm accessor test>"))
                        .expect("storage plan");
                let root = context
                    .executables()
                    .find(|executable| executable.metadata().name() == Some(name))
                    .expect("named function");
                let tree = context
                    .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                    .expect("verified function tree");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    }

    fn runtime_with_function_constructor()
    -> (Runtime, crate::ids::RealmId, FunctionId, NativeFunction) {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let realm = runtime.context(&realm).expect("context").realm;
        let global = runtime.realm_global_object(realm).expect("global object");
        let key = runtime.predefined_property_key(PredefinedAtom::Function);
        let StoredValue::Function(constructor) =
            read_heap_property(&runtime, HeapReference::Object(global), &key)
                .expect("Function property")
        else {
            panic!("global Function is not callable");
        };
        let native = runtime
            .functions
            .get(constructor)
            .and_then(HeapFunction::native)
            .copied()
            .expect("native Function");
        (runtime, realm, constructor, native)
    }

    fn source_object(runtime: &mut Runtime, realm: crate::ids::RealmId) -> ObjectId {
        let prototype = runtime
            .realm_object_prototype(realm)
            .expect("Object.prototype");
        runtime
            .allocate_ordinary_object(prototype)
            .expect("source object")
    }

    fn assert_native_type_error(error: NativeFailure, expected: &str) {
        let NativeFailure::Abrupt(PendingException {
            payload: PendingExceptionPayload::EngineError { kind, message },
            ..
        }) = error
        else {
            panic!("expected native JavaScript exception");
        };
        assert_eq!(kind, ExceptionKind::TypeError);
        assert_eq!(message.to_utf8_lossy().expect("UTF-8"), expected);
    }
}
