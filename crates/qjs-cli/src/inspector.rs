//! Engine-side Chrome DevTools Protocol inspection for the `qjs` CLI.
//!
//! The `Runtime` domain's inspection methods (property listing, object
//! previews, function invocation) need live JavaScript values, so they run on
//! the runtime-owning REPL task through the engine request channel. This
//! module keeps the objectId registry and the rooted intrinsic-function
//! handles used to inspect values through the engine's own builtins — the
//! same technique V8's InjectedScript uses.

use std::collections::HashMap;

use quickjs::{ScriptEvaluationError, ScriptLimits, evaluate_script};
use quickjs_runtime::{
    CallError, Context, ExecutionError, ExecutionLimits, Function, GlobalScriptError, JsNumber,
    JsString, JsValue, ValueKind,
};
use serde_json::{Map, Value, json};

use crate::cdp::{protocol_error, protocol_result, source_position};

/// Engine-side inspection state owned by the runtime task for one session.
pub(crate) struct InspectState {
    pub(crate) objects: ObjectRegistry,
    next_exception_id: u64,
}

impl InspectState {
    pub(crate) fn new() -> Self {
        Self {
            objects: ObjectRegistry::new(),
            next_exception_id: 1,
        }
    }

    pub(crate) fn next_exception_id(&mut self) -> u64 {
        let id = self.next_exception_id;
        self.next_exception_id += 1;
        id
    }
}

/// Registry mapping CDP objectIds to rooted live JavaScript values.
///
/// Each entry holds a public root, so a registered object stays alive until
/// its objectId is released or the session ends. Repeated registration of the
/// same identity reuses the existing objectId.
pub(crate) struct ObjectRegistry {
    next_id: u64,
    entries: HashMap<u64, RegistryEntry>,
}

struct RegistryEntry {
    value: JsValue,
    group: Option<String>,
}

impl ObjectRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            entries: HashMap::new(),
        }
    }

    pub(crate) fn register(&mut self, value: &JsValue, group: Option<&str>) -> String {
        if let Some(id) = self.find(value) {
            return object_id(id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(
            id,
            RegistryEntry {
                value: value.clone(),
                group: group.map(str::to_owned),
            },
        );
        object_id(id)
    }

    pub(crate) fn get(&self, object_id: &str) -> Option<&JsValue> {
        let id = parse_object_id(object_id)?;
        self.entries.get(&id).map(|entry| &entry.value)
    }

    pub(crate) fn release(&mut self, object_id: &str) -> bool {
        let Some(id) = parse_object_id(object_id) else {
            return false;
        };
        self.entries.remove(&id).is_some()
    }

    pub(crate) fn release_group(&mut self, group: &str) {
        self.entries
            .retain(|_, entry| entry.group.as_deref() != Some(group));
    }

    fn find(&self, value: &JsValue) -> Option<u64> {
        self.entries
            .iter()
            .find(|(_, entry)| same_identity(value, &entry.value))
            .map(|(id, _)| *id)
    }
}

fn object_id(id: u64) -> String {
    format!("qjs:{id}")
}

fn parse_object_id(object_id: &str) -> Option<u64> {
    object_id.strip_prefix("qjs:")?.parse().ok()
}

fn same_identity(left: &JsValue, right: &JsValue) -> bool {
    match (left.kind(), right.kind()) {
        (Ok(ValueKind::Object), Ok(ValueKind::Object)) => left
            .clone()
            .into_object()
            .ok()
            .zip(right.clone().into_object().ok())
            .is_some_and(|(left, right)| left.same_identity(&right).unwrap_or(false)),
        (Ok(ValueKind::Function), Ok(ValueKind::Function)) => left
            .clone()
            .into_function()
            .ok()
            .zip(right.clone().into_function().ok())
            .is_some_and(|(left, right)| left.same_identity(&right).unwrap_or(false)),
        _ => false,
    }
}

/// Rooted intrinsic functions used to inspect values through the engine's
/// own builtins.
pub(crate) struct InspectIntrinsics {
    get_own_property_names: Function,
    get_own_property_symbols: Function,
    get_own_property_descriptor: Function,
    get_prototype_of: Function,
    object_keys: Function,
    object_to_string: Function,
    reflect_get: Function,
    json_stringify: Function,
    symbol_replacer: Function,
    symbol_by_name: Function,
    global_object: JsValue,
    /// The global object's own string-named bindings, rooted, used to label
    /// namespace objects the way V8 labels them (`Reflect`, `Math`, ...).
    global_bindings: Vec<(String, JsValue)>,
}

/// Failure while collecting the inspection intrinsics.
#[derive(Debug)]
pub(crate) enum InspectSetupError {
    Evaluate(ScriptEvaluationError),
    NotAFunction(&'static str),
}

impl std::fmt::Display for InspectSetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Evaluate(error) => error.fmt(formatter),
            Self::NotAFunction(source) => {
                write!(
                    formatter,
                    "intrinsic did not evaluate to a function: {source}"
                )
            }
        }
    }
}

impl InspectIntrinsics {
    pub(crate) fn new(context: &mut Context<'_>) -> Result<Self, InspectSetupError> {
        let intrinsic = |context: &mut Context<'_>, source: &'static str| {
            evaluate_script(context, source, "<cdp-intrinsic>", ScriptLimits::default())
                .map_err(InspectSetupError::Evaluate)?
                .into_function()
                .map_err(|_| InspectSetupError::NotAFunction(source))
        };
        let global_object = evaluate_script(
            context,
            "globalThis",
            "<cdp-intrinsic>",
            ScriptLimits::default(),
        )
        .map_err(InspectSetupError::Evaluate)?;
        let mut intrinsics = Self {
            get_own_property_names: intrinsic(context, "Object.getOwnPropertyNames")?,
            get_own_property_symbols: intrinsic(context, "Object.getOwnPropertySymbols")?,
            get_own_property_descriptor: intrinsic(context, "Object.getOwnPropertyDescriptor")?,
            get_prototype_of: intrinsic(context, "Object.getPrototypeOf")?,
            object_keys: intrinsic(context, "Object.keys")?,
            object_to_string: intrinsic(context, "Object.prototype.toString")?,
            reflect_get: intrinsic(context, "Reflect.get")?,
            json_stringify: intrinsic(context, "JSON.stringify")?,
            symbol_replacer: intrinsic(
                context,
                "(key, value) => typeof value === 'symbol' ? String(value) : value",
            )?,
            symbol_by_name: intrinsic(
                context,
                "(object, name) => Object.getOwnPropertySymbols(object).find((symbol) => String(symbol) === name)",
            )?,
            global_object,
            global_bindings: Vec::new(),
        };
        let global = intrinsics.global_object.clone();
        let names = intrinsics
            .own_string_keys(context, &global)
            .unwrap_or_default();
        for name in names {
            if let Some(value) = reflect_value(context, &intrinsics, &global, &name) {
                intrinsics.global_bindings.push((name, value));
            }
        }
        Ok(intrinsics)
    }

