/*
 * JavaScript JSON.parse semantics derived from QuickJS.
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

//! Exact ECMA-404 parsing and resumable ECMA-262 reviver traversal.

use std::collections::HashSet;

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

type JsonNodeId = usize;

#[derive(Clone, Copy)]
struct JsonSpan {
    start: u32,
    end: u32,
}

enum JsonNodeKind {
    Null,
    Boolean(bool),
    Number(JsNumber),
    String(JsString),
    Array(Vec<JsonNodeId>),
    Object(Vec<(JsString, JsonNodeId)>),
}

struct JsonNode {
    span: JsonSpan,
    kind: JsonNodeKind,
}

struct JsonDocument {
    text: JsString,
    nodes: Vec<JsonNode>,
    root: JsonNodeId,
}

enum JsonTextFailure {
    Syntax,
    Native(NativeFailure),
}

impl fmt::Debug for JsonTextFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => formatter.write_str("invalid JSON syntax"),
            Self::Native(_) => formatter.write_str("JSON parser resource failure"),
        }
    }
}

impl From<JsStringError> for JsonTextFailure {
    fn from(error: JsStringError) -> Self {
        Self::Native(NativeFailure::from(error))
    }
}

impl From<ExecutionError> for JsonTextFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Native(NativeFailure::Execution(error))
    }
}

enum JsonFrame {
    Array {
        start: usize,
        elements: Vec<JsonNodeId>,
        expecting_value: bool,
        allow_end: bool,
    },
    Object {
        start: usize,
        entries: Vec<(JsString, JsonNodeId)>,
        state: JsonObjectState,
    },
}

enum JsonObjectState {
    KeyOrEnd { allow_end: bool },
    Value { key: JsString },
    CommaOrEnd,
}

struct JsonTextParser {
    text: JsString,
    units: Vec<u16>,
    index: usize,
    nodes: Vec<JsonNode>,
    frames: Vec<JsonFrame>,
}

impl JsonTextParser {
    fn new(text: JsString) -> Result<Self, JsonTextFailure> {
        let capacity = usize::try_from(text.len()).map_err(|_| ExecutionError::LimitExceeded {
            resource: RuntimeResource::FrameValues,
            limit: usize_to_u64(usize::MAX),
            observed: u64::from(text.len()),
        })?;
        let mut units = Vec::new();
        reserve_json(&mut units, capacity)?;
        units.extend(text.code_units());
        Ok(Self {
            text,
            units,
            index: 0,
            nodes: Vec::new(),
            frames: Vec::new(),
        })
    }

    fn parse(mut self) -> Result<JsonDocument, JsonTextFailure> {
        let mut pending = self.parse_value_start()?;
        loop {
            if let Some(node) = pending.take() {
                if let Some(frame) = self.frames.last_mut() {
                    match frame {
                        JsonFrame::Array {
                            elements,
                            expecting_value,
                            ..
                        } => {
                            if !*expecting_value {
                                return Err(JsonTextFailure::Syntax);
                            }
                            reserve_json(elements, 1)?;
                            elements.push(node);
                            *expecting_value = false;
                        }
                        JsonFrame::Object { entries, state, .. } => {
                            let JsonObjectState::Value { key } =
                                std::mem::replace(state, JsonObjectState::CommaOrEnd)
                            else {
                                return Err(JsonTextFailure::Syntax);
                            };
                            reserve_json(entries, 1)?;
                            entries.push((key, node));
                        }
                    }
                    continue;
                }
                self.skip_whitespace();
                if self.index != self.units.len() {
                    return Err(JsonTextFailure::Syntax);
                }
                return Ok(JsonDocument {
                    text: self.text,
                    nodes: self.nodes,
                    root: node,
                });
            }

            let frame_index = self
                .frames
                .len()
                .checked_sub(1)
                .ok_or(JsonTextFailure::Syntax)?;
            let is_array = matches!(self.frames[frame_index], JsonFrame::Array { .. });
            if is_array {
                pending = self.advance_array_frame(frame_index)?;
            } else {
                pending = self.advance_object_frame(frame_index)?;
            }
        }
    }

    fn advance_array_frame(
        &mut self,
        frame_index: usize,
    ) -> Result<Option<JsonNodeId>, JsonTextFailure> {
        self.skip_whitespace();
        let (expecting_value, allow_end) = match &self.frames[frame_index] {
            JsonFrame::Array {
                expecting_value,
                allow_end,
                ..
            } => (*expecting_value, *allow_end),
            JsonFrame::Object { .. } => return Err(JsonTextFailure::Syntax),
        };
        if expecting_value {
            if allow_end && self.consume(u16::from(b']')) {
                return self.finish_container(frame_index);
            }
            return self.parse_value_start();
        }
        if self.consume(u16::from(b',')) {
            let JsonFrame::Array {
                expecting_value,
                allow_end,
                ..
            } = &mut self.frames[frame_index]
            else {
                return Err(JsonTextFailure::Syntax);
            };
            *expecting_value = true;
            *allow_end = false;
            return Ok(None);
        }
        if self.consume(u16::from(b']')) {
            return self.finish_container(frame_index);
        }
        Err(JsonTextFailure::Syntax)
    }

    fn advance_object_frame(
        &mut self,
        frame_index: usize,
    ) -> Result<Option<JsonNodeId>, JsonTextFailure> {
        self.skip_whitespace();
        let state = match &self.frames[frame_index] {
            JsonFrame::Object { state, .. } => match state {
                JsonObjectState::KeyOrEnd { allow_end } => JsonObjectAction::KeyOrEnd(*allow_end),
                JsonObjectState::Value { .. } => JsonObjectAction::Value,
                JsonObjectState::CommaOrEnd => JsonObjectAction::CommaOrEnd,
            },
            JsonFrame::Array { .. } => return Err(JsonTextFailure::Syntax),
        };
        match state {
            JsonObjectAction::KeyOrEnd(allow_end) => {
                if allow_end && self.consume(u16::from(b'}')) {
                    return self.finish_container(frame_index);
                }
                if self.peek() != Some(u16::from(b'"')) {
                    return Err(JsonTextFailure::Syntax);
                }
                let (key, _) = self.parse_string()?;
                self.skip_whitespace();
                if !self.consume(u16::from(b':')) {
                    return Err(JsonTextFailure::Syntax);
                }
                let JsonFrame::Object { state, .. } = &mut self.frames[frame_index] else {
                    return Err(JsonTextFailure::Syntax);
                };
                *state = JsonObjectState::Value { key };
                Ok(None)
            }
            JsonObjectAction::Value => self.parse_value_start(),
            JsonObjectAction::CommaOrEnd => {
                if self.consume(u16::from(b',')) {
                    let JsonFrame::Object { state, .. } = &mut self.frames[frame_index] else {
                        return Err(JsonTextFailure::Syntax);
                    };
                    *state = JsonObjectState::KeyOrEnd { allow_end: false };
                    return Ok(None);
                }
                if self.consume(u16::from(b'}')) {
                    return self.finish_container(frame_index);
                }
                Err(JsonTextFailure::Syntax)
            }
        }
    }

    fn finish_container(
        &mut self,
        frame_index: usize,
    ) -> Result<Option<JsonNodeId>, JsonTextFailure> {
        if frame_index + 1 != self.frames.len() {
            return Err(JsonTextFailure::Syntax);
        }
        let frame = self.frames.pop().ok_or(JsonTextFailure::Syntax)?;
        let end = self.index;
        let (start, kind) = match frame {
            JsonFrame::Array {
                start, elements, ..
            } => (start, JsonNodeKind::Array(elements)),
            JsonFrame::Object { start, entries, .. } => (start, JsonNodeKind::Object(entries)),
        };
        self.push_node(start, end, kind).map(Some)
    }

    fn parse_value_start(&mut self) -> Result<Option<JsonNodeId>, JsonTextFailure> {
        self.skip_whitespace();
        let start = self.index;
        match self.peek() {
            Some(unit) if unit == u16::from(b'[') => {
                self.index += 1;
                reserve_json(&mut self.frames, 1)?;
                self.frames.push(JsonFrame::Array {
                    start,
                    elements: Vec::new(),
                    expecting_value: true,
                    allow_end: true,
                });
                Ok(None)
            }
            Some(unit) if unit == u16::from(b'{') => {
                self.index += 1;
                reserve_json(&mut self.frames, 1)?;
                self.frames.push(JsonFrame::Object {
                    start,
                    entries: Vec::new(),
                    state: JsonObjectState::KeyOrEnd { allow_end: true },
                });
                Ok(None)
            }
            Some(unit) if unit == u16::from(b'"') => {
                let (value, span) = self.parse_string()?;
                self.push_node(
                    usize::try_from(span.start).map_err(|_| JsonTextFailure::Syntax)?,
                    usize::try_from(span.end).map_err(|_| JsonTextFailure::Syntax)?,
                    JsonNodeKind::String(value),
                )
                .map(Some)
            }
            Some(unit) if unit == u16::from(b't') => {
                self.parse_literal(b"true")?;
                self.push_node(start, self.index, JsonNodeKind::Boolean(true))
                    .map(Some)
            }
            Some(unit) if unit == u16::from(b'f') => {
                self.parse_literal(b"false")?;
                self.push_node(start, self.index, JsonNodeKind::Boolean(false))
                    .map(Some)
            }
            Some(unit) if unit == u16::from(b'n') => {
                self.parse_literal(b"null")?;
                self.push_node(start, self.index, JsonNodeKind::Null)
                    .map(Some)
            }
            Some(unit) if unit == u16::from(b'-') || is_ascii_digit(unit) => {
                let number = self.parse_number()?;
                self.push_node(start, self.index, JsonNodeKind::Number(number))
                    .map(Some)
            }
            _ => Err(JsonTextFailure::Syntax),
        }
    }

    fn parse_literal(&mut self, expected: &[u8]) -> Result<(), JsonTextFailure> {
        for byte in expected {
            if !self.consume(u16::from(*byte)) {
                return Err(JsonTextFailure::Syntax);
            }
        }
        Ok(())
    }

    fn parse_string(&mut self) -> Result<(JsString, JsonSpan), JsonTextFailure> {
        let start = self.index;
        if !self.consume(u16::from(b'"')) {
            return Err(JsonTextFailure::Syntax);
        }
        let mut decoded = Vec::new();
        loop {
            let unit = self.peek().ok_or(JsonTextFailure::Syntax)?;
            self.index += 1;
            match unit {
                unit if unit == u16::from(b'"') => {
                    let value = JsString::from_code_units(decoded)?;
                    return Ok((value, Self::span(start, self.index)?));
                }
                unit if unit == u16::from(b'\\') => {
                    let escaped = self.peek().ok_or(JsonTextFailure::Syntax)?;
                    self.index += 1;
                    let decoded_unit = match escaped {
                        unit if unit == u16::from(b'"') => u16::from(b'"'),
                        unit if unit == u16::from(b'\\') => u16::from(b'\\'),
                        unit if unit == u16::from(b'/') => u16::from(b'/'),
                        unit if unit == u16::from(b'b') => 0x0008,
                        unit if unit == u16::from(b'f') => 0x000c,
                        unit if unit == u16::from(b'n') => 0x000a,
                        unit if unit == u16::from(b'r') => 0x000d,
                        unit if unit == u16::from(b't') => 0x0009,
                        unit if unit == u16::from(b'u') => self.parse_hex_escape()?,
                        _ => return Err(JsonTextFailure::Syntax),
                    };
                    reserve_json(&mut decoded, 1)?;
                    decoded.push(decoded_unit);
                }
                0x0000..=0x001f => return Err(JsonTextFailure::Syntax),
                unit => {
                    reserve_json(&mut decoded, 1)?;
                    decoded.push(unit);
                }
            }
        }
    }

    fn parse_hex_escape(&mut self) -> Result<u16, JsonTextFailure> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let unit = self.peek().ok_or(JsonTextFailure::Syntax)?;
            self.index += 1;
            let digit = match unit {
                unit if is_ascii_digit(unit) => unit - u16::from(b'0'),
                unit if (u16::from(b'a')..=u16::from(b'f')).contains(&unit) => {
                    unit - u16::from(b'a') + 10
                }
                unit if (u16::from(b'A')..=u16::from(b'F')).contains(&unit) => {
                    unit - u16::from(b'A') + 10
                }
                _ => return Err(JsonTextFailure::Syntax),
            };
            value = value.saturating_mul(16).saturating_add(digit);
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsNumber, JsonTextFailure> {
        let start = self.index;
        let _ = self.consume(u16::from(b'-'));
        match self.peek() {
            Some(unit) if unit == u16::from(b'0') => {
                self.index += 1;
                if self.peek().is_some_and(is_ascii_digit) {
                    return Err(JsonTextFailure::Syntax);
                }
            }
            Some(unit) if (u16::from(b'1')..=u16::from(b'9')).contains(&unit) => {
                self.index += 1;
                while self.peek().is_some_and(is_ascii_digit) {
                    self.index += 1;
                }
            }
            _ => return Err(JsonTextFailure::Syntax),
        }
        if self.consume(u16::from(b'.')) {
            if !self.peek().is_some_and(is_ascii_digit) {
                return Err(JsonTextFailure::Syntax);
            }
            while self.peek().is_some_and(is_ascii_digit) {
                self.index += 1;
            }
        }
        if self
            .peek()
            .is_some_and(|unit| unit == u16::from(b'e') || unit == u16::from(b'E'))
        {
            self.index += 1;
            if self
                .peek()
                .is_some_and(|unit| unit == u16::from(b'+') || unit == u16::from(b'-'))
            {
                self.index += 1;
            }
            if !self.peek().is_some_and(is_ascii_digit) {
                return Err(JsonTextFailure::Syntax);
            }
            while self.peek().is_some_and(is_ascii_digit) {
                self.index += 1;
            }
        }
        let number_text = JsString::from_code_units(self.units[start..self.index].iter().copied())?;
        Ok(string_to_number(&number_text)?)
    }

    fn push_node(
        &mut self,
        start: usize,
        end: usize,
        kind: JsonNodeKind,
    ) -> Result<JsonNodeId, JsonTextFailure> {
        let span = Self::span(start, end)?;
        reserve_json(&mut self.nodes, 1)?;
        let id = self.nodes.len();
        self.nodes.push(JsonNode { span, kind });
        Ok(id)
    }

    fn span(start: usize, end: usize) -> Result<JsonSpan, JsonTextFailure> {
        Ok(JsonSpan {
            start: u32::try_from(start).map_err(|_| JsonTextFailure::Syntax)?,
            end: u32::try_from(end).map_err(|_| JsonTextFailure::Syntax)?,
        })
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(is_json_whitespace) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<u16> {
        self.units.get(self.index).copied()
    }

    fn consume(&mut self, expected: u16) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy)]
enum JsonObjectAction {
    KeyOrEnd(bool),
    Value,
    CommaOrEnd,
}

const fn is_json_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009 | 0x000a | 0x000d | 0x0020)
}

const fn is_ascii_digit(unit: u16) -> bool {
    unit >= 0x0030 && unit <= 0x0039
}

fn reserve_json<T>(values: &mut Vec<T>, additional: usize) -> Result<(), JsonTextFailure> {
    values.try_reserve(additional).map_err(|_| {
        JsonTextFailure::Native(NativeFailure::Execution(ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional,
        }))
    })
}

struct JsonSnapshot {
    document: JsonDocument,
    initial: Vec<Option<StoredValue>>,
}

impl JsonSnapshot {
    fn initial(&self, node: JsonNodeId) -> Option<&StoredValue> {
        self.initial.get(node).and_then(Option::as_ref)
    }

    fn record_for_object_key(&self, node: JsonNodeId, name: &JsString) -> Option<JsonNodeId> {
        let JsonNodeKind::Object(entries) = &self.document.nodes.get(node)?.kind else {
            return None;
        };
        entries.iter().rev().find_map(|(key, child)| {
            (key == name && self.initial(*child).is_some()).then_some(*child)
        })
    }

    fn record_for_array_index(&self, node: JsonNodeId, index: u32) -> Option<JsonNodeId> {
        let JsonNodeKind::Array(elements) = &self.document.nodes.get(node)?.kind else {
            return None;
        };
        elements
            .get(usize::try_from(index).ok()?)
            .copied()
            .filter(|child| self.initial(*child).is_some())
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for value in self.initial.iter().flatten() {
            trace_stored_value_root(value, mark);
        }
    }

    fn retained_values(&self) -> u64 {
        usize_to_u64(self.initial.iter().filter(|value| value.is_some()).count())
    }
}

enum JsonMaterializeTask {
    Visit(JsonNodeId),
    DefineArray {
        array: ObjectId,
        index: u32,
        child: JsonNodeId,
    },
    DefineObject {
        object: ObjectId,
        key: JsString,
        child: JsonNodeId,
    },
}

#[allow(
    clippy::too_many_lines,
    reason = "one worklist loop preserves JSON allocation and property-definition order without recursion"
)]
fn materialize_json(
    runtime: &mut Runtime,
    realm: RealmId,
    document: JsonDocument,
    execution_budget: &mut ExecutionBudget,
) -> Result<(StoredValue, JsonSnapshot), NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(document.nodes.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: document.nodes.len(),
        })?;
    values.resize_with(document.nodes.len(), || None);
    let mut tasks = Vec::new();
    tasks
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    tasks.push(JsonMaterializeTask::Visit(document.root));
    while let Some(task) = tasks.pop() {
        execution_budget.charge_instructions(1)?;
        match task {
            JsonMaterializeTask::Visit(node) => {
                let record = document
                    .nodes
                    .get(node)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON materializer reached a missing parse node",
                    })?;
                let value = match &record.kind {
                    JsonNodeKind::Null => StoredValue::Null,
                    JsonNodeKind::Boolean(value) => StoredValue::Boolean(*value),
                    JsonNodeKind::Number(value) => StoredValue::Number(*value),
                    JsonNodeKind::String(value) => StoredValue::String(value.clone()),
                    JsonNodeKind::Array(elements) => {
                        let length = u32::try_from(elements.len()).map_err(|_| {
                            ExecutionError::LimitExceeded {
                                resource: RuntimeResource::ObjectProperties,
                                limit: u64::from(u32::MAX),
                                observed: usize_to_u64(elements.len()),
                            }
                        })?;
                        let prototype = runtime.realm_array_prototype(realm)?;
                        let array = runtime.allocate_sparse_array_with_prototype(
                            HeapReference::Object(prototype),
                            length,
                        )?;
                        tasks
                            .try_reserve(elements.len().saturating_mul(2))
                            .map_err(|_| ExecutionError::AllocationFailed {
                                resource: RuntimeResource::FrameValues,
                                additional: elements.len().saturating_mul(2),
                            })?;
                        for (index, child) in elements.iter().copied().enumerate().rev() {
                            let index = u32::try_from(index).map_err(|_| {
                                ExecutionError::LimitExceeded {
                                    resource: RuntimeResource::ObjectProperties,
                                    limit: u64::from(u32::MAX),
                                    observed: usize_to_u64(index),
                                }
                            })?;
                            tasks.push(JsonMaterializeTask::DefineArray {
                                array,
                                index,
                                child,
                            });
                            tasks.push(JsonMaterializeTask::Visit(child));
                        }
                        StoredValue::Object(array)
                    }
                    JsonNodeKind::Object(entries) => {
                        let object = runtime
                            .allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
                        tasks
                            .try_reserve(entries.len().saturating_mul(2))
                            .map_err(|_| ExecutionError::AllocationFailed {
                                resource: RuntimeResource::FrameValues,
                                additional: entries.len().saturating_mul(2),
                            })?;
                        for (key, child) in entries.iter().rev() {
                            tasks.push(JsonMaterializeTask::DefineObject {
                                object,
                                key: key.clone(),
                                child: *child,
                            });
                            tasks.push(JsonMaterializeTask::Visit(*child));
                        }
                        StoredValue::Object(object)
                    }
                };
                *values.get_mut(node).ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON materializer lost its value slot",
                })? = Some(value);
            }
            JsonMaterializeTask::DefineArray {
                array,
                index,
                child,
            } => {
                let value = values
                    .get(child)
                    .and_then(Option::as_ref)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON array element completed before its value",
                    })?
                    .duplicate();
                let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "JSON array element reached the non-index u32 maximum",
                    },
                )?);
                ensure_json_definition(&define_static_property(
                    runtime,
                    &StoredValue::Object(array),
                    key,
                    value,
                    execution_budget,
                )?)?;
            }
            JsonMaterializeTask::DefineObject { object, key, child } => {
                let value = values
                    .get(child)
                    .and_then(Option::as_ref)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON object member completed before its value",
                    })?
                    .duplicate();
                let key = runtime.property_key_from_string(&key)?;
                ensure_json_definition(&define_static_property(
                    runtime,
                    &StoredValue::Object(object),
                    key,
                    value,
                    execution_budget,
                )?)?;
            }
        }
    }

    let unfiltered = values
        .get(document.root)
        .and_then(Option::as_ref)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "JSON materializer completed without a root value",
        })?
        .duplicate();
    let mut initial = Vec::new();
    initial
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: values.len(),
        })?;
    initial.resize_with(values.len(), || None);
    let mut reachable = Vec::new();
    reachable
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    reachable.push(document.root);
    while let Some(node) = reachable.pop() {
        execution_budget.charge_instructions(1)?;
        let value =
            values
                .get_mut(node)
                .and_then(Option::take)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "reachable JSON parse node has no materialized value",
                })?;
        initial[node] = Some(value);
        match &document.nodes[node].kind {
            JsonNodeKind::Array(elements) => {
                reachable.try_reserve(elements.len()).map_err(|_| {
                    ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: elements.len(),
                    }
                })?;
                reachable.extend(elements.iter().copied());
            }
            JsonNodeKind::Object(entries) => {
                let mut seen = HashSet::new();
                seen.try_reserve(entries.len())
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: entries.len(),
                    })?;
                reachable.try_reserve(entries.len()).map_err(|_| {
                    ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: entries.len(),
                    }
                })?;
                for (key, child) in entries.iter().rev() {
                    if seen.insert(key.clone()) {
                        reachable.push(*child);
                    }
                }
            }
            JsonNodeKind::Null
            | JsonNodeKind::Boolean(_)
            | JsonNodeKind::Number(_)
            | JsonNodeKind::String(_) => {}
        }
    }
    Ok((unfiltered, JsonSnapshot { document, initial }))
}

fn ensure_json_definition(outcome: &PropertyWriteOutcome) -> Result<(), NativeFailure> {
    match outcome {
        PropertyWriteOutcome::Complete => Ok(()),
        PropertyWriteOutcome::Setter { .. } | PropertyWriteOutcome::Failed(_) => {
            Err(EngineFault::RuntimeInvariant {
                message: "fresh JSON container refused a data property",
            }
            .into())
        }
    }
}

/// The source operand retained while `JSON.parse` performs `ToString`.
pub(super) struct JsonParseTextContinuation {
    reviver: StoredValue,
}

impl JsonParseTextContinuation {
    #[allow(
        clippy::unused_self,
        reason = "uniform continuation accounting keeps the retained reviver explicit"
    )]
    pub(super) const fn retained_values(&self) -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.reviver, mark);
    }
}

#[derive(Clone, Copy)]
enum JsonInternalizeStage {
    AwaitGet,
    Walk,
    AwaitReviver,
}

enum JsonTraversal {
    None,
    Array {
        next: u32,
        length: u32,
        record: Option<JsonNodeId>,
    },
    Object {
        children: Vec<JsonChild>,
        next: usize,
    },
}

struct JsonChild {
    key: PropertyKey,
    name: JsString,
    record: Option<JsonNodeId>,
}

struct JsonInternalizeFrame {
    holder: StoredValue,
    key: PropertyKey,
    name: JsString,
    record: Option<JsonNodeId>,
    value: Option<StoredValue>,
    context: Option<ObjectId>,
    traversal: JsonTraversal,
    pending_child: Option<PropertyKey>,
    stage: JsonInternalizeStage,
}

/// One suspended `InternalizeJSONProperty` worklist.
pub(super) struct JsonParseContinuation {
    snapshot: JsonSnapshot,
    reviver: FunctionId,
    frames: Vec<JsonInternalizeFrame>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl JsonParseContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        let frames = self.frames.iter().fold(0_u64, |count, frame| {
            let keys = match &frame.traversal {
                JsonTraversal::Object { children, .. } => usize_to_u64(children.len()),
                JsonTraversal::None | JsonTraversal::Array { .. } => 0,
            };
            count
                .saturating_add(1)
                .saturating_add(u64::from(frame.value.is_some()))
                .saturating_add(u64::from(frame.context.is_some()))
                .saturating_add(keys)
        });
        1_u64
            .saturating_add(self.snapshot.retained_values())
            .saturating_add(frames)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(self.reviver)));
        self.snapshot.trace_roots(mark);
        for frame in &self.frames {
            trace_stored_value_root(&frame.holder, mark);
            if let Some(value) = &frame.value {
                trace_stored_value_root(value, mark);
            }
            if let Some(context) = frame.context {
                mark(CollectionRoot::Heap(HeapReference::Object(context)));
            }
        }
    }
}

/// Implements the unforgeable `[[IsRawJSON]]` brand test.
pub(super) fn json_is_raw_json(
    runtime: &Runtime,
    value: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let branded = match value {
        StoredValue::Object(object) => runtime.is_raw_json_object(*object)?,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::BigInt(_)
        | StoredValue::Function(_) => false,
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(branded)))
}

/// Begins `JSON.rawJSON` with the specification's observable `ToString`.
pub(super) fn begin_json_raw_json(
    runtime: &mut Runtime,
    text: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_operator_primitive_conversion(
        runtime,
        text,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::JsonRawJsonText,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Validates one primitive JSON text and creates its frozen branded wrapper.
pub(super) fn finish_json_raw_json_text(
    runtime: &mut Runtime,
    text: JsString,
    realm: RealmId,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(u64::from(text.len()).saturating_add(1))?;
    let Some(last_index) = text.len().checked_sub(1) else {
        return Err(NativeFailure::Abrupt(json_syntax_exception(realm, origin)?));
    };
    let first = text.code_unit_at(0).ok_or(EngineFault::RuntimeInvariant {
        message: "non-empty raw JSON text has no first code unit",
    })?;
    let last = text
        .code_unit_at(last_index)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "non-empty raw JSON text has no last code unit",
        })?;
    if !is_raw_json_first_code_unit(first) || !is_raw_json_last_code_unit(last) {
        return Err(NativeFailure::Abrupt(json_syntax_exception(realm, origin)?));
    }

    let document = match JsonTextParser::new(text.clone()).and_then(JsonTextParser::parse) {
        Ok(document) => document,
        Err(JsonTextFailure::Syntax) => {
            return Err(NativeFailure::Abrupt(json_syntax_exception(realm, origin)?));
        }
        Err(JsonTextFailure::Native(failure)) => return Err(failure),
    };
    let root = document
        .nodes
        .get(document.root)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "raw JSON parser completed without a root node",
        })?;
    if !matches!(
        root.kind,
        JsonNodeKind::Null
            | JsonNodeKind::Boolean(_)
            | JsonNodeKind::Number(_)
            | JsonNodeKind::String(_)
    ) {
        return Err(EngineFault::RuntimeInvariant {
            message: "raw JSON boundary checks admitted an object or array",
        }
        .into());
    }
    let object = runtime.allocate_raw_json_object(text)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn is_raw_json_first_code_unit(unit: u16) -> bool {
    is_ascii_lowercase(unit)
        || is_ascii_digit(unit)
        || unit == u16::from(b'"')
        || unit == u16::from(b'-')
}

fn is_raw_json_last_code_unit(unit: u16) -> bool {
    is_ascii_lowercase(unit) || is_ascii_digit(unit) || unit == u16::from(b'"')
}

fn is_ascii_lowercase(unit: u16) -> bool {
    unit >= u16::from(b'a') && unit <= u16::from(b'z')
}

fn json_syntax_exception(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::SyntaxError,
            message: JsString::from_utf8("invalid JSON")?,
        },
        origin,
    })
}

/// Begins `ToString(text)` before validating any JSON grammar.
pub(super) fn begin_json_parse(
    runtime: &mut Runtime,
    text: StoredValue,
    reviver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_operator_primitive_conversion(
        runtime,
        text,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::JsonParseText(JsonParseTextContinuation { reviver }),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Parses exact JSON, materializes the result, and starts a callable reviver.
pub(super) fn finish_json_parse_text(
    runtime: &mut Runtime,
    state: JsonParseTextContinuation,
    text: JsString,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let JsonParseTextContinuation { reviver } = state;
    execution_budget.charge_instructions(u64::from(text.len()).saturating_add(1))?;
    let document = match JsonTextParser::new(text).and_then(JsonTextParser::parse) {
        Ok(document) => document,
        Err(JsonTextFailure::Syntax) => {
            return Err(NativeFailure::Abrupt(json_syntax_exception(realm, origin)?));
        }
        Err(JsonTextFailure::Native(failure)) => return Err(failure),
    };
    let root_node = document.root;
    let (unfiltered, snapshot) = materialize_json(runtime, realm, document, execution_budget)?;
    let StoredValue::Function(reviver) = reviver else {
        return Ok(NativeDispatch::Immediate(unfiltered));
    };

    let root = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    runtime.append_data_property(
        HeapReference::Object(root),
        runtime.predefined_property_key(PredefinedAtom::EmptyString),
        PropertyLayout::data(true, true, true),
        unfiltered,
    )?;
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    frames.push(JsonInternalizeFrame {
        holder: StoredValue::Object(root),
        key: runtime.predefined_property_key(PredefinedAtom::EmptyString),
        name: JsString::empty(),
        record: Some(root_node),
        value: None,
        context: None,
        traversal: JsonTraversal::None,
        pending_child: None,
        stage: JsonInternalizeStage::AwaitGet,
    });
    drive_json_parse(
        runtime,
        JsonParseContinuation {
            snapshot,
            reviver,
            frames,
            realm,
            origin,
        },
        None,
        return_to,
        execution_budget,
    )
}

/// Resumes after a holder getter or the reviver itself returns.
pub(super) fn advance_json_parse(
    runtime: &mut Runtime,
    state: JsonParseContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    drive_json_parse(
        runtime,
        state,
        Some(completion),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one explicit worklist keeps InternalizeJSONProperty getter, child, mutation, and callback order auditable"
)]
fn drive_json_parse(
    runtime: &mut Runtime,
    mut state: JsonParseContinuation,
    mut completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        execution_budget.charge_instructions(1)?;
        if let Some(value) = completion.take() {
            let phase = state.frames.last().map(|frame| frame.stage).ok_or(
                EngineFault::RuntimeInvariant {
                    message: "JSON reviver completion has no internalize frame",
                },
            )?;
            match phase {
                JsonInternalizeStage::AwaitGet => {
                    initialize_json_frame(runtime, &mut state, value, execution_budget)?;
                }
                JsonInternalizeStage::AwaitReviver => {
                    let _completed = state.frames.pop().ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON reviver completion lost its frame",
                    })?;
                    let Some(parent) = state.frames.last_mut() else {
                        return Ok(NativeDispatch::Immediate(value));
                    };
                    let key = parent
                        .pending_child
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "JSON child reviver completed without a parent key",
                        })?;
                    let target = parent.value.as_ref().ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON child reviver parent has no traversed value",
                    })?;
                    if matches!(value, StoredValue::Undefined) {
                        charge_heap_property_lookup(runtime, target, execution_budget)?;
                        let _ = delete_static_property(runtime, target, &key)?;
                    } else {
                        let _ =
                            define_static_property(runtime, target, key, value, execution_budget)?;
                    }
                }
                JsonInternalizeStage::Walk => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "JSON internalize walk received an external completion",
                    }
                    .into());
                }
            }
            continue;
        }

        let phase =
            state
                .frames
                .last()
                .map(|frame| frame.stage)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON internalize worklist became empty before completion",
                })?;
        match phase {
            JsonInternalizeStage::AwaitGet => {
                let frame = state.frames.last().ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON property read has no frame",
                })?;
                charge_heap_property_lookup(runtime, &frame.holder, execution_budget)?;
                match read_static_property(runtime, state.realm, &frame.holder, &frame.key)? {
                    PropertyReadOutcome::Value(value) => completion = Some(value),
                    PropertyReadOutcome::Getter { function, receiver } => {
                        return call_json_function(
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
            JsonInternalizeStage::Walk => {
                if let Some(child) = next_json_child(
                    &state.snapshot,
                    state
                        .frames
                        .last_mut()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "JSON child traversal has no frame",
                        })?,
                )? {
                    let parent = state
                        .frames
                        .last_mut()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "JSON child traversal lost its parent frame",
                        })?;
                    let holder = parent.value.as_ref().ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON child traversal parent has no value",
                    })?;
                    parent.pending_child = Some(child.key.clone());
                    let child_frame = JsonInternalizeFrame {
                        holder: holder.duplicate(),
                        key: child.key,
                        name: child.name,
                        record: child.record,
                        value: None,
                        context: None,
                        traversal: JsonTraversal::None,
                        pending_child: None,
                        stage: JsonInternalizeStage::AwaitGet,
                    };
                    state
                        .frames
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    state.frames.push(child_frame);
                    continue;
                }

                let frame = state
                    .frames
                    .last_mut()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "JSON reviver call has no frame",
                    })?;
                let value = frame.value.as_ref().ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON reviver call has no property value",
                })?;
                let context = frame.context.ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON reviver call has no context object",
                })?;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(3)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 3,
                    })?;
                arguments.push(StoredValue::String(frame.name.clone()));
                arguments.push(value.duplicate());
                arguments.push(StoredValue::Object(context));
                let receiver = frame.holder.duplicate();
                frame.stage = JsonInternalizeStage::AwaitReviver;
                return call_json_function(state.reviver, receiver, arguments, state, return_to);
            }
            JsonInternalizeStage::AwaitReviver => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "JSON reviver frame resumed without a callback completion",
                }
                .into());
            }
        }
    }
}

fn initialize_json_frame(
    runtime: &mut Runtime,
    state: &mut JsonParseContinuation,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let frame_index = state
        .frames
        .len()
        .checked_sub(1)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "JSON property initialization has no frame",
        })?;
    let applies = state.frames[frame_index]
        .record
        .and_then(|record| state.snapshot.initial(record))
        .is_some_and(|initial| initial.same_value(&value));
    if !applies {
        state.frames[frame_index].record = None;
    }

    let context = runtime.allocate_ordinary_object(runtime.realm_object_prototype(state.realm)?)?;
    if applies && value.heap_reference().is_none() {
        let record = state.frames[frame_index]
            .record
            .ok_or(EngineFault::RuntimeInvariant {
                message: "applicable JSON primitive lost its parse record",
            })?;
        let span = state
            .snapshot
            .document
            .nodes
            .get(record)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "JSON primitive source record is missing",
            })?
            .span;
        let source = state.snapshot.document.text.slice(span.start..span.end)?;
        runtime.append_data_property(
            HeapReference::Object(context),
            runtime.predefined_property_key(PredefinedAtom::Source),
            PropertyLayout::data(true, true, true),
            StoredValue::String(source),
        )?;
    }

    let traversal = if let StoredValue::Object(object) = value
        && runtime.is_array_object(object)?
    {
        JsonTraversal::Array {
            next: 0,
            length: runtime
                .array_length(object)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "JSON array lost its cached length",
                })?,
            record: state.frames[frame_index].record,
        }
    } else if let Some(reference) = value.heap_reference() {
        let (snapshot, work) =
            runtime.try_own_key_snapshot(reference, 0, KeyPhases::STRING_KEYS)?;
        execution_budget.charge_instructions(work)?;
        let mut children = Vec::new();
        children.try_reserve_exact(snapshot.len()).map_err(|_| {
            ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: snapshot.len(),
            }
        })?;
        for index in 0..snapshot.len() {
            let candidate = snapshot.get(index).ok_or(EngineFault::RuntimeInvariant {
                message: "JSON own-key snapshot shrank during traversal",
            })?;
            if !candidate.enumerable() {
                continue;
            }
            let key = candidate.key().clone();
            let name = json_property_key_string(&key)?;
            let record = state.frames[frame_index]
                .record
                .and_then(|record| state.snapshot.record_for_object_key(record, &name));
            children.push(JsonChild { key, name, record });
        }
        JsonTraversal::Object { children, next: 0 }
    } else {
        JsonTraversal::None
    };

    let frame = &mut state.frames[frame_index];
    frame.value = Some(value);
    frame.context = Some(context);
    frame.traversal = traversal;
    frame.stage = JsonInternalizeStage::Walk;
    Ok(())
}

fn next_json_child(
    snapshot: &JsonSnapshot,
    frame: &mut JsonInternalizeFrame,
) -> Result<Option<JsonChild>, NativeFailure> {
    match &mut frame.traversal {
        JsonTraversal::None => Ok(None),
        JsonTraversal::Array {
            next,
            length,
            record,
        } => {
            if *next >= *length {
                return Ok(None);
            }
            let index = *next;
            *next = next.saturating_add(1);
            let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
                EngineFault::RuntimeInvariant {
                    message: "JSON reviver index reached the non-index u32 maximum",
                },
            )?);
            let name = json_index_name(index)?;
            let record = record.and_then(|record| snapshot.record_for_array_index(record, index));
            Ok(Some(JsonChild { key, name, record }))
        }
        JsonTraversal::Object { children, next } => {
            let Some(child) = children.get(*next) else {
                return Ok(None);
            };
            *next = next.saturating_add(1);
            Ok(Some(JsonChild {
                key: child.key.clone(),
                name: child.name.clone(),
                record: child.record,
            }))
        }
    }
}

fn call_json_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: JsonParseContinuation,
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
    continuations.push(NativeContinuation::JsonParse(Box::new(state)));
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

fn json_property_key_string(key: &PropertyKey) -> Result<JsString, NativeFailure> {
    if let Some(index) = key.as_index() {
        return json_index_name(index.get());
    }
    key.as_atom()
        .and_then(|atom| atom.description())
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "JSON string key has no string description",
        })
        .map_err(NativeFailure::from)
}

fn json_index_name(index: u32) -> Result<JsString, NativeFailure> {
    JsNumber::from_u32(index)
        .to_radix_string(10)
        .map_err(NativeFailure::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<JsonDocument, JsonTextFailure> {
        JsonTextParser::new(JsString::from_utf8(text).expect("test JSON string"))?.parse()
    }

    #[test]
    fn parser_accepts_exact_json_and_preserves_source_spans() {
        let document = parse(" {\"a\":-0,\"a\":[true,null,\"\\ud800\"]} ").expect("valid JSON");
        let JsonNodeKind::Object(entries) = &document.nodes[document.root].kind else {
            panic!("root should be an object");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, JsString::from_utf8("a").expect("key"));
        let JsonNodeKind::Number(number) = document.nodes[entries[0].1].kind else {
            panic!("first value should be a number");
        };
        assert!(number.same_value(JsNumber::from_f64(-0.0)));
        assert_eq!(
            document.nodes[entries[0].1].span.start, 6,
            "number source starts at the minus sign"
        );
    }

    #[test]
    fn parser_rejects_javascript_extensions_and_trailing_separators() {
        for invalid in [
            "",
            "undefined",
            "NaN",
            "+1",
            "01",
            "1.",
            "[1,]",
            "{'a':1}",
            "{\"a\":1,}",
            "\"\\x41\"",
            "true false",
        ] {
            assert!(
                matches!(parse(invalid), Err(JsonTextFailure::Syntax)),
                "accepted {invalid:?}"
            );
        }
    }
}
