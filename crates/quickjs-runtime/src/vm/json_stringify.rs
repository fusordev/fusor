/*
 * JavaScript JSON.stringify semantics derived from QuickJS.
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
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Resumable ECMA-262 JSON serialization.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy)]
enum JsonStringifySetupStage {
    ReplacerList,
    AwaitReplacerGet,
    Space,
    Serialize,
}

struct JsonReplacerArray {
    object: ObjectId,
    next: u32,
    length: u32,
}

#[derive(Clone, Copy)]
enum JsonPropertyStage {
    Get,
    AwaitGet,
    ToJson,
    AwaitToJsonGet,
    CallToJson(FunctionId),
    AwaitToJsonCall,
    Replacer,
    AwaitReplacerCall,
    Normalize,
    AwaitBoxedNumber,
    AwaitBoxedString,
}

struct JsonPropertyFrame {
    holder: StoredValue,
    key: PropertyKey,
    name: JsString,
    value: Option<StoredValue>,
    stage: JsonPropertyStage,
}

struct JsonArrayFrame {
    object: ObjectId,
    next: u32,
    length: u32,
    partial: Vec<JsString>,
    step_back: JsString,
    pending: bool,
}

struct JsonObjectFrame {
    object: ObjectId,
    keys: Vec<JsString>,
    next: usize,
    partial: Vec<JsString>,
    step_back: JsString,
    pending: Option<JsString>,
}

enum JsonStringifyFrame {
    Property(JsonPropertyFrame),
    Array(JsonArrayFrame),
    Object(JsonObjectFrame),
}

/// One suspended `JSON.stringify` setup and serialization worklist.
pub(super) struct JsonStringifyContinuation {
    value: Option<StoredValue>,
    replacer: Option<StoredValue>,
    space: Option<StoredValue>,
    replacer_function: Option<FunctionId>,
    replacer_array: Option<JsonReplacerArray>,
    property_list: Option<Vec<JsString>>,
    gap: JsString,
    indent: JsString,
    stack: Vec<ObjectId>,
    frames: Vec<JsonStringifyFrame>,
    setup: JsonStringifySetupStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl JsonStringifyContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        let inputs = u64::from(self.value.is_some())
            .saturating_add(u64::from(self.replacer.is_some()))
            .saturating_add(u64::from(self.space.is_some()))
            .saturating_add(u64::from(self.replacer_function.is_some()))
            .saturating_add(u64::from(self.replacer_array.is_some()))
            .saturating_add(
                self.property_list
                    .as_ref()
                    .map_or(0, |list| usize_to_u64(list.len())),
            );
        self.frames.iter().fold(
            inputs.saturating_add(usize_to_u64(self.stack.len())),
            |count, frame| {
                count.saturating_add(match frame {
                    JsonStringifyFrame::Property(frame) => {
                        1_u64.saturating_add(u64::from(frame.value.is_some()))
                    }
                    JsonStringifyFrame::Array(frame) => {
                        1_u64.saturating_add(usize_to_u64(frame.partial.len()))
                    }
                    JsonStringifyFrame::Object(frame) => 1_u64
                        .saturating_add(usize_to_u64(frame.keys.len()))
                        .saturating_add(usize_to_u64(frame.partial.len()))
                        .saturating_add(u64::from(frame.pending.is_some())),
                })
            },
        )
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for value in [&self.value, &self.replacer, &self.space]
            .into_iter()
            .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        if let Some(function) = self.replacer_function {
            mark(CollectionRoot::Heap(HeapReference::Function(function)));
        }
        if let Some(replacer) = &self.replacer_array {
            mark(CollectionRoot::Heap(HeapReference::Object(replacer.object)));
        }
        for object in &self.stack {
            mark(CollectionRoot::Heap(HeapReference::Object(*object)));
        }
        for frame in &self.frames {
            match frame {
                JsonStringifyFrame::Property(frame) => {
                    trace_stored_value_root(&frame.holder, mark);
                    if let Some(value) = &frame.value {
                        trace_stored_value_root(value, mark);
                    }
                }
                JsonStringifyFrame::Array(frame) => {
                    mark(CollectionRoot::Heap(HeapReference::Object(frame.object)));
                }
                JsonStringifyFrame::Object(frame) => {
                    mark(CollectionRoot::Heap(HeapReference::Object(frame.object)));
                }
            }
        }
    }
}

/// Starts replacer-list construction, gap conversion, and root serialization.
#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps the three arguments, realm, caller, origin, and shared budget explicit"
)]
pub(super) fn begin_json_stringify(
    runtime: &mut Runtime,
    value: StoredValue,
    replacer: StoredValue,
    space: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut state = JsonStringifyContinuation {
        value: Some(value),
        replacer: Some(replacer),
        space: Some(space),
        replacer_function: None,
        replacer_array: None,
        property_list: None,
        gap: JsString::empty(),
        indent: JsString::empty(),
        stack: Vec::new(),
        frames: Vec::new(),
        setup: JsonStringifySetupStage::ReplacerList,
        realm,
        origin,
    };

    match state.replacer.take().unwrap_or(StoredValue::Undefined) {
        StoredValue::Function(function) => state.replacer_function = Some(function),
        StoredValue::Object(object) if runtime.is_array_object(object)? => {
            state.property_list = Some(Vec::new());
            state.replacer_array = Some(JsonReplacerArray {
                object,
                next: 0,
                length: runtime
                    .array_length(object)?
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON replacer Array lost its cached length",
                    })?,
            });
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::BigInt(_)
        | StoredValue::Object(_) => {}
    }
    drive_json_stringify(runtime, state, None, return_to, execution_budget)
}

/// Resumes after a replacer-list getter, property getter, `toJSON`, or replacer call.
pub(super) fn advance_json_stringify(
    runtime: &mut Runtime,
    state: JsonStringifyContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    drive_json_stringify(
        runtime,
        state,
        Some(completion),
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_json_stringify_replacer_item(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    item: JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    append_unique_property_list_item(&mut state, item, execution_budget)?;
    state.setup = JsonStringifySetupStage::ReplacerList;
    drive_json_stringify(runtime, state, None, return_to, execution_budget)
}

pub(super) fn finish_json_stringify_space_number(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    number: JsNumber,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.gap = json_gap_from_number(number)?;
    begin_json_stringify_serialization(runtime, &mut state)?;
    drive_json_stringify(runtime, state, None, return_to, execution_budget)
}

pub(super) fn finish_json_stringify_space_string(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    string: JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.gap = json_gap_from_string(string)?;
    begin_json_stringify_serialization(runtime, &mut state)?;
    drive_json_stringify(runtime, state, None, return_to, execution_budget)
}

pub(super) fn finish_json_stringify_boxed_number(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    number: JsNumber,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let property = current_json_property_mut(&mut state)?;
    property.value = Some(StoredValue::Number(number));
    property.stage = JsonPropertyStage::Normalize;
    drive_json_stringify(runtime, state, None, return_to, execution_budget)
}

pub(super) fn finish_json_stringify_boxed_string(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    string: JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let property = current_json_property_mut(&mut state)?;
    property.value = Some(StoredValue::String(string));
    property.stage = JsonPropertyStage::Normalize;
    drive_json_stringify(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "one explicit dispatcher keeps JSON setup and every observable suspension point ordered"
)]
fn drive_json_stringify(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(completion) = completion {
        match state.setup {
            JsonStringifySetupStage::AwaitReplacerGet => {
                if let Some(dispatch) = process_json_replacer_item(
                    runtime,
                    &mut state,
                    completion,
                    return_to,
                    execution_budget,
                )? {
                    return Ok(dispatch);
                }
            }
            JsonStringifySetupStage::Serialize => {
                resume_json_property(&mut state, completion)?;
            }
            JsonStringifySetupStage::ReplacerList | JsonStringifySetupStage::Space => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "JSON stringify received a completion outside an awaiting stage",
                }
                .into());
            }
        }
    }

    loop {
        execution_budget.charge_instructions(1)?;
        match state.setup {
            JsonStringifySetupStage::ReplacerList => {
                let Some(replacer) = state.replacer_array.as_mut() else {
                    state.setup = JsonStringifySetupStage::Space;
                    continue;
                };
                if replacer.next >= replacer.length {
                    state.replacer_array = None;
                    state.setup = JsonStringifySetupStage::Space;
                    continue;
                }
                let index = replacer.next;
                replacer.next = replacer.next.saturating_add(1);
                let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "JSON replacer index reached the non-index u32 maximum",
                    },
                )?);
                let holder = StoredValue::Object(replacer.object);
                charge_heap_property_lookup(runtime, &holder, execution_budget)?;
                state.setup = JsonStringifySetupStage::AwaitReplacerGet;
                match read_static_property(runtime, state.realm, &holder, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        if let Some(dispatch) = process_json_replacer_item(
                            runtime,
                            &mut state,
                            value,
                            return_to,
                            execution_budget,
                        )? {
                            return Ok(dispatch);
                        }
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        return call_json_stringify_function(
                            function,
                            receiver,
                            Vec::new(),
                            state,
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(NativeFailure::Abrupt(property_exception_at(
                            state.realm,
                            state.origin,
                            None,
                            failure,
                        )?));
                    }
                }
            }
            JsonStringifySetupStage::AwaitReplacerGet => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "JSON replacer getter resumed without a completion",
                }
                .into());
            }
            JsonStringifySetupStage::Space => {
                return begin_json_stringify_space(runtime, state, return_to, execution_budget);
            }
            JsonStringifySetupStage::Serialize => {
                if let Some(result) =
                    drive_json_serialization(runtime, &mut state, return_to, execution_budget)?
                {
                    return Ok(result);
                }
            }
        }
    }
}

fn process_json_replacer_item(
    runtime: &mut Runtime,
    state: &mut JsonStringifyContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    match value {
        StoredValue::String(item) => {
            append_unique_property_list_item(state, item, execution_budget)?;
        }
        StoredValue::Number(number) => {
            append_unique_property_list_item(
                state,
                number.to_javascript_string()?,
                execution_budget,
            )?;
        }
        StoredValue::Object(object)
            if runtime.boxed_string(object)?.is_some()
                || runtime.boxed_number(object)?.is_some() =>
        {
            let realm = state.realm;
            let origin = state.origin.clone();
            let owned = take_json_stringify_state(state);
            return Ok(Some(begin_operator_primitive_conversion(
                runtime,
                StoredValue::Object(object),
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::JsonStringifyReplacerItem(Box::new(owned)),
                realm,
                return_to,
                origin,
                execution_budget,
            )?));
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Symbol(_)
        | StoredValue::BigInt(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => {}
    }
    state.setup = JsonStringifySetupStage::ReplacerList;
    Ok(None)
}

fn append_unique_property_list_item(
    state: &mut JsonStringifyContinuation,
    item: JsString,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let list = state
        .property_list
        .as_mut()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "JSON replacer item completed without a property list",
        })?;
    execution_budget.charge_instructions(usize_to_u64(list.len()).saturating_add(1))?;
    if list.contains(&item) {
        return Ok(());
    }
    list.try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    list.push(item);
    Ok(())
}

fn begin_json_stringify_space(
    runtime: &mut Runtime,
    mut state: JsonStringifyContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let space = state.space.take().unwrap_or(StoredValue::Undefined);
    match space {
        StoredValue::Object(object) if runtime.boxed_number(object)?.is_some() => {
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                StoredValue::Object(object),
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::JsonStringifySpaceNumber(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        StoredValue::Object(object) if runtime.boxed_string(object)?.is_some() => {
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                StoredValue::Object(object),
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::JsonStringifySpaceString(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        StoredValue::Number(number) => {
            state.gap = json_gap_from_number(number)?;
            begin_json_stringify_serialization(runtime, &mut state)?;
            drive_json_stringify(runtime, state, None, return_to, execution_budget)
        }
        StoredValue::String(string) => {
            state.gap = json_gap_from_string(string)?;
            begin_json_stringify_serialization(runtime, &mut state)?;
            drive_json_stringify(runtime, state, None, return_to, execution_budget)
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Symbol(_)
        | StoredValue::BigInt(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => {
            begin_json_stringify_serialization(runtime, &mut state)?;
            drive_json_stringify(runtime, state, None, return_to, execution_budget)
        }
    }
}

fn json_gap_from_number(number: JsNumber) -> Result<JsString, NativeFailure> {
    const SPACES: [u8; 10] = [b' '; 10];

    let integer = number_to_integer_or_infinity(number);
    let count = if integer < 1.0 {
        0
    } else if integer >= 10.0 {
        10
    } else {
        const INDENT_THRESHOLDS: [f64; 9] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
        INDENT_THRESHOLDS.partition_point(|threshold| integer >= *threshold)
    };
    Ok(JsString::from_latin1(&SPACES[..count])?)
}

fn json_gap_from_string(string: JsString) -> Result<JsString, NativeFailure> {
    if string.len() <= 10 {
        Ok(string)
    } else {
        Ok(string.slice(0..10)?)
    }
}

fn begin_json_stringify_serialization(
    runtime: &mut Runtime,
    state: &mut JsonStringifyContinuation,
) -> Result<(), NativeFailure> {
    let wrapper = runtime.allocate_ordinary_object(runtime.realm_object_prototype(state.realm)?)?;
    let key = runtime.predefined_property_key(PredefinedAtom::EmptyString);
    runtime.append_data_property(
        HeapReference::Object(wrapper),
        key.clone(),
        PropertyLayout::data(true, true, true),
        state.value.take().unwrap_or(StoredValue::Undefined),
    )?;
    state
        .frames
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state
        .frames
        .push(JsonStringifyFrame::Property(JsonPropertyFrame {
            holder: StoredValue::Object(wrapper),
            key,
            name: JsString::empty(),
            value: None,
            stage: JsonPropertyStage::Get,
        }));
    state.setup = JsonStringifySetupStage::Serialize;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one worklist loop makes getter, callback, container, and child completion order explicit"
)]
fn drive_json_serialization(
    runtime: &mut Runtime,
    state: &mut JsonStringifyContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    loop {
        execution_budget.charge_instructions(1)?;
        let Some(frame) = state.frames.last() else {
            return Err(EngineFault::RuntimeInvariant {
                message: "JSON serialization worklist became empty without a result",
            }
            .into());
        };
        match frame {
            JsonStringifyFrame::Property(property) => match property.stage {
                JsonPropertyStage::Get => {
                    let holder = property.holder.duplicate();
                    let key = property.key.clone();
                    charge_heap_property_lookup(runtime, &holder, execution_budget)?;
                    current_json_property_mut(state)?.stage = JsonPropertyStage::AwaitGet;
                    match read_static_property(runtime, state.realm, &holder, &key)? {
                        PropertyReadOutcome::Value(value) => {
                            resume_json_property(state, value)?;
                        }
                        PropertyReadOutcome::Getter { function, receiver } => {
                            return Ok(Some(call_json_stringify_function(
                                function,
                                receiver,
                                Vec::new(),
                                take_json_stringify_state(state),
                                return_to,
                            )?));
                        }
                        PropertyReadOutcome::Failed(failure) => {
                            return Err(NativeFailure::Abrupt(property_exception_at(
                                state.realm,
                                state.origin.clone(),
                                None,
                                failure,
                            )?));
                        }
                    }
                }
                JsonPropertyStage::ToJson => {
                    let value = current_json_property(state)?
                        .value
                        .as_ref()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "JSON toJSON lookup has no property value",
                        })?
                        .duplicate();
                    if !matches!(
                        value,
                        StoredValue::Object(_) | StoredValue::Function(_) | StoredValue::BigInt(_)
                    ) {
                        current_json_property_mut(state)?.stage = JsonPropertyStage::Replacer;
                        continue;
                    }
                    let key = runtime.predefined_property_key(PredefinedAtom::ToJson);
                    if matches!(value, StoredValue::BigInt(_)) {
                        charge_heap_property_lookup(
                            runtime,
                            &StoredValue::Object(runtime.realm_bigint_prototype(state.realm)?),
                            execution_budget,
                        )?;
                    } else {
                        charge_heap_property_lookup(runtime, &value, execution_budget)?;
                    }
                    current_json_property_mut(state)?.stage = JsonPropertyStage::AwaitToJsonGet;
                    match read_static_property(runtime, state.realm, &value, &key)? {
                        PropertyReadOutcome::Value(method) => resume_json_property(state, method)?,
                        PropertyReadOutcome::Getter { function, receiver } => {
                            return Ok(Some(call_json_stringify_function(
                                function,
                                receiver,
                                Vec::new(),
                                take_json_stringify_state(state),
                                return_to,
                            )?));
                        }
                        PropertyReadOutcome::Failed(failure) => {
                            return Err(NativeFailure::Abrupt(property_exception_at(
                                state.realm,
                                state.origin.clone(),
                                None,
                                failure,
                            )?));
                        }
                    }
                }
                JsonPropertyStage::CallToJson(function) => {
                    let property = current_json_property_mut(state)?;
                    let receiver = property
                        .value
                        .as_ref()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "JSON toJSON call has no receiver value",
                        })?
                        .duplicate();
                    let mut arguments = Vec::new();
                    arguments.try_reserve_exact(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: 1,
                        }
                    })?;
                    arguments.push(StoredValue::String(property.name.clone()));
                    property.stage = JsonPropertyStage::AwaitToJsonCall;
                    return Ok(Some(call_json_stringify_function(
                        function,
                        receiver,
                        arguments,
                        take_json_stringify_state(state),
                        return_to,
                    )?));
                }
                JsonPropertyStage::Replacer => {
                    let Some(function) = state.replacer_function else {
                        current_json_property_mut(state)?.stage = JsonPropertyStage::Normalize;
                        continue;
                    };
                    let property = current_json_property_mut(state)?;
                    let receiver = property.holder.duplicate();
                    let mut arguments = Vec::new();
                    arguments.try_reserve_exact(2).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: 2,
                        }
                    })?;
                    arguments.push(StoredValue::String(property.name.clone()));
                    arguments.push(
                        property
                            .value
                            .as_ref()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "JSON replacer call has no property value",
                            })?
                            .duplicate(),
                    );
                    property.stage = JsonPropertyStage::AwaitReplacerCall;
                    return Ok(Some(call_json_stringify_function(
                        function,
                        receiver,
                        arguments,
                        take_json_stringify_state(state),
                        return_to,
                    )?));
                }
                JsonPropertyStage::Normalize => {
                    if let Some(dispatch) =
                        normalize_json_property(runtime, state, return_to, execution_budget)?
                    {
                        return Ok(Some(dispatch));
                    }
                }
                JsonPropertyStage::AwaitGet
                | JsonPropertyStage::AwaitToJsonGet
                | JsonPropertyStage::AwaitToJsonCall
                | JsonPropertyStage::AwaitReplacerCall
                | JsonPropertyStage::AwaitBoxedNumber
                | JsonPropertyStage::AwaitBoxedString => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "JSON property worklist resumed without its external completion",
                    }
                    .into());
                }
            },
            JsonStringifyFrame::Array(array) => {
                if array.pending {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "JSON Array child is pending without a property frame",
                    }
                    .into());
                }
                if array.next < array.length {
                    let index = array.next;
                    let object = array.object;
                    let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "JSON Array serialization reached the non-index u32 maximum",
                        },
                    )?);
                    let name = json_stringify_index_name(index)?;
                    let JsonStringifyFrame::Array(array) =
                        state
                            .frames
                            .last_mut()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "JSON Array frame disappeared",
                            })?
                    else {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "JSON Array frame changed kind",
                        }
                        .into());
                    };
                    array.next = array.next.saturating_add(1);
                    array.pending = true;
                    push_json_property_frame(state, StoredValue::Object(object), key, name)?;
                    continue;
                }
                if let Some(dispatch) = finish_json_container(state, true, execution_budget)? {
                    return Ok(Some(dispatch));
                }
            }
            JsonStringifyFrame::Object(object) => {
                if object.pending.is_some() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "JSON Object child is pending without a property frame",
                    }
                    .into());
                }
                if object.next < object.keys.len() {
                    let object_id = object.object;
                    let name = object.keys[object.next].clone();
                    let key = runtime.property_key_from_string(&name)?;
                    let JsonStringifyFrame::Object(object) =
                        state
                            .frames
                            .last_mut()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "JSON Object frame disappeared",
                            })?
                    else {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "JSON Object frame changed kind",
                        }
                        .into());
                    };
                    object.next = object.next.saturating_add(1);
                    object.pending = Some(name.clone());
                    push_json_property_frame(state, StoredValue::Object(object_id), key, name)?;
                    continue;
                }
                if let Some(dispatch) = finish_json_container(state, false, execution_budget)? {
                    return Ok(Some(dispatch));
                }
            }
        }
    }
}

fn resume_json_property(
    state: &mut JsonStringifyContinuation,
    completion: StoredValue,
) -> Result<(), NativeFailure> {
    let property = current_json_property_mut(state)?;
    match property.stage {
        JsonPropertyStage::AwaitGet => {
            property.value = Some(completion);
            property.stage = JsonPropertyStage::ToJson;
        }
        JsonPropertyStage::AwaitToJsonGet => {
            if let StoredValue::Function(function) = completion {
                property.stage = JsonPropertyStage::CallToJson(function);
            } else {
                property.stage = JsonPropertyStage::Replacer;
            }
        }
        JsonPropertyStage::AwaitToJsonCall => {
            property.value = Some(completion);
            property.stage = JsonPropertyStage::Replacer;
        }
        JsonPropertyStage::AwaitReplacerCall => {
            property.value = Some(completion);
            property.stage = JsonPropertyStage::Normalize;
        }
        JsonPropertyStage::Get
        | JsonPropertyStage::ToJson
        | JsonPropertyStage::CallToJson(_)
        | JsonPropertyStage::Replacer
        | JsonPropertyStage::Normalize
        | JsonPropertyStage::AwaitBoxedNumber
        | JsonPropertyStage::AwaitBoxedString => {
            return Err(EngineFault::RuntimeInvariant {
                message: "JSON property completion reached a non-awaiting stage",
            }
            .into());
        }
    }
    Ok(())
}

fn normalize_json_property(
    runtime: &mut Runtime,
    state: &mut JsonStringifyContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let value =
        current_json_property_mut(state)?
            .value
            .take()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "JSON normalization has no property value",
            })?;
    match value {
        StoredValue::Object(object) => {
            if let Some(text) = runtime.raw_json_text(object)? {
                return complete_json_property(state, Some(text), execution_budget);
            }
            if runtime.boxed_number(object)?.is_some() {
                current_json_property_mut(state)?.stage = JsonPropertyStage::AwaitBoxedNumber;
                let realm = state.realm;
                let origin = state.origin.clone();
                let owned = take_json_stringify_state(state);
                return Ok(Some(begin_operator_primitive_conversion(
                    runtime,
                    StoredValue::Object(object),
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::JsonStringifyBoxedNumber(Box::new(owned)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )?));
            }
            if runtime.boxed_string(object)?.is_some() {
                current_json_property_mut(state)?.stage = JsonPropertyStage::AwaitBoxedString;
                let realm = state.realm;
                let origin = state.origin.clone();
                let owned = take_json_stringify_state(state);
                return Ok(Some(begin_operator_primitive_conversion(
                    runtime,
                    StoredValue::Object(object),
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::JsonStringifyBoxedString(Box::new(owned)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )?));
            }
            if let Some(boolean) = runtime.boxed_boolean(object)? {
                current_json_property_mut(state)?.value = Some(StoredValue::Boolean(boolean));
                return Ok(None);
            }
            if let Some(bigint) = runtime.boxed_bigint(object)? {
                current_json_property_mut(state)?.value = Some(StoredValue::BigInt(bigint));
                return Ok(None);
            }
            enter_json_container(runtime, state, object, execution_budget)?;
            Ok(None)
        }
        StoredValue::Null => {
            complete_json_property(state, Some(JsString::from_utf8("null")?), execution_budget)
        }
        StoredValue::Boolean(false) => {
            complete_json_property(state, Some(JsString::from_utf8("false")?), execution_budget)
        }
        StoredValue::Boolean(true) => {
            complete_json_property(state, Some(JsString::from_utf8("true")?), execution_budget)
        }
        StoredValue::String(string) => {
            execution_budget.charge_instructions(u64::from(string.len()).saturating_add(1))?;
            complete_json_property(state, Some(quote_json_string(&string)?), execution_budget)
        }
        StoredValue::Number(number) => {
            let text = if number.as_f64().is_finite() {
                number.to_javascript_string()?
            } else {
                JsString::from_utf8("null")?
            };
            complete_json_property(state, Some(text), execution_budget)
        }
        StoredValue::BigInt(_) => Err(json_stringify_type_error(
            state.realm,
            state.origin.clone(),
            "bigint value cannot be serialized",
        )?),
        StoredValue::Undefined | StoredValue::Symbol(_) | StoredValue::Function(_) => {
            complete_json_property(state, None, execution_budget)
        }
    }
}

fn enter_json_container(
    runtime: &mut Runtime,
    state: &mut JsonStringifyContinuation,
    object: ObjectId,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    execution_budget.charge_instructions(usize_to_u64(state.stack.len()).saturating_add(1))?;
    if state.stack.contains(&object) {
        return Err(json_stringify_type_error(
            state.realm,
            state.origin.clone(),
            "circular reference",
        )?);
    }
    state
        .stack
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    state.stack.push(object);
    let step_back = state.indent.clone();
    state.indent = state.indent.concat(&state.gap)?;

    let replacement = if runtime.is_array_object(object)? {
        JsonStringifyFrame::Array(JsonArrayFrame {
            object,
            next: 0,
            length: runtime
                .array_length(object)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON Array lost its cached length",
                })?,
            partial: Vec::new(),
            step_back,
            pending: false,
        })
    } else {
        let keys = json_object_keys(runtime, state, object, execution_budget)?;
        JsonStringifyFrame::Object(JsonObjectFrame {
            object,
            keys,
            next: 0,
            partial: Vec::new(),
            step_back,
            pending: None,
        })
    };
    let frame = state
        .frames
        .last_mut()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "JSON container has no property frame to replace",
        })?;
    *frame = replacement;
    Ok(())
}

fn json_object_keys(
    runtime: &mut Runtime,
    state: &JsonStringifyContinuation,
    object: ObjectId,
    execution_budget: &mut ExecutionBudget,
) -> Result<Vec<JsString>, NativeFailure> {
    if let Some(property_list) = &state.property_list {
        execution_budget.charge_instructions(usize_to_u64(property_list.len()))?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(property_list.len()).map_err(|_| {
            ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: property_list.len(),
            }
        })?;
        keys.extend(property_list.iter().cloned());
        return Ok(keys);
    }
    let (snapshot, work) =
        runtime.try_own_key_snapshot(HeapReference::Object(object), 0, KeyPhases::STRING_KEYS)?;
    execution_budget.charge_instructions(work)?;
    let mut keys = Vec::new();
    keys.try_reserve_exact(snapshot.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: snapshot.len(),
        })?;
    for index in 0..snapshot.len() {
        let candidate = snapshot.get(index).ok_or(EngineFault::RuntimeInvariant {
            message: "JSON own-key snapshot shrank during enumeration",
        })?;
        if candidate.enumerable() {
            keys.push(json_stringify_property_name(candidate.key())?);
        }
    }
    Ok(keys)
}

fn complete_json_property(
    state: &mut JsonStringifyContinuation,
    result: Option<JsString>,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let Some(JsonStringifyFrame::Property(_)) = state.frames.pop() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "JSON property completion lost its property frame",
        }
        .into());
    };
    accept_json_serialization_result(state, result, execution_budget)
}

fn accept_json_serialization_result(
    state: &mut JsonStringifyContinuation,
    result: Option<JsString>,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let Some(parent) = state.frames.last_mut() else {
        let value = result.map_or(StoredValue::Undefined, StoredValue::String);
        return Ok(Some(NativeDispatch::Immediate(value)));
    };
    match parent {
        JsonStringifyFrame::Array(array) => {
            if !array.pending {
                return Err(EngineFault::RuntimeInvariant {
                    message: "JSON Array received an unrequested child completion",
                }
                .into());
            }
            array
                .partial
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            array
                .partial
                .push(result.unwrap_or(JsString::from_utf8("null")?));
            array.pending = false;
        }
        JsonStringifyFrame::Object(object) => {
            let name = object.pending.take().ok_or(EngineFault::RuntimeInvariant {
                message: "JSON Object received an unrequested child completion",
            })?;
            if let Some(result) = result {
                execution_budget.charge_instructions(u64::from(name.len()).saturating_add(1))?;
                let member = json_object_member(&name, result, !state.gap.is_empty())?;
                object
                    .partial
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                object.partial.push(member);
            }
        }
        JsonStringifyFrame::Property(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "JSON property completed directly into another property frame",
            }
            .into());
        }
    }
    Ok(None)
}

fn finish_json_container(
    state: &mut JsonStringifyContinuation,
    array: bool,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let frame = state.frames.pop().ok_or(EngineFault::RuntimeInvariant {
        message: "JSON container completion lost its frame",
    })?;
    let (object, partial, step_back) = match frame {
        JsonStringifyFrame::Array(frame) if array => (frame.object, frame.partial, frame.step_back),
        JsonStringifyFrame::Object(frame) if !array => {
            (frame.object, frame.partial, frame.step_back)
        }
        JsonStringifyFrame::Property(_)
        | JsonStringifyFrame::Array(_)
        | JsonStringifyFrame::Object(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "JSON container completion changed frame kind",
            }
            .into());
        }
    };
    let popped = state.stack.pop().ok_or(EngineFault::RuntimeInvariant {
        message: "JSON container stack became empty during completion",
    })?;
    if popped != object {
        return Err(EngineFault::RuntimeInvariant {
            message: "JSON container stack order diverged from the worklist",
        }
        .into());
    }
    execution_budget.charge_instructions(usize_to_u64(partial.len()).saturating_add(1))?;
    let final_text = json_container_text(array, &partial, &state.indent, &step_back, &state.gap)?;
    state.indent = step_back;
    accept_json_serialization_result(state, Some(final_text), execution_budget)
}

fn push_json_property_frame(
    state: &mut JsonStringifyContinuation,
    holder: StoredValue,
    key: PropertyKey,
    name: JsString,
) -> Result<(), NativeFailure> {
    state
        .frames
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state
        .frames
        .push(JsonStringifyFrame::Property(JsonPropertyFrame {
            holder,
            key,
            name,
            value: None,
            stage: JsonPropertyStage::Get,
        }));
    Ok(())
}

fn current_json_property(
    state: &JsonStringifyContinuation,
) -> Result<&JsonPropertyFrame, NativeFailure> {
    match state.frames.last() {
        Some(JsonStringifyFrame::Property(property)) => Ok(property),
        Some(JsonStringifyFrame::Array(_) | JsonStringifyFrame::Object(_)) | None => {
            Err(EngineFault::RuntimeInvariant {
                message: "JSON property operation has no current property frame",
            }
            .into())
        }
    }
}

fn current_json_property_mut(
    state: &mut JsonStringifyContinuation,
) -> Result<&mut JsonPropertyFrame, NativeFailure> {
    match state.frames.last_mut() {
        Some(JsonStringifyFrame::Property(property)) => Ok(property),
        Some(JsonStringifyFrame::Array(_) | JsonStringifyFrame::Object(_)) | None => {
            Err(EngineFault::RuntimeInvariant {
                message: "JSON property operation has no current property frame",
            }
            .into())
        }
    }
}

fn take_json_stringify_state(state: &mut JsonStringifyContinuation) -> JsonStringifyContinuation {
    let placeholder = JsonStringifyContinuation {
        value: None,
        replacer: None,
        space: None,
        replacer_function: None,
        replacer_array: None,
        property_list: None,
        gap: JsString::empty(),
        indent: JsString::empty(),
        stack: Vec::new(),
        frames: Vec::new(),
        setup: JsonStringifySetupStage::Serialize,
        realm: state.realm,
        origin: state.origin.clone(),
    };
    std::mem::replace(state, placeholder)
}

fn call_json_stringify_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: JsonStringifyContinuation,
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
    continuations.push(NativeContinuation::JsonStringify(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn json_stringify_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

fn json_stringify_property_name(key: &PropertyKey) -> Result<JsString, NativeFailure> {
    if let Some(index) = key.as_index() {
        return json_stringify_index_name(index.get());
    }
    key.as_atom()
        .and_then(|atom| atom.description())
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "JSON string key has no string description",
        })
        .map_err(NativeFailure::from)
}

fn json_stringify_index_name(index: u32) -> Result<JsString, NativeFailure> {
    JsNumber::from_u32(index)
        .to_radix_string(10)
        .map_err(NativeFailure::from)
}

fn quote_json_string(value: &JsString) -> Result<JsString, NativeFailure> {
    let capacity = usize::try_from(value.len())
        .unwrap_or(usize::MAX)
        .saturating_add(2);
    let mut output = Vec::new();
    output
        .try_reserve(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: capacity,
        })?;
    output.push(u16::from(b'"'));
    let mut index = 0_u32;
    while index < value.len() {
        let unit = value
            .code_unit_at(index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "JSON string quoting lost a code unit",
            })?;
        let escape = match unit {
            0x0008 => Some(b'b'),
            0x0009 => Some(b't'),
            0x000a => Some(b'n'),
            0x000c => Some(b'f'),
            0x000d => Some(b'r'),
            0x0022 => Some(b'"'),
            0x005c => Some(b'\\'),
            _ => None,
        };
        if let Some(escape) = escape {
            reserve_json_output(&mut output, 2)?;
            output.push(u16::from(b'\\'));
            output.push(u16::from(escape));
        } else if unit < 0x0020 || is_lone_json_surrogate(value, index, unit) {
            reserve_json_output(&mut output, 6)?;
            push_unicode_escape(&mut output, unit);
        } else {
            reserve_json_output(&mut output, 1)?;
            output.push(unit);
        }
        index = index.saturating_add(1);
    }
    reserve_json_output(&mut output, 1)?;
    output.push(u16::from(b'"'));
    Ok(JsString::from_code_units(output)?)
}

fn is_lone_json_surrogate(value: &JsString, index: u32, unit: u16) -> bool {
    if (0xd800..=0xdbff).contains(&unit) {
        return !value
            .code_unit_at(index.saturating_add(1))
            .is_some_and(|next| (0xdc00..=0xdfff).contains(&next));
    }
    if (0xdc00..=0xdfff).contains(&unit) {
        return index == 0
            || !value
                .code_unit_at(index - 1)
                .is_some_and(|previous| (0xd800..=0xdbff).contains(&previous));
    }
    false
}

fn push_unicode_escape(output: &mut Vec<u16>, unit: u16) {
    output.push(u16::from(b'\\'));
    output.push(u16::from(b'u'));
    for shift in [12_u16, 8, 4, 0] {
        let digit = ((unit >> shift) & 0x000f) as u8;
        output.push(u16::from(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        }));
    }
}

fn reserve_json_output(output: &mut Vec<u16>, additional: usize) -> Result<(), NativeFailure> {
    output.try_reserve(additional).map_err(|_| {
        NativeFailure::Execution(ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional,
        })
    })
}

fn json_object_member(
    name: &JsString,
    value: JsString,
    spaced: bool,
) -> Result<JsString, NativeFailure> {
    let colon = if spaced { ": " } else { ":" };
    concat_json_strings(&[quote_json_string(name)?, JsString::from_utf8(colon)?, value])
}

fn json_container_text(
    array: bool,
    partial: &[JsString],
    indent: &JsString,
    step_back: &JsString,
    gap: &JsString,
) -> Result<JsString, NativeFailure> {
    let (open, close) = if array { ("[", "]") } else { ("{", "}") };
    if partial.is_empty() {
        return JsString::from_utf8(if array { "[]" } else { "{}" }).map_err(NativeFailure::from);
    }
    if gap.is_empty() {
        let body = join_json_strings(partial, &JsString::from_utf8(",")?)?;
        return concat_json_strings(&[
            JsString::from_utf8(open)?,
            body,
            JsString::from_utf8(close)?,
        ]);
    }
    let newline = JsString::from_utf8("\n")?;
    let separator = concat_json_strings(&[JsString::from_utf8(",\n")?, indent.clone()])?;
    let body = join_json_strings(partial, &separator)?;
    concat_json_strings(&[
        JsString::from_utf8(open)?,
        newline.clone(),
        indent.clone(),
        body,
        newline,
        step_back.clone(),
        JsString::from_utf8(close)?,
    ])
}

fn join_json_strings(values: &[JsString], separator: &JsString) -> Result<JsString, NativeFailure> {
    let Some((first, rest)) = values.split_first() else {
        return Ok(JsString::empty());
    };
    let mut output = first.clone();
    for value in rest {
        output = output.concat(separator)?.concat(value)?;
    }
    Ok(output)
}

fn concat_json_strings(values: &[JsString]) -> Result<JsString, NativeFailure> {
    let mut output = JsString::empty();
    for value in values {
        output = output.concat(value)?;
    }
    Ok(output)
}