    /// Returns the global binding name this object is installed under, when
    /// the object is one of the global object's own property values.
    pub(crate) fn global_binding_name(&self, value: &JsValue) -> Option<&str> {
        if value.kind() != Ok(ValueKind::Object) {
            return None;
        }
        let Ok(object) = value.clone().into_object() else {
            return None;
        };
        self.global_bindings
            .iter()
            .find(|(_, binding)| {
                binding
                    .clone()
                    .into_object()
                    .ok()
                    .is_some_and(|candidate| object.same_identity(&candidate).unwrap_or(false))
            })
            .map(|(name, _)| name.as_str())
    }

    /// Returns the object's own symbol keys rendered as `Symbol(description)`.
    ///
    /// # Errors
    ///
    /// Returns the engine call failure; a Proxy `ownKeys` trap may throw.
    pub(crate) fn symbol_keys(
        &self,
        context: &mut Context<'_>,
        value: &JsValue,
    ) -> Result<Vec<String>, CallError> {
        let undefined = context.undefined_value();
        let symbols = call(
            context,
            &self.get_own_property_symbols,
            undefined.clone(),
            vec![value.clone()],
        )?;
        let json = call(
            context,
            &self.json_stringify,
            undefined,
            vec![symbols, self.symbol_replacer.as_value()],
        )?;
        let text = string_value(&json).unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    /// Returns the object's own symbol whose `String` rendering matches
    /// `name`, or `undefined` when no such symbol exists.
    ///
    /// # Errors
    ///
    /// Returns the engine call failure.
    pub(crate) fn symbol_by_name(
        &self,
        context: &mut Context<'_>,
        value: &JsValue,
        name: &str,
    ) -> Result<JsValue, CallError> {
        let undefined = context.undefined_value();
        let Ok(name) = JsString::from_utf8(name) else {
            return Ok(undefined);
        };
        let name = context.string(name);
        call(
            context,
            &self.symbol_by_name,
            undefined,
            vec![value.clone(), name],
        )
    }

    /// Returns the object's own string keys through
    /// `Object.getOwnPropertyNames`.
    ///
    /// # Errors
    ///
    /// Returns the engine call failure; enumeration itself never throws for
    /// ordinary values, but a Proxy `ownKeys` trap may.
    pub(crate) fn own_string_keys(
        &self,
        context: &mut Context<'_>,
        value: &JsValue,
    ) -> Result<Vec<String>, CallError> {
        self.string_keys(context, &self.get_own_property_names, value)
    }

    /// Returns the object's own enumerable string keys through `Object.keys`.
    ///
    /// # Errors
    ///
    /// Returns the engine call failure; a Proxy `ownKeys` trap may throw.
    pub(crate) fn enumerable_string_keys(
        &self,
        context: &mut Context<'_>,
        value: &JsValue,
    ) -> Result<Vec<String>, CallError> {
        self.string_keys(context, &self.object_keys, value)
    }

    fn string_keys(
        &self,
        context: &mut Context<'_>,
        function: &Function,
        value: &JsValue,
    ) -> Result<Vec<String>, CallError> {
        let undefined = context.undefined_value();
        let names = call(context, function, undefined.clone(), vec![value.clone()])?;
        let json = call(context, &self.json_stringify, undefined, vec![names])?;
        let text = string_value(&json).unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    /// Returns the `Object.prototype.toString` class tag, lowercased.
    ///
    /// # Errors
    ///
    /// Returns the engine call failure; a `Symbol.toStringTag` getter may
    /// throw.
    pub(crate) fn class_tag(
        &self,
        context: &mut Context<'_>,
        value: &JsValue,
    ) -> Result<String, CallError> {
        let rendered = call(context, &self.object_to_string, value.clone(), vec![])?;
        let text = string_value(&rendered).unwrap_or_default();
        Ok(text
            .strip_prefix("[object ")
            .and_then(|tag| tag.strip_suffix(']'))
            .unwrap_or("object")
            .to_ascii_lowercase())
    }
}

/// Maps an engine `Object.prototype.toString` class tag to the CDP
/// `RemoteObject` subtype, collapsing the concrete typed-array tags into
/// `"typedarray"` the way V8 reports them.
fn remote_object_subtype(tag: &str) -> Option<&'static str> {
    const TYPED_ARRAYS: [&str; 12] = [
        "int8array",
        "uint8array",
        "uint8clampedarray",
        "int16array",
        "uint16array",
        "int32array",
        "uint32array",
        "float16array",
        "float32array",
        "float64array",
        "bigint64array",
        "biguint64array",
    ];
    if TYPED_ARRAYS.contains(&tag) {
        return Some("typedarray");
    }
    Some(match tag {
        "object" => return None,
        "array" => "array",
        "arraybuffer" => "arraybuffer",
        "dataview" => "dataview",
        "date" => "date",
        "error" => "error",
        "generator" => "generator",
        "iterator" => "iterator",
        "map" => "map",
        "promise" => "promise",
        "proxy" => "proxy",
        "regexp" => "regexp",
        "set" => "set",
        "sharedarraybuffer" => "sharedarraybuffer",
        "weakmap" => "weakmap",
        "weakset" => "weakset",
        _ => return None,
    })
}

/// Maps an engine class tag to the V8-style capitalized label used in
/// `RemoteObject` descriptions.
fn tag_label(tag: &str) -> String {
    let known = match tag {
        "object" => "Object",
        "array" => "Array",
        "arraybuffer" => "ArrayBuffer",
        "arguments" => "Arguments",
        "bigint64array" => "BigInt64Array",
        "biguint64array" => "BigUint64Array",
        "dataview" => "DataView",
        "date" => "Date",
        "error" => "Error",
        "float16array" => "Float16Array",
        "float32array" => "Float32Array",
        "float64array" => "Float64Array",
        "function" => "Function",
        "generator" => "Generator",
        "int8array" => "Int8Array",
        "int16array" => "Int16Array",
        "int32array" => "Int32Array",
        "iterator" => "Iterator",
        "map" => "Map",
        "promise" => "Promise",
        "proxy" => "Proxy",
        "regexp" => "RegExp",
        "set" => "Set",
        "sharedarraybuffer" => "SharedArrayBuffer",
        "uint8array" => "Uint8Array",
        "uint8clampedarray" => "Uint8ClampedArray",
        "uint16array" => "Uint16Array",
        "uint32array" => "Uint32Array",
        "weakmap" => "WeakMap",
        "weakset" => "WeakSet",
        _ => {
            return {
                let mut chars = tag.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            };
        }
    };
    known.to_owned()
}

/// The V8-style class identity of one live object: the capitalized label
/// used as the `RemoteObject` description and the CDP subtype.
struct ObjectClass {
    label: String,
    subtype: Option<&'static str>,
}

fn object_class(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
) -> ObjectClass {
    if context.object_is_proxy(value).unwrap_or(false) {
        return ObjectClass {
            label: "Proxy".to_owned(),
            subtype: Some("proxy"),
        };
    }
    let tag = intrinsics
        .class_tag(context, value)
        .unwrap_or_else(|_| "object".to_owned());
    let subtype = remote_object_subtype(&tag);
    let mut label = tag_label(&tag);
    if subtype == Some("array") || subtype == Some("typedarray") {
        if let Some(length) = reflect_number(context, intrinsics, value, "length") {
            label = format!("{label}({length})");
        }
    } else if subtype.is_none()
        && let Some(binding) = intrinsics.global_binding_name(value)
    {
        label = binding.to_owned();
    }
    ObjectClass { label, subtype }
}

/// Renders one `Runtime.consoleAPICalled` event for host `print` output, so
/// printed values also appear in the DevTools console.
pub(crate) fn console_api_event(
    context: &mut Context<'_>,
    registry: &mut ObjectRegistry,
    intrinsics: &InspectIntrinsics,
    arguments: &[JsValue],
) -> Value {
    let args = arguments
        .iter()
        .map(|argument| {
            remote_object(
                context,
                registry,
                intrinsics,
                argument,
                Some("console"),
                false,
            )
        })
        .collect::<Vec<_>>();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    json!({
        "method": "Runtime.consoleAPICalled",
        "params": {
            "type": "log",
            "args": args,
            "executionContextId": 1,
            "timestamp": timestamp,
            "stackTrace": null,
        }
    })
}

/// Renders one live value as a CDP `RemoteObject`.
///
/// Object and function results receive a registered `objectId`, and object
/// results carry their class tag as the description.
pub(crate) fn remote_object(
    context: &mut Context<'_>,
    registry: &mut ObjectRegistry,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    group: Option<&str>,
    generate_preview: bool,
) -> Value {
    let Ok(kind) = value.kind() else {
        return json!({"type": "undefined"});
    };
    match kind {
        ValueKind::Undefined => json!({"type": "undefined"}),
        ValueKind::Null => json!({"type": "object", "subtype": "null", "value": null}),
        ValueKind::Boolean => {
            json!({"type": "boolean", "value": value.as_boolean().ok().flatten()})
        }
        ValueKind::Number => match value.as_number().ok().flatten() {
            Some(number) => {
                let number = number.as_f64();
                if number.is_finite() {
                    json!({"type": "number", "value": number})
                } else if number.is_nan() {
                    json!({"type": "number", "unserializableValue": "NaN", "description": "NaN"})
                } else if number > 0.0 {
                    json!({"type": "number", "unserializableValue": "Infinity", "description": "Infinity"})
                } else {
                    json!({"type": "number", "unserializableValue": "-Infinity", "description": "-Infinity"})
                }
            }
            None => json!({"type": "number", "description": "[number]"}),
        },
        ValueKind::String => json!({
            "type": "string",
            "value": string_value(value),
        }),
        ValueKind::BigInt => json!({"type": "bigint", "description": "[bigint]"}),
        ValueKind::Symbol => json!({
            "type": "symbol",
            "description": value
                .as_symbol()
                .ok()
                .flatten()
                .and_then(|atom| atom.description())
                .and_then(|description| description.to_utf8_lossy().ok())
                .map(|description| format!("Symbol({description})"))
                .unwrap_or_else(|| "[symbol]".to_owned()),
        }),
        ValueKind::Function => {
            let name = reflect_string(context, intrinsics, value, "name")
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "anonymous".to_owned());
            let object_id = registry.register(value, group);
            json!({
                "type": "function",
                "description": format!("\u{0192} {name}()"),
                "objectId": object_id,
            })
        }
        ValueKind::Object => {
            let class = object_class(context, intrinsics, value);
            let mut rendered = json!({
                "type": "object",
                "description": class.label,
            });
            if let Some(subtype) = class.subtype {
                rendered["subtype"] = Value::String(subtype.to_owned());
            }
            if generate_preview && let Some(preview) = object_preview(context, intrinsics, value, 0)
            {
                rendered["preview"] = preview;
            }
            rendered["objectId"] = Value::String(registry.register(value, group));
            rendered
        }
    }
}

/// Dispatches one engine-bound CDP request on the runtime-owning task.
pub(crate) fn handle_engine_request(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    message: Value,
) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "Runtime.evaluate" => evaluate_request(context, state, intrinsics, id, &params),
        "Runtime.getProperties" => get_properties_request(context, state, intrinsics, id, &params),
        "Runtime.callFunctionOn" => {
            call_function_on_request(context, state, intrinsics, id, &params)
        }
        "Runtime.releaseObject" => release_object_request(state, id, &params),
        "Runtime.releaseObjectGroup" => release_object_group_request(state, id, &params),
        "Runtime.globalLexicalScopeNames" => {
            global_lexical_scope_names_request(context, intrinsics, id)
        }
        "Runtime.getIsolateId" => protocol_result(id, json!({"id": "quickjs"})),
        "Runtime.getHeapUsage" => {
            let usage = context.runtime_usage();
            let used_size = usage.heap_objects()
                + usage.heap_functions()
                + usage.object_properties()
                + usage.array_buffer_bytes();
            protocol_result(id, json!({"usedSize": used_size, "totalSize": used_size}))
        }
        _ => protocol_error(id, -32601, &format!("unsupported CDP method: {method}")),
    }
}

fn call_function_on_request(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    id: Value,
    params: &Value,
) -> Value {
    let receiver = match params.get("objectId").and_then(Value::as_str) {
        Some(object_id) => match state.objects.get(object_id).cloned() {
            Some(object) => object,
            None => return protocol_error(id, -32000, "unknown objectId"),
        },
        None => {
            if params.get("executionContextId").is_some() {
                intrinsics.global_object.clone()
            } else {
                return protocol_error(
                    id,
                    -32602,
                    "Runtime.callFunctionOn requires params.objectId or params.executionContextId",
                );
            }
        }
    };
    let Some(declaration) = params.get("functionDeclaration").and_then(Value::as_str) else {
        return protocol_error(
            id,
            -32602,
            "Runtime.callFunctionOn requires params.functionDeclaration",
        );
    };
    let function_source = format!("({declaration})");
    let function = match evaluate_script(
        context,
        &function_source,
        "<cdp-function>",
        ScriptLimits::default(),
    ) {
        Ok(value) => match value.into_function() {
            Ok(function) => function,
            Err(_) => {
                return protocol_error(
                    id,
                    -32602,
                    "functionDeclaration did not evaluate to a function",
                );
            }
        },
        Err(error) => {
            let (text, line, column) =
                script_error_position(context, intrinsics, &error, &function_source);
            return protocol_result(
                id,
                json!({
                    "result": {"type": "object", "subtype": "error", "description": text},
                    "exceptionDetails": exception_details(state, &text, line, column, "<cdp-function>"),
                }),
            );
        }
    };
    let mut arguments = Vec::new();
    if let Some(list) = params.get("arguments").and_then(Value::as_array) {
        for argument in list {
            match call_argument(context, state, argument) {
                Some(value) => arguments.push(value),
                None => {
                    return protocol_error(id, -32602, "unsupported callFunctionOn argument");
                }
            }
        }
    }
    let return_by_value = params
        .get("returnByValue")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let generate_preview = params
        .get("generatePreview")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match call(context, &function, receiver, arguments) {
        Ok(value) => {
            let mut result = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &value,
                None,
                generate_preview,
            );
            if return_by_value
                && let Some(serialized) = serialize_value(context, intrinsics, &value, 0)
            {
                result["value"] = serialized;
            }
            protocol_result(id, json!({"result": result}))
        }
        Err(CallError::Thrown(exception)) => {
            let text = thrown_value_text(context, intrinsics, &exception)
                .unwrap_or_else(|| "[exception]".to_owned());
            protocol_result(
                id,
                json!({
                    "result": {"type": "object", "subtype": "error", "description": text},
                    "exceptionDetails": exception_details(state, &text, 0, 0, "<cdp-function>"),
                }),
            )
        }
        Err(error) => protocol_error(id, -32000, &format!("function call failed: {error}")),
    }
}

/// Converts one CDP `CallArgument` into a live value; `None` means the
/// argument shape is unsupported.
fn call_argument(context: &Context<'_>, state: &InspectState, argument: &Value) -> Option<JsValue> {
    if let Some(object_id) = argument.get("objectId").and_then(Value::as_str) {
        return state.objects.get(object_id).cloned();
    }
    if let Some(unserializable) = argument.get("unserializableValue").and_then(Value::as_str) {
        return Some(context.number(JsNumber::from_f64(match unserializable {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            "-0" => -0.0,
            _ => return None,
        })));
    }
    let value = argument.get("value")?;
    Some(match value {
        Value::Null => context.null(),
        Value::Bool(value) => context.boolean(*value),
        Value::Number(value) => context.number(JsNumber::from_f64(value.as_f64()?)),
        Value::String(value) => context.string(JsString::from_utf8(value).ok()?),
        _ => return None,
    })
}

fn release_object_request(state: &mut InspectState, id: Value, params: &Value) -> Value {
    let Some(object_id) = params.get("objectId").and_then(Value::as_str) else {
        return protocol_error(id, -32602, "Runtime.releaseObject requires params.objectId");
    };
    if state.objects.release(object_id) {
        protocol_result(id, json!({}))
    } else {
        protocol_error(id, -32000, "unknown objectId")
    }
}

fn release_object_group_request(state: &mut InspectState, id: Value, params: &Value) -> Value {
    let Some(group) = params.get("objectGroup").and_then(Value::as_str) else {
        return protocol_error(
            id,
            -32602,
            "Runtime.releaseObjectGroup requires params.objectGroup",
        );
    };
    state.objects.release_group(group);
    protocol_result(id, json!({}))
}

fn global_lexical_scope_names_request(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    id: Value,
) -> Value {
    let names = intrinsics
        .own_string_keys(context, &intrinsics.global_object)
        .unwrap_or_default();
    protocol_result(id, json!({"names": names}))
}

fn get_properties_request(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    id: Value,
    params: &Value,
) -> Value {
    let Some(object_id) = params.get("objectId").and_then(Value::as_str) else {
        return protocol_error(id, -32602, "Runtime.getProperties requires params.objectId");
    };
    let Some(object) = state.objects.get(object_id).cloned() else {
        return protocol_error(id, -32000, "unknown objectId");
    };
    let own_only = params
        .get("ownProperties")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let accessors_only = params
        .get("accessorPropertiesOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let generate_preview = params
        .get("generatePreview")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let non_enumerable = params
        .get("nonEnumerableProperties")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut result = Vec::new();
    for name in intrinsics
        .own_string_keys(context, &object)
        .unwrap_or_default()
    {
        if let Ok(key) = JsString::from_utf8(&name) {
            if let Some(entry) = descriptor_entry(
                context,
                state,
                intrinsics,
                &object,
                &context.string(key),
                &name,
                accessors_only,
                non_enumerable,
                generate_preview,
            ) {
                result.push(entry);
            }
        }
    }
    for name in intrinsics.symbol_keys(context, &object).unwrap_or_default() {
        if let Ok(symbol) = intrinsics.symbol_by_name(context, &object, &name)
            && symbol.kind() == Ok(ValueKind::Symbol)
            && let Some(entry) = descriptor_entry(
                context,
                state,
                intrinsics,
                &object,
                &symbol,
                &name,
                accessors_only,
                non_enumerable,
                generate_preview,
            )
        {
            result.push(entry);
        }
    }
    let mut internal = Vec::new();
    if !accessors_only {
        let undefined = context.undefined_value();
        if let Ok(prototype) = call(
            context,
            &intrinsics.get_prototype_of,
            undefined,
            vec![object.clone()],
        ) && prototype.kind() == Ok(ValueKind::Object)
        {
            if !own_only {
                result.push(json!({
                    "name": "__proto__",
                    "value": remote_object(context, &mut state.objects, intrinsics, &prototype, None, generate_preview),
                    "configurable": true,
                    "enumerable": true,
                    "writable": true,
                    "isOwn": true,
                }));
            }
            // `ownProperties` restricts only the result list; the frontend
            // walks the chain by expanding [[Prototype]] from here.
            internal.push(json!({
                "name": "[[Prototype]]",
                "value": remote_object(context, &mut state.objects, intrinsics, &prototype, None, false),
            }));
        }
    }
    protocol_result(
        id,
        json!({"result": result, "internalProperties": internal}),
    )
}

/// Builds one `PropertyDescriptor` entry for an own property of `object`,
/// reading the descriptor through `Object.getOwnPropertyDescriptor`.
///
/// Returns `None` for properties the requested filters exclude.
#[allow(
    clippy::too_many_arguments,
    reason = "the CDP flags describe one descriptor request and stay grouped"
)]
fn descriptor_entry(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    object: &JsValue,
    key: &JsValue,
    name: &str,
    accessors_only: bool,
    non_enumerable: bool,
    generate_preview: bool,
) -> Option<Value> {
    let undefined = context.undefined_value();
    let descriptor = call(
        context,
        &intrinsics.get_own_property_descriptor,
        undefined.clone(),
        vec![object.clone(), key.clone()],
    )
    .ok()?;
    if descriptor.kind() != Ok(ValueKind::Object) {
        return None;
    }
    let enumerable = reflect_bool(context, intrinsics, &descriptor, "enumerable").unwrap_or(false);
    let configurable =
        reflect_bool(context, intrinsics, &descriptor, "configurable").unwrap_or(false);
    let writable = reflect_bool(context, intrinsics, &descriptor, "writable").unwrap_or(false);
    let get = reflect_value(context, intrinsics, &descriptor, "get")
        .filter(|get| get.kind() == Ok(ValueKind::Function));
    let set = reflect_value(context, intrinsics, &descriptor, "set")
        .filter(|set| set.kind() == Ok(ValueKind::Function));
    let is_accessor = get.is_some() || set.is_some();
    if !non_enumerable && !enumerable {
        return None;
    }
    let mut entry = json!({
        "name": name,
        "configurable": configurable,
        "enumerable": enumerable,
        "writable": writable,
        "isOwn": true,
    });
    if is_accessor {
        if let Some(get) = &get {
            entry["get"] = remote_object(context, &mut state.objects, intrinsics, get, None, false);
        }
        if let Some(set) = &set {
            entry["set"] = remote_object(context, &mut state.objects, intrinsics, set, None, false);
        }
        if accessors_only {
            let mut value = undefined;
            let mut was_thrown = false;
            if let Some(getter) = &get {
                match call(
                    context,
                    &getter.clone().into_function().expect("function kind"),
                    object.clone(),
                    vec![],
                ) {
                    Ok(result) => value = result,
                    Err(_) => was_thrown = true,
                }
            }
            entry["value"] = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &value,
                None,
                generate_preview,
            );
            entry["wasThrown"] = Value::Bool(was_thrown);
        }
    } else {
        if accessors_only {
            return None;
        }
        let value = reflect_value(context, intrinsics, &descriptor, "value")
            .unwrap_or_else(|| undefined.clone());
        entry["value"] = remote_object(
            context,
            &mut state.objects,
            intrinsics,
            &value,
            None,
            generate_preview,
        );
    }
    Some(entry)
}

fn reflect_bool(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    key: &str,
) -> Option<bool> {
    reflect_value(context, intrinsics, value, key)?
        .as_boolean()
        .ok()
        .flatten()
}

fn evaluate_request(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    id: Value,
    params: &Value,
) -> Value {
    let Some(expression) = params.get("expression").and_then(Value::as_str) else {
        return protocol_error(id, -32602, "Runtime.evaluate requires params.expression");
    };
    let source_name = params
        .get("sourceURL")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        // One stable name keeps the debugger's script registry and the
        // frontend's script list from churning on every console evaluation.
        .unwrap_or_else(|| "console".to_owned());
    let return_by_value = params
        .get("returnByValue")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let generate_preview = params
        .get("generatePreview")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let group = params.get("objectGroup").and_then(Value::as_str);
    match evaluate_script(context, expression, &source_name, ScriptLimits::default()) {
        Ok(value) => {
            let mut result = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &value,
                group,
                generate_preview,
            );
            if return_by_value
                && let Some(serialized) = serialize_value(context, intrinsics, &value, 0)
            {
                result["value"] = serialized;
            }
            protocol_result(id, json!({"result": result}))
        }
        Err(error) => {
            let (text, line, column) =
                script_error_position(context, intrinsics, &error, expression);
            protocol_result(
                id,
                json!({
                    "result": {"type": "object", "subtype": "error", "description": text},
                    "exceptionDetails": exception_details(state, &text, line, column, &source_name),
                }),
            )
        }
    }
}

/// Renders a shallow `ObjectPreview` for one live object: up to five own
/// enumerable properties with short primitive renderings and one nested level
/// of object previews.
fn object_preview(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    depth: usize,
) -> Option<Value> {
    if depth > 1 || value.kind() != Ok(ValueKind::Object) {
        return None;
    }
    let class = object_class(context, intrinsics, value);
    let keys = intrinsics
        .enumerable_string_keys(context, value)
        .unwrap_or_default();
    const CAP: usize = 5;
    let overflow = keys.len() > CAP;
    let properties = keys
        .iter()
        .take(CAP)
        .filter_map(|key| property_preview(context, intrinsics, value, key, depth))
        .collect::<Vec<_>>();
    let mut preview = json!({
        "type": "object",
        "description": class.label,
        "overflow": overflow,
        "properties": properties,
    });
    if let Some(subtype) = class.subtype {
        preview["subtype"] = Value::String(subtype.to_owned());
    }
    Some(preview)
}

fn property_preview(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    object: &JsValue,
    key: &str,
    depth: usize,
) -> Option<Value> {
    let undefined = context.undefined_value();
    let key_value = context.string(JsString::from_utf8(key).ok()?);
    let descriptor = call(
        context,
        &intrinsics.get_own_property_descriptor,
        undefined.clone(),
        vec![object.clone(), key_value],
    )
    .ok()?;
    let value = if descriptor.kind() == Ok(ValueKind::Object) {
        if let Some(get) = reflect_value(context, intrinsics, &descriptor, "get")
            && get.kind() == Ok(ValueKind::Function)
        {
            return Some(json!({"name": key, "type": "accessor"}));
        }
        reflect_value(context, intrinsics, &descriptor, "value")?
    } else {
        return None;
    };
    let kind = value.kind().ok()?;
    let mut entry = match kind {
        ValueKind::Undefined => json!({"name": key, "type": "undefined"}),
        ValueKind::Null => {
            json!({"name": key, "type": "object", "subtype": "null", "value": "null"})
        }
        ValueKind::Boolean => json!({
            "name": key,
            "type": "boolean",
            "value": value.as_boolean().ok().flatten().map(|v| v.to_string()),
        }),
        ValueKind::Number => json!({
            "name": key,
            "type": "number",
            "value": short_number(&value),
        }),
        ValueKind::String => json!({
            "name": key,
            "type": "string",
            "value": short_string(&value),
        }),
        ValueKind::BigInt => json!({"name": key, "type": "bigint", "value": "[bigint]"}),
        ValueKind::Symbol => json!({
            "name": key,
            "type": "symbol",
            "value": value
                .as_symbol()
                .ok()
                .flatten()
                .and_then(|atom| atom.description())
                .and_then(|description| description.to_utf8_lossy().ok())
                .map(|description| format!("Symbol({description})"))
                .unwrap_or_else(|| "[symbol]".to_owned()),
        }),
        ValueKind::Function => json!({
            "name": key,
            "type": "function",
            "value": reflect_string(context, intrinsics, &value, "name")
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "anonymous".to_owned()),
        }),
        ValueKind::Object => {
            let class = object_class(context, intrinsics, &value);
            let mut nested = json!({
                "name": key,
                "type": "object",
                "value": class.label,
            });
            if depth == 0
                && let Some(preview) = object_preview(context, intrinsics, &value, depth + 1)
            {
                nested["valuePreview"] = preview;
            }
            if let Some(subtype) = class.subtype {
                nested["subtype"] = Value::String(subtype.to_owned());
            }
            nested
        }
    };
    Some(entry)
}

/// Deep-serializes one live value into a JSON value for `returnByValue`.
///
/// Own enumerable properties and array indices are read (accessor getters
/// execute), bounded by depth and element caps; cyclic or over-deep values
/// return `None` so the caller keeps the `RemoteObject` shape instead.
fn serialize_value(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    depth: usize,
) -> Option<Value> {
    const MAX_DEPTH: usize = 4;
    const MAX_ELEMENTS: u64 = 1000;
    let Ok(kind) = value.kind() else {
        return None;
    };
    if depth > MAX_DEPTH {
        return None;
    }
    match kind {
        ValueKind::Undefined => None,
        ValueKind::Null => Some(Value::Null),
        ValueKind::Boolean => value.as_boolean().ok().flatten().map(Value::Bool),
        ValueKind::Number => value
            .as_number()
            .ok()
            .flatten()
            .map(|number| number.as_f64())
            .filter(|number| number.is_finite())
            .map(Value::from),
        ValueKind::String => value
            .as_string()
            .ok()
            .flatten()
            .and_then(|string| string.to_utf8_lossy().ok())
            .map(Value::String),
        ValueKind::BigInt | ValueKind::Symbol | ValueKind::Function => None,
        ValueKind::Object => {
            let tag = intrinsics
                .class_tag(context, value)
                .unwrap_or_else(|_| "object".to_owned());
            if tag == "array" {
                let length =
                    reflect_number(context, intrinsics, value, "length")?.min(MAX_ELEMENTS);
                let mut elements = Vec::with_capacity(length as usize);
                for index in 0..length {
                    let element = reflect_value(context, intrinsics, value, &index.to_string())
                        .and_then(|element| {
                            serialize_value(context, intrinsics, &element, depth + 1)
                        })
                        .unwrap_or(Value::Null);
                    elements.push(element);
                }
                return Some(Value::Array(elements));
            }
            let keys = intrinsics
                .enumerable_string_keys(context, value)
                .unwrap_or_default();
            let mut object = Map::new();
            for key in keys {
                let Some(entry) = reflect_value(context, intrinsics, value, &key)
                    .and_then(|entry| serialize_value(context, intrinsics, &entry, depth + 1))
                else {
                    continue;
                };
                object.insert(key, entry);
            }
            Some(Value::Object(object))
        }
    }
}

fn exception_details(
    state: &mut InspectState,
    text: &str,
    line: u64,
    column: u64,
    url: &str,
) -> Value {
    json!({
        "exceptionId": state.next_exception_id(),
        "text": text,
        "lineNumber": line,
        "columnNumber": column,
        "url": url,
        "exception": {"type": "object", "subtype": "error", "description": text},
    })
}

/// Renders a script failure as `(text, line, column)`, extracting the thrown
/// exception's own position and message when the engine retained one.
fn script_error_position(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    error: &ScriptEvaluationError,
    source: &str,
) -> (String, u64, u64) {
    match error {
        ScriptEvaluationError::Runtime(GlobalScriptError::Execution(
            ExecutionError::Exception(exception),
        )) => {
            let (line, column) = source_position(
                exception.source_text(),
                exception.source_span().start() as usize,
            );
            (exception_text(context, intrinsics, exception), line, column)
        }
        ScriptEvaluationError::Frontend(frontend) => {
            let diagnostic = frontend
                .diagnostics()
                .first()
                .map(|diagnostic| {
                    let position = diagnostic
                        .labels
                        .first()
                        .map(|label| label.span.start as usize)
                        .unwrap_or_default();
                    (diagnostic.message.clone(), position)
                })
                .unwrap_or_default();
            let (line, column) = source_position(source, diagnostic.1);
            (diagnostic.0, line, column)
        }
        other => (other.to_string(), 0, 0),
    }
}

/// Renders a thrown exception like DevTools does: `Name: message` followed by
/// the retained stack text when the engine installed one.
fn exception_text(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    exception: &quickjs_runtime::JsException,
) -> String {
    if let Some(value) = exception.thrown_value()
        && let Some(text) = thrown_value_text(context, intrinsics, value)
    {
        return text;
    }
    exception.to_string()
}

/// Renders an arbitrary thrown value: error objects become
/// `Name: message` with their stack, strings render raw, and other values
/// use the completion rendering.
fn thrown_value_text(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
) -> Option<String> {
    if value.kind() == Ok(ValueKind::Object) {
        let name = reflect_string(context, intrinsics, value, "name")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Error".to_owned());
        let message = reflect_string(context, intrinsics, value, "message").unwrap_or_default();
        let text = if message.is_empty() {
            name
        } else {
            format!("{name}: {message}")
        };
        if let Some(stack) = reflect_string(context, intrinsics, value, "stack")
            && !stack.is_empty()
        {
            return Some(format!("{text}\n{stack}"));
        }
        return Some(text);
    }
    string_value(value).or_else(|| Some(crate::format::format_value(value)))
}

/// Reads one property through `Reflect.get`, returning `None` when the
/// property is missing or the read throws.
fn reflect_value(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    key: &str,
) -> Option<JsValue> {
    let undefined = context.undefined_value();
    let key = context.string(JsString::from_utf8(key).ok()?);
    call(
        context,
        &intrinsics.reflect_get,
        undefined,
        vec![value.clone(), key],
    )
    .ok()
}

fn reflect_number(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    key: &str,
) -> Option<u64> {
    let value = reflect_value(context, intrinsics, value, key)?;
    let number = value.as_number().ok()??.as_f64();
    (number.is_finite() && number >= 0.0 && number.fract() == 0.0).then_some(number as u64)
}

fn short_number(value: &JsValue) -> String {
    crate::format::format_value(value)
}

fn short_string(value: &JsValue) -> String {
    let text = string_value(value).unwrap_or_default();
    const MAX_CHARS: usize = 100;
    if text.chars().count() > MAX_CHARS {
        let mut truncated: String = text.chars().take(MAX_CHARS).collect();
        truncated.push('…');
        return truncated;
    }
    text
}

/// Invokes one intrinsic function with the given receiver and arguments.
fn call(
    context: &mut Context<'_>,
    function: &Function,
    receiver: JsValue,
    arguments: Vec<JsValue>,
) -> Result<JsValue, CallError> {
    context.call_function(function, receiver, arguments, ExecutionLimits::default())
}

/// Reads one string property through `Reflect.get`, returning `None` for a
/// missing, non-string, or throwing property.
fn reflect_string(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    key: &str,
) -> Option<String> {
    let undefined = context.undefined_value();
    let result = call(
        context,
        &intrinsics.reflect_get,
        undefined,
        vec![
            value.clone(),
            context.string(JsString::from_utf8(key).ok()?),
        ],
    )
    .ok()?;
    string_value(&result)
}

fn string_value(value: &JsValue) -> Option<String> {
    value
        .as_string()
        .ok()
        .flatten()
        .and_then(|string| string.to_utf8_lossy().ok())
}

#[cfg(test)]
mod tests {
    use quickjs::{ScriptLimits, evaluate_script};
    use quickjs_runtime::{Runtime, RuntimeLimits};
    use serde_json::Value;

    use super::*;

    fn with_engine<T>(
        run: impl FnOnce(&mut Context<'_>, &mut InspectState, &InspectIntrinsics) -> T,
    ) -> T {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let mut state = InspectState::new();
        let intrinsics = InspectIntrinsics::new(&mut context).expect("intrinsics");
        run(&mut context, &mut state, &intrinsics)
    }

    fn evaluate(context: &mut Context<'_>, source: &str) -> JsValue {
        evaluate_script(
            context,
            source,
            "inspector-test.js",
            ScriptLimits::default(),
        )
        .expect("test script evaluates")
    }

    #[test]
    fn registry_roots_and_deduplicates_object_identity() {
        with_engine(|context, state, _| {
            let value = evaluate(context, "({ marker: 1 })");
            let first = state.objects.register(&value, Some("console"));
            let second = state.objects.register(&value, None);
            assert_eq!(first, second, "the same identity reuses one objectId");
            assert!(first.starts_with("qjs:"));
            assert!(state.objects.get(&first).is_some());
            state.objects.release(&first);
            assert!(state.objects.get(&first).is_none());
        });
    }

    #[test]
    fn registry_releases_object_groups() {
        with_engine(|context, state, _| {
            let value = evaluate(context, "({})");
            let id = state.objects.register(&value, Some("console"));
            state.objects.release_group("console");
            assert!(state.objects.get(&id).is_none());
        });
    }

    #[test]
    fn intrinsics_enumerate_own_property_names_as_json() {
        with_engine(|context, _, intrinsics| {
            let object = evaluate(context, "({ a: 1, b: 'x' })");
            let names = intrinsics
                .own_string_keys(context, &object)
                .expect("own property names");
            assert_eq!(names, vec!["a".to_owned(), "b".to_owned()]);
        });
    }

    #[test]
    fn intrinsics_report_well_known_object_tags() {
        with_engine(|context, _, intrinsics| {
            for (source, tag) in [
                ("({})", "object"),
                ("[]", "array"),
                ("new Date(0)", "date"),
                ("/x/", "regexp"),
                ("new Map()", "map"),
                ("new Uint8Array(2)", "uint8array"),
            ] {
                let value = evaluate(context, source);
                let rendered = intrinsics.class_tag(context, &value).expect("class tag");
                assert_eq!(rendered, tag, "class tag for {source}");
            }
        });
    }

    #[test]
    fn remote_object_subtypes_group_typed_arrays_and_buffers() {
        for (tag, subtype) in [
            ("uint8array", Some("typedarray")),
            ("float64array", Some("typedarray")),
            ("bigint64array", Some("typedarray")),
            ("arraybuffer", Some("arraybuffer")),
            ("sharedarraybuffer", Some("sharedarraybuffer")),
            ("dataview", Some("dataview")),
            ("object", None),
            ("array", Some("array")),
        ] {
            assert_eq!(remote_object_subtype(tag), subtype, "subtype for {tag}");
        }
    }

    fn protocol(
        context: &mut Context<'_>,
        state: &mut InspectState,
        intrinsics: &InspectIntrinsics,
        method: &str,
        params: Value,
    ) -> Value {
        handle_engine_request(
            context,
            state,
            intrinsics,
            serde_json::json!({"id": 7, "method": method, "params": params}),
        )
    }

    #[test]
    fn evaluate_returns_object_id_and_preview() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({a: 1, b: 'x'})", "generatePreview": true}),
            );
            let result = &response["result"]["result"];
            assert_eq!(result["type"], "object");
            assert!(
                result["objectId"].as_str().is_some(),
                "object results carry a registered objectId"
            );
            let preview = &result["preview"];
            assert_eq!(preview["type"], "object");
            assert_eq!(preview["overflow"], false);
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|property| property["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(names, vec!["a", "b"]);
            let a = &preview["properties"][0];
            assert_eq!(a["type"], "number");
            assert_eq!(a["value"], "1");
            let b = &preview["properties"][1];
            assert_eq!(b["type"], "string");
            assert_eq!(b["value"], "x");
        });
    }

    #[test]
    fn evaluate_serializes_return_by_value() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({a: 1, nested: {b: 2}})", "returnByValue": true}),
            );
            let result = &response["result"]["result"];
            assert_eq!(result["value"]["a"].as_f64(), Some(1.0));
            assert_eq!(result["value"]["nested"]["b"].as_f64(), Some(2.0));
        });
    }

    #[test]
    fn evaluate_reports_exception_details() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw new Error('boom')"}),
            );
            let result = &response["result"]["result"];
            assert_eq!(result["type"], "object");
            assert_eq!(result["subtype"], "error");
            let details = &response["result"]["exceptionDetails"];
            assert!(
                details["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("boom")),
                "exception text names the message"
            );
            assert!(details["exceptionId"].is_u64());
        });
    }

    #[test]
    fn evaluate_requires_an_expression() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({}),
            );
            assert_eq!(response["error"]["code"], -32602);
        });
    }

    #[test]
    fn get_properties_lists_data_properties_and_prototype() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ 0: 'indexed', b: 'x', a: 1 })"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id}),
            );
            let result = response["result"]["result"].as_array().expect("properties");
            let names: Vec<&str> = result
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(
                names,
                vec!["0", "b", "a", "__proto__"],
                "own keys follow [[OwnPropertyKeys]] order and the chain entry is last"
            );
            assert_eq!(result[0]["value"]["value"].as_str(), Some("indexed"));
            assert_eq!(result[1]["value"]["value"].as_str(), Some("x"));
            assert_eq!(result[2]["value"]["value"].as_f64(), Some(1.0));
            assert!(
                result[0]["isOwn"].as_bool().expect("isOwn"),
                "own properties are marked"
            );
            assert!(
                result[0]["enumerable"].as_bool().expect("enumerable"),
                "enumerable data properties are marked"
            );
            let internal = response["result"]["internalProperties"]
                .as_array()
                .expect("internal properties");
            assert_eq!(internal[0]["name"], "[[Prototype]]");
            assert!(internal[0]["value"]["objectId"].is_string());

            let proto_id = result[3]["value"]["objectId"].as_str().expect("proto id");
            let proto_response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": proto_id, "ownProperties": true, "nonEnumerableProperties": true}),
            );
            let proto_names: Vec<&str> = proto_response["result"]["result"]
                .as_array()
                .expect("proto properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert!(
                proto_names.contains(&"hasOwnProperty"),
                "non-enumerable Object.prototype methods are listed on request"
            );
        });
    }

    #[test]
    fn get_properties_executes_accessor_getters_on_request() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ get answer() { return 42; }, plain: 1 })"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id, "accessorPropertiesOnly": true}),
            );
            let result = response["result"]["result"].as_array().expect("properties");
            let names: Vec<&str> = result
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(names, vec!["answer"], "only accessors are returned");
            assert_eq!(result[0]["value"]["value"].as_f64(), Some(42.0));
            assert_eq!(result[0]["get"]["type"], "function");
            assert!(!result[0]["wasThrown"].as_bool().unwrap_or(true));
        });
    }

    #[test]
    fn get_properties_includes_symbol_keys() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ [Symbol('tag')]: 'v', x: 1 })"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id}),
            );
            let names: Vec<&str> = response["result"]["result"]
                .as_array()
                .expect("properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert!(
                names.contains(&"Symbol(tag)"),
                "symbol keys render as Symbol(description), got {names:?}"
            );
        });
    }

    #[test]
    fn remote_object_descriptions_use_v8_style_labels() {
        with_engine(|context, state, intrinsics| {
            for (source, description, subtype) in [
                ("({})", "Object", None),
                ("[1, 2]", "Array(2)", Some("array")),
                ("new Map()", "Map", Some("map")),
                ("new Proxy({}, {})", "Proxy", Some("proxy")),
                ("globalThis.Reflect", "Reflect", None),
                ("new Uint8Array(3)", "Uint8Array(3)", Some("typedarray")),
            ] {
                let value = evaluate(context, source);
                let rendered =
                    remote_object(context, &mut state.objects, intrinsics, &value, None, false);
                assert_eq!(rendered["description"], description, "label for {source}");
                match subtype {
                    Some(expected) => {
                        assert_eq!(rendered["subtype"], expected, "subtype for {source}")
                    }
                    None => assert!(rendered["subtype"].is_null(), "no subtype for {source}"),
                }
            }
        });
    }

    #[test]
    fn console_evaluations_share_one_source_name() {
        with_engine(|context, state, intrinsics| {
            let first = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw 1"}),
            );
            let second = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw 2"}),
            );
            assert_eq!(
                first["result"]["exceptionDetails"]["url"], "console",
                "console evaluations reuse one source name"
            );
            assert_eq!(second["result"]["exceptionDetails"]["url"], "console");
            let named = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw 3", "sourceURL": "custom.js"}),
            );
            assert_eq!(named["result"]["exceptionDetails"]["url"], "custom.js");
        });
    }

    #[test]
    fn console_api_event_renders_arguments() {
        with_engine(|context, state, intrinsics| {
            let first = evaluate(context, "40 + 2");
            let second = evaluate(context, "'hello'");
            let event =
                console_api_event(context, &mut state.objects, intrinsics, &[first, second]);
            assert_eq!(event["method"], "Runtime.consoleAPICalled");
            assert_eq!(event["params"]["type"], "log");
            assert_eq!(event["params"]["args"][0]["value"].as_f64(), Some(42.0));
            assert_eq!(event["params"]["args"][1]["value"], "hello");
            assert_eq!(event["params"]["executionContextId"], 1);
            assert!(event["params"]["timestamp"].is_u64());
        });
    }

    #[test]
    fn get_properties_rejects_unknown_object_ids() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": "qjs:999999"}),
            );
            assert_eq!(response["error"]["code"], -32000);
        });
    }

    #[test]
    fn get_properties_keeps_the_prototype_internal_property_with_own_properties() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({})"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id, "ownProperties": true}),
            );
            let result = response["result"]["result"].as_array().expect("properties");
            let names: Vec<&str> = result
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert!(
                !names.contains(&"__proto__"),
                "ownProperties: true omits the __proto__ result entry"
            );
            let internal = response["result"]["internalProperties"]
                .as_array()
                .expect("internal properties");
            assert_eq!(internal[0]["name"], "[[Prototype]]");
            assert!(
                internal[0]["value"]["objectId"].is_string(),
                "the [[Prototype]] objectId is what the frontend expands"
            );

            // The prototype chain walk must work from that objectId.
            let proto_id = internal[0]["value"]["objectId"].as_str().expect("proto id");
            let proto_response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": proto_id, "ownProperties": true, "nonEnumerableProperties": true}),
            );
            let proto_names: Vec<&str> = proto_response["result"]["result"]
                .as_array()
                .expect("proto properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert!(
                proto_names.contains(&"hasOwnProperty"),
                "expanding [[Prototype]] lists built-in methods"
            );
        });
    }

    #[test]
    fn get_properties_omits_internal_properties_for_accessor_only_listings() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ get x() { return 1; } })"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id, "accessorPropertiesOnly": true}),
            );
            let internal = response["result"]["internalProperties"]
                .as_array()
                .expect("internal properties");
            assert!(
                internal.is_empty(),
                "accessor-only listings omit internal properties"
            );
        });
    }

    #[test]
    fn call_function_on_invokes_a_function_on_an_object_id() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ base: 40 })"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.callFunctionOn",
                serde_json::json!({
                    "objectId": object_id,
                    "functionDeclaration": "function (extra) { return this.base + extra; }",
                    "arguments": [{"value": 2}],
                }),
            );
            assert_eq!(response["result"]["result"]["value"].as_f64(), Some(42.0));
        });
    }

    #[test]
    fn call_function_on_accepts_object_id_arguments() {
        with_engine(|context, state, intrinsics| {
            let first = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ x: 1 })"}),
            );
            let second = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ x: 2 })"}),
            );
            let first_id = first["result"]["result"]["objectId"].as_str().expect("id");
            let second_id = second["result"]["result"]["objectId"].as_str().expect("id");
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.callFunctionOn",
                serde_json::json!({
                    "executionContextId": 1,
                    "functionDeclaration": "function (a, b) { return a.x + b.x; }",
                    "arguments": [{"objectId": first_id}, {"objectId": second_id}],
                }),
            );
            assert_eq!(response["result"]["result"]["value"].as_f64(), Some(3.0));
        });
    }

    #[test]
    fn release_object_and_group_invalidate_object_ids() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({})", "objectGroup": "console"}),
            );
            let object_id = evaluated["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let released = protocol(
                context,
                state,
                intrinsics,
                "Runtime.releaseObject",
                serde_json::json!({"objectId": object_id}),
            );
            assert_eq!(released["error"], Value::Null);
            let after = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id}),
            );
            assert_eq!(after["error"]["code"], -32000);

            let grouped = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({})", "objectGroup": "console"}),
            );
            let grouped_id = grouped["result"]["result"]["objectId"]
                .as_str()
                .expect("objectId")
                .to_owned();
            let released = protocol(
                context,
                state,
                intrinsics,
                "Runtime.releaseObjectGroup",
                serde_json::json!({"objectGroup": "console"}),
            );
            assert_eq!(released["error"], Value::Null);
            let after = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": grouped_id}),
            );
            assert_eq!(after["error"]["code"], -32000);
        });
    }

    #[test]
    fn global_lexical_scope_names_lists_global_bindings() {
        with_engine(|context, state, intrinsics| {
            let _ = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "globalThis.__qjs_scope_test = 1"}),
            );
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.globalLexicalScopeNames",
                serde_json::json!({}),
            );
            let names: Vec<&str> = response["result"]["names"]
                .as_array()
                .expect("scope names")
                .iter()
                .map(|name| name.as_str().expect("name"))
                .collect();
            assert!(names.contains(&"Object"), "global builtins are listed");
            assert!(
                names.contains(&"__qjs_scope_test"),
                "installed global bindings are listed"
            );
        });
    }

    #[test]
    fn isolate_and_heap_usage_reply() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getIsolateId",
                serde_json::json!({}),
            );
            assert_eq!(response["result"]["id"], "quickjs");
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getHeapUsage",
                serde_json::json!({}),
            );
            assert!(response["result"]["usedSize"].is_u64());
        });
    }

    #[test]
    fn remote_object_assigns_object_ids_and_function_descriptions() {
        with_engine(|context, state, intrinsics| {
            let object = evaluate(context, "({})");
            let rendered = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &object,
                Some("console"),
                false,
            );
            assert_eq!(rendered["type"], "object");
            assert!(
                rendered["objectId"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("qjs:")),
                "object results carry a registered objectId"
            );

            let function = evaluate(context, "(function named() {})");
            let rendered = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &function,
                None,
                false,
            );
            assert_eq!(rendered["type"], "function");
            assert!(
                rendered["description"]
                    .as_str()
                    .is_some_and(|text| text.contains("named")),
                "function descriptions include the name"
            );

            let number = evaluate(context, "40 + 2");
            let rendered = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &number,
                None,
                false,
            );
            assert_eq!(
                rendered,
                serde_json::json!({"type": "number", "value": 42.0})
            );
        });
    }
}
