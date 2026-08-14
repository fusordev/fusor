//! Engine-side Chrome DevTools Protocol inspection: the objectId registry,
//! intrinsic-function handles, and the `Runtime` domain handlers.
//!
//! The `Runtime` domain's inspection methods (property listing, object
//! previews, function invocation) need live JavaScript values, so they run on
//! the runtime-owning REPL task through the engine request channel. This
//! module keeps the objectId registry and the rooted intrinsic-function
//! handles used to inspect values through the engine's own builtins — the
//! same technique V8's InjectedScript uses.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use fusor::{
    CompiledFunctionTree, ModuleEvaluationError, ScriptEvaluationError, ScriptLimits,
    evaluate_script,
};
use fusor_runtime::{
    CallError, Context, ExecutionError, ExecutionLimits, Function, GlobalScriptError, JsNumber,
    JsString, JsValue, PromiseInspection, ValueKind,
};
use serde_json::{Map, Value, json};

use super::cdp::{protocol_error, protocol_result, script_compile_error_position, source_position};

/// Engine-side inspection state owned by the runtime task for one session.
pub struct InspectState {
    pub objects: ObjectRegistry,
    pub(crate) script_cache: ScriptCache,
    next_exception_id: u64,
    next_vm: u64,
}

impl InspectState {
    pub fn new() -> Self {
        Self {
            objects: ObjectRegistry::new(),
            script_cache: ScriptCache::new(),
            next_exception_id: 1,
            next_vm: 0,
        }
    }

    pub(crate) fn next_exception_id(&mut self) -> u64 {
        let id = self.next_exception_id;
        self.next_exception_id += 1;
        id
    }

    pub(crate) fn next_vm(&mut self) -> u64 {
        let id = self.next_vm;
        self.next_vm += 1;
        id
    }
}

/// Bounded LRU cache of compiled console scripts.
///
/// The frontend re-evaluates identical source text constantly (eager
/// evaluation probes, completion prefixes, getter invocations), so reusing
/// the verified bytecode skips the whole parse-compile-verify pipeline.
pub(crate) struct ScriptCache {
    entries: HashMap<String, Arc<CompiledFunctionTree>>,
    order: VecDeque<String>,
    hits: u64,
}

const SCRIPT_CACHE_CAPACITY: usize = 64;

impl ScriptCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
        }
    }

    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }

    pub(crate) fn get(&mut self, source: &str) -> Option<Arc<CompiledFunctionTree>> {
        let compiled = self.entries.get(source).cloned()?;
        self.hits += 1;
        self.order.retain(|entry| entry != source);
        self.order.push_back(source.to_owned());
        Some(compiled)
    }

    pub(crate) fn insert(&mut self, source: String, compiled: CompiledFunctionTree) {
        if self.entries.contains_key(&source) {
            return;
        }
        if self.entries.len() >= SCRIPT_CACHE_CAPACITY
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(source.clone(), Arc::new(compiled));
        self.order.push_back(source);
    }
}

/// Registry mapping CDP objectIds to rooted live JavaScript values.
///
/// Each entry holds a public root, so a registered object stays alive until
/// its objectId is released or the session ends. Repeated registration of the
/// same identity reuses the existing objectId. Synthetic entries views over
/// Map/Set/WeakMap/WeakSet collections register separately and expand through
/// `getProperties` (V8's `internal#entries` handles).
pub struct ObjectRegistry {
    next_id: u64,
    entries: HashMap<u64, RegistryEntry>,
}

enum RegistryEntryValue {
    Live(JsValue),
    /// A synthetic view over one collection, re-inspected on expansion.
    CollectionEntries(JsValue),
}

struct RegistryEntry {
    value: RegistryEntryValue,
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
                value: RegistryEntryValue::Live(value.clone()),
                group: group.map(str::to_owned),
            },
        );
        object_id(id)
    }

    /// Registers one expandable entries view over a collection; the view is
    /// re-inspected on every expansion, so rows stay live.
    pub(crate) fn register_entries(&mut self, collection: &JsValue) -> String {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(
            id,
            RegistryEntry {
                value: RegistryEntryValue::CollectionEntries(collection.clone()),
                group: None,
            },
        );
        object_id(id)
    }

    pub(crate) fn get(&self, object_id: &str) -> Option<&JsValue> {
        let id = parse_object_id(object_id)?;
        match self.entries.get(&id) {
            Some(RegistryEntry {
                value: RegistryEntryValue::Live(value),
                ..
            }) => Some(value),
            _ => None,
        }
    }

    /// Returns the collection behind one registered entries view.
    pub(crate) fn get_entries(&self, object_id: &str) -> Option<JsValue> {
        let id = parse_object_id(object_id)?;
        match self.entries.get(&id) {
            Some(RegistryEntry {
                value: RegistryEntryValue::CollectionEntries(collection),
                ..
            }) => Some(collection.clone()),
            _ => None,
        }
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
            .find(|(_, entry)| match &entry.value {
                RegistryEntryValue::Live(candidate) => same_identity(value, candidate),
                RegistryEntryValue::CollectionEntries(_) => false,
            })
            .map(|(id, _)| *id)
    }
}

fn object_id(id: u64) -> String {
    format!("fusor:{id}")
}

fn parse_object_id(object_id: &str) -> Option<u64> {
    object_id.strip_prefix("fusor:")?.parse().ok()
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
pub struct InspectIntrinsics {
    get_own_property_names: Function,
    get_own_property_symbols: Function,
    get_own_property_descriptor: Function,
    get_prototype_of: Function,
    object_keys: Function,
    object_to_string: Function,
    reflect_get: Function,
    json_stringify: Function,
    function_to_string: Function,
    symbol_replacer: Function,
    symbol_by_name: Function,
    global_object: JsValue,
    /// The global object's own string-named bindings, rooted, used to label
    /// namespace objects the way V8 labels them (`Reflect`, `Math`, ...).
    global_bindings: Vec<(String, JsValue)>,
}

/// Failure while collecting the inspection intrinsics.
#[derive(Debug)]
pub enum InspectSetupError {
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
    pub fn new(context: &mut Context<'_>) -> Result<Self, InspectSetupError> {
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
            function_to_string: intrinsic(context, "Function.prototype.toString")?,
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
            .unwrap_or("Object")
            .to_owned())
    }
}

/// Reads the constructor's `name` off the prototype chain, used to label
/// class instances the way V8 labels them (`A {…}`). Returns `None` when
/// the chain carries no function constructor, the name is empty, or the
/// constructor is the default `Object`.
fn constructor_name(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
) -> Option<String> {
    let undefined = context.undefined_value();
    let prototype = call(
        context,
        &intrinsics.get_prototype_of,
        undefined,
        vec![value.clone()],
    )
    .ok()?;
    if !matches!(
        prototype.kind(),
        Ok(ValueKind::Object) | Ok(ValueKind::Function)
    ) {
        return None;
    }
    let constructor = reflect_value(context, intrinsics, &prototype, "constructor")?;
    if constructor.kind() != Ok(ValueKind::Function) {
        return None;
    }
    let name = reflect_string(context, intrinsics, &constructor, "name")?;
    (!name.is_empty() && name != "Object").then_some(name)
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
        // Primitive wrapper objects carry their payload's subtype, the way
        // V8 reports `new Number(5)` as a `number` subtype.
        "number" => "number",
        "string" => "string",
        "boolean" => "boolean",
        "symbol" => "symbol",
        "bigint" => "bigint",
        _ => return None,
    })
}

/// Maps an engine class tag to the V8-style capitalized label used in
/// `RemoteObject` descriptions.
fn tag_label(tag: &str) -> String {
    let known = match tag.to_ascii_lowercase().as_str() {
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
        // V8 drops the `Temporal.` prefix from the class label; the DevTools
        // frontend re-adds it when rendering Temporal values.
        "temporal.plaindate" => "PlainDate",
        "temporal.plaindatetime" => "PlainDateTime",
        "temporal.plaintime" => "PlainTime",
        "temporal.plainmonthday" => "PlainMonthDay",
        "temporal.plainyearmonth" => "PlainYearMonth",
        "temporal.instant" => "Instant",
        "temporal.zoneddatetime" => "ZonedDateTime",
        "temporal.duration" => "Duration",
        "temporal.calendar" => "Calendar",
        "temporal.timezone" => "TimeZone",
        _ => {
            // Preserve the engine's casing and capitalize each
            // dot-separated segment (e.g. a vendor namespaced tag).
            return tag
                .split('.')
                .map(|segment| {
                    let mut chars = segment.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(".");
        }
    };
    known.to_owned()
}

/// The V8-style class identity of one live object: the capitalized label
/// used as the `RemoteObject` description, the CDP subtype, and the
/// length-free `className`.
struct ObjectClass {
    label: String,
    class_name: String,
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
            class_name: "Proxy".to_owned(),
            subtype: Some("proxy"),
        };
    }
    let tag = intrinsics
        .class_tag(context, value)
        .unwrap_or_else(|_| "Object".to_owned());
    let subtype = remote_object_subtype(&tag.to_ascii_lowercase());
    let mut class_name = tag_label(&tag);
    let mut label = class_name.clone();
    if subtype == Some("regexp") {
        // V8 labels regexps as their literal: `/source/flags`.
        let source = reflect_string(context, intrinsics, value, "source").unwrap_or_default();
        let flags = reflect_string(context, intrinsics, value, "flags").unwrap_or_default();
        label = format!("/{source}/{flags}");
    } else if subtype == Some("array") || subtype == Some("typedarray") {
        if let Some(length) = reflect_number(context, intrinsics, value, "length") {
            label = format!("{label}({length})");
        }
    } else if matches!(subtype, Some("arraybuffer") | Some("dataview"))
        && let Some(length) = reflect_number(context, intrinsics, value, "byteLength")
    {
        label = format!("{label}({length})");
    } else if matches!(
        subtype,
        Some("map") | Some("set") | Some("weakmap") | Some("weakset")
    ) && let Some(size) = reflect_number(context, intrinsics, value, "size")
    {
        // V8 labels collections with their live entry count: `Map(2)`.
        label = format!("{label}({size})");
    } else if subtype.is_none()
        && let Some(binding) = intrinsics.global_binding_name(value)
    {
        label = binding.to_owned();
        class_name = binding.to_owned();
    } else if subtype == Some("error")
        && let Some(kind) = context.error_object_kind(value).ok().flatten()
    {
        // V8 reports the concrete error family, not the shared
        // `@@toStringTag` ("Error" for every family per ECMA-262 §20.5.1).
        let name = match kind {
            fusor_runtime::ErrorObjectKind::Error => "Error",
            fusor_runtime::ErrorObjectKind::EvalError => "EvalError",
            fusor_runtime::ErrorObjectKind::RangeError => "RangeError",
            fusor_runtime::ErrorObjectKind::ReferenceError => "ReferenceError",
            fusor_runtime::ErrorObjectKind::SyntaxError => "SyntaxError",
            fusor_runtime::ErrorObjectKind::TypeError => "TypeError",
            fusor_runtime::ErrorObjectKind::UriError => "URIError",
            fusor_runtime::ErrorObjectKind::InternalError => "InternalError",
            fusor_runtime::ErrorObjectKind::AggregateError => "AggregateError",
        };
        class_name = name.to_owned();
        label = match reflect_string(context, intrinsics, value, "message") {
            Some(message) if !message.is_empty() => format!("{name}: {message}"),
            _ => name.to_owned(),
        };
    } else if subtype.is_none()
        && let Some(constructor) = constructor_name(context, intrinsics, value)
    {
        // V8 labels class instances with their constructor's name
        // (`A {…}`), keeping the generic `Object` className.
        label = constructor;
    }
    ObjectClass {
        label,
        class_name,
        subtype,
    }
}

/// Renders one `Runtime.consoleAPICalled` event for host `print` output, so
/// printed values also appear in the DevTools console.
pub fn console_api_event(
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
    json!({
        "method": "Runtime.consoleAPICalled",
        "params": {
            "type": "log",
            "args": args,
            "executionContextId": 1,
            "timestamp": event_timestamp_ms(),
            "stackTrace": null,
        }
    })
}

/// The wall-clock timestamp CDP events carry, in milliseconds since the Unix
/// epoch.
fn event_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// One uncaught CLI evaluation failure, classified for CDP rendering.
///
/// The REPL and the module runner map their typed facade errors onto this
/// shape before emitting `Runtime.exceptionThrown`, so DevTools renders
/// terminal entries and uncaught script failures the way it renders console
/// exceptions.
#[derive(Debug)]
pub enum CliException {
    /// A parse/early-error failure (the `SyntaxError` family); the position
    /// is the first diagnostic label in UTF-16 line/column coordinates.
    Compile {
        text: String,
        line: u64,
        column: u64,
    },
    /// An escaped JavaScript exception or engine failure during execution.
    Execution(ExecutionError),
    /// Any other evaluation failure (module link, loader, limits), rendered
    /// from its message.
    Message(String),
}

impl CliException {
    /// Classifies one Global Script evaluation failure against the entry's
    /// source text, which anchors `SyntaxError` positions.
    pub fn from_script_error(error: &ScriptEvaluationError, source: &str) -> Self {
        match error {
            ScriptEvaluationError::Runtime(GlobalScriptError::Execution(execution)) => {
                Self::Execution(execution.clone())
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
                Self::Compile {
                    text: diagnostic.0,
                    line,
                    column,
                }
            }
            other => Self::Message(other.to_string()),
        }
    }

    /// Classifies one module evaluation failure against the module's source
    /// text, which anchors `SyntaxError` positions; loader and link failures
    /// render as messages.
    pub fn from_module_error(error: &ModuleEvaluationError, source: &str) -> Self {
        match error {
            ModuleEvaluationError::Execution(execution) => Self::Execution(execution.clone()),
            ModuleEvaluationError::Frontend(frontend) => {
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
                Self::Compile {
                    text: diagnostic.0,
                    line,
                    column,
                }
            }
            other => Self::Message(other.to_string()),
        }
    }
}

/// Builds one `Runtime.exceptionThrown` protocol event for an uncaught CLI
/// evaluation failure, V8-aligned: the escaped exception renders as an
/// expandable `RemoteObject` with its retained stack, `SyntaxError` entries
/// carry their diagnostic position, and every event carries a wall-clock
/// timestamp.
///
/// `intrinsics` enables full exception text rendering; callers without the
/// inspection intrinsics (the module runner) still get a complete event with
/// the engine-rendered message.
pub fn exception_thrown_event(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: Option<&InspectIntrinsics>,
    exception: &CliException,
    source: &str,
    url: &str,
) -> Value {
    let (text, line, column, stack_trace, exception_remote) = match exception {
        CliException::Compile { text, line, column } => {
            // V8 prefixes console syntax failures with `Uncaught` and sends a
            // real SyntaxError object as the exception.
            let uncaught = format!("Uncaught SyntaxError: {text}");
            let exception_remote = match intrinsics {
                Some(intrinsics) => {
                    syntax_error_remote(context, state, intrinsics, text, url, *line, *column)
                }
                None => json!({
                    "type": "object",
                    "subtype": "error",
                    "className": "SyntaxError",
                    "description": format!("SyntaxError: {text}"),
                }),
            };
            (
                uncaught,
                *line,
                *column,
                single_frame_stack_trace(url, *line, *column),
                exception_remote,
            )
        }
        CliException::Execution(error) => {
            let (text, line, column, stack_trace) =
                execution_error_position(context, intrinsics, error, source);
            let exception_remote = execution_error_remote_object(context, state, intrinsics, error);
            (text, line, column, stack_trace, exception_remote)
        }
        CliException::Message(message) => (
            format!("Uncaught {message}"),
            0,
            0,
            json!({"callFrames": []}),
            json!({"type": "object", "subtype": "error", "description": message}),
        ),
    };
    let details = exception_details(
        state,
        &text,
        line,
        column,
        url,
        stack_trace,
        exception_remote,
    );
    json!({
        "method": "Runtime.exceptionThrown",
        "params": {"timestamp": event_timestamp_ms(), "exceptionDetails": details},
    })
}

/// Renders a syntax failure as a real `SyntaxError` object the way V8
/// reports console parse errors: an expandable error `RemoteObject` with the
/// `SyntaxError` class, registered for the frontend to inspect, carrying the
/// V8 console stack (`Name: message` header plus the evaluation frame).
fn syntax_error_remote(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    message: &str,
    url: &str,
    line: u64,
    column: u64,
) -> Value {
    let stack = format!(
        "SyntaxError: {message}\n    at <anonymous> ({url}:{}:{})",
        line + 1,
        column + 1
    );
    match context.error_with_stack(fusor_runtime::ErrorObjectKind::SyntaxError, message, &stack) {
        Ok(object) => {
            // The frontend concatenates exceptionDetails.text with the
            // exception description into the title line; an empty
            // description keeps the line single (V8 behavior).
            let mut remote = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &object,
                None,
                false,
            );
            remote["description"] = Value::String(String::new());
            remote
        }
        Err(_) => json!({
            "type": "object",
            "subtype": "error",
            "className": "SyntaxError",
            "description": "",
        }),
    }
}

/// Renders one execution failure as `(text, line, column, stackTrace)` for
/// CDP `exceptionDetails`, using the retained exception's own position and
/// message when the engine kept one.
fn execution_error_position(
    context: &mut Context<'_>,
    intrinsics: Option<&InspectIntrinsics>,
    error: &ExecutionError,
    _source: &str,
) -> (String, u64, u64, Value) {
    if let ExecutionError::Exception(exception) = error {
        let (line, column) = source_position(
            exception.source_text(),
            exception.source_span().start() as usize,
        );
        let text = match intrinsics {
            Some(intrinsics) => exception_text(context, intrinsics, exception),
            None => exception.to_string(),
        };
        return (
            format!("Uncaught {text}"),
            line,
            column,
            exception_stack_trace(exception),
        );
    }
    (error.to_string(), 0, 0, json!({"callFrames": []}))
}

/// Renders the thrown exception of one execution failure as an expandable
/// `RemoteObject`. When the engine retained the actual value it renders
/// directly; escaped engine errors (interpreter-thrown `TypeError`s,
/// `ReferenceError`s, dynamic-function `SyntaxError`s) carry no rooted
/// value, so an equivalent family object is materialized through the
/// engine's canonical path with the retained frames rendered as its stack.
fn execution_error_remote_object(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: Option<&InspectIntrinsics>,
    error: &ExecutionError,
) -> Value {
    if let ExecutionError::Exception(exception) = error
        && let Some(value) = exception.thrown_value()
        && let Some(intrinsics) = intrinsics
    {
        return remote_object(context, &mut state.objects, intrinsics, value, None, false);
    }
    if let ExecutionError::Exception(exception) = error
        && let Some(intrinsics) = intrinsics
        && let Some(remote) = engine_error_remote_object(context, state, intrinsics, exception)
    {
        return remote;
    }
    json!({"type": "object", "subtype": "error", "description": error.to_string()})
}

/// Materializes one escaped engine error (no rooted thrown value) as an
/// expandable family object: `eval('@')` and `null.x` render the way V8
/// renders them, with the `Name: message` stack header and the retained
/// caller frames.
fn engine_error_remote_object(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    exception: &fusor_runtime::JsException,
) -> Option<Value> {
    let kind = exception.kind()?;
    let message = exception.message()?.to_utf8_lossy().ok()?;
    let family = match kind {
        fusor_runtime::ExceptionKind::RangeError => fusor_runtime::ErrorObjectKind::RangeError,
        fusor_runtime::ExceptionKind::ReferenceError => {
            fusor_runtime::ErrorObjectKind::ReferenceError
        }
        fusor_runtime::ExceptionKind::SyntaxError => fusor_runtime::ErrorObjectKind::SyntaxError,
        fusor_runtime::ExceptionKind::TypeError => fusor_runtime::ErrorObjectKind::TypeError,
        fusor_runtime::ExceptionKind::InternalError | fusor_runtime::ExceptionKind::UriError => {
            fusor_runtime::ErrorObjectKind::Error
        }
    };
    let mut stack = format!("{}: {message}\n", kind.name());
    for frame in exception.caller_frames() {
        let (line, column) =
            source_position(frame.source_text(), frame.source_span().start() as usize);
        stack.push_str(&format!(
            "    at <anonymous> ({}:{}:{})\n",
            frame.source_name(),
            line + 1,
            column + 1
        ));
    }
    let object = context.error_with_stack(family, &message, &stack).ok()?;
    // The frontend concatenates exceptionDetails.text with the exception
    // description into the title line; an empty description keeps the line
    // single (V8 behavior).
    let mut remote = remote_object(
        context,
        &mut state.objects,
        intrinsics,
        &object,
        None,
        false,
    );
    remote["description"] = Value::String(String::new());
    Some(remote)
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
            let boolean = value.as_boolean().ok().flatten();
            json!({"type": "boolean", "value": boolean, "description": boolean.map(|v| v.to_string())})
        }
        ValueKind::Number => match value.as_number().ok().flatten() {
            Some(number) => {
                let number = number.as_f64();
                if number.is_finite() {
                    json!({"type": "number", "value": number, "description": short_number(value)})
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
        ValueKind::String => {
            let text = string_value(value);
            json!({
                "type": "string",
                "value": text,
                "description": text,
            })
        }
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
            // V8's function descriptions come from Function.prototype.toString
            // and the frontend parses them to derive the displayed name.
            let description =
                reflect_function_source(context, intrinsics, value).unwrap_or_else(|| {
                    let name = reflect_string(context, intrinsics, value, "name")
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "anonymous".to_owned());
                    format!("\u{0192} {name}()")
                });
            let object_id = registry.register(value, group);
            json!({
                "type": "function",
                "className": "Function",
                "description": description,
                "objectId": object_id,
            })
        }
        ValueKind::Object => {
            let class = object_class(context, intrinsics, value);
            let mut rendered = json!({
                "type": "object",
                "className": class.class_name,
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
        "Runtime.getIsolateId" => protocol_result(id, json!({"id": "fusor"})),
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
    let function = {
        let limits = ScriptLimits::default();
        let compiled = match state.script_cache.get(&function_source) {
            Some(compiled) => compiled,
            None => match fusor::compile_script(&function_source, "console", limits) {
                Ok(compiled) => {
                    let compiled = Arc::new(compiled);
                    state
                        .script_cache
                        .insert(function_source.clone(), (*compiled).clone());
                    compiled
                }
                Err(error) => {
                    let (text, line, column) =
                        script_compile_error_position(&error, &function_source);
                    let uncaught = format!("Uncaught SyntaxError: {text}");
                    let exception_remote = syntax_error_remote(
                        context, state, intrinsics, &text, "console", line, column,
                    );
                    return protocol_result(
                        id,
                        json!({
                            "result": {"type": "object", "subtype": "error", "description": uncaught},
                            "exceptionDetails": exception_details(
                                state,
                                &uncaught,
                                line,
                                column,
                                "console",
                                single_frame_stack_trace("console", line, column),
                                exception_remote,
                            ),
                        }),
                    );
                }
            },
        };
        match fusor::execute_compiled_script(context, &compiled, limits) {
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
                let (text, line, column, stack_trace) =
                    script_error_position(context, intrinsics, &error, &function_source);
                let exception = exception_remote_object(context, state, intrinsics, &error);
                return protocol_result(
                    id,
                    json!({
                        "result": {"type": "object", "subtype": "error", "description": text},
                        "exceptionDetails": exception_details(
                            state, &text, line, column, "console", stack_trace, exception,
                        ),
                    }),
                );
            }
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
            let exception_object = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &exception,
                None,
                true,
            );
            protocol_result(
                id,
                json!({
                    "result": {"type": "object", "subtype": "error", "description": text},
                    "exceptionDetails": exception_details(
                        state,
                        &text,
                        0,
                        0,
                        "console",
                        json!({}),
                        exception_object,
                    ),
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
    // Synthetic entries views (V8's `internal#entries` handles) expand by
    // re-inspecting their collection: one row per live entry.
    if let Some(collection) = state.objects.get_entries(object_id) {
        return protocol_result(
            id,
            json!({"result": collection_entry_rows(context, intrinsics, &collection)}),
        );
    }
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
    // For large arrays the frontend renders index range groups itself and
    // asks for non-indexed properties only; returning the indices too would
    // duplicate every element row under the range headers.
    let non_indexed_only = params
        .get("nonIndexedPropertiesOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut result = Vec::new();
    for name in intrinsics
        .own_string_keys(context, &object)
        .unwrap_or_default()
    {
        if non_indexed_only && is_array_index_key(&name) {
            continue;
        }
        if let Ok(key) = JsString::from_utf8(&name) {
            if let Some(entry) = descriptor_entry(
                context,
                state,
                intrinsics,
                &object,
                &context.string(key),
                &name,
                accessors_only,
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
        ) && matches!(
            prototype.kind(),
            // Prototypes may be ordinary objects or callable functions.
            Ok(ValueKind::Object) | Ok(ValueKind::Function)
        ) {
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
        internal.extend(internal_slot_entries(
            context,
            state,
            intrinsics,
            &object,
            generate_preview,
        ));
    }
    if accessors_only {
        // V8 omits the key entirely; the frontend's destructuring defaults
        // handle `undefined` but iterate `null`, so an explicit null breaks
        // the whole expansion.
        return protocol_result(id, json!({"result": result}));
    }
    protocol_result(
        id,
        json!({"result": result, "internalProperties": Value::Array(internal)}),
    )
}

/// Renders the expansion rows of one registered collection entries view:
/// each live entry becomes a property row whose value is an
/// `internal#entry` object previewing the key/value pair (values only for
/// Set/WeakSet), the shape the DevTools frontend renders as `key => value`.
fn collection_entry_rows(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    collection: &JsValue,
) -> Vec<Value> {
    let Ok(Some(inspection)) = context.collection_inspection(collection) else {
        return Vec::new();
    };
    let mut row = |index: usize, key: Option<&JsValue>, value: &JsValue| {
        let mut properties = Vec::new();
        let mut description = String::new();
        if let Some(key) = key
            && let Some(entry) = preview_entry(context, intrinsics, "key", key, 0)
        {
            let rendered = entry["value"].as_str().unwrap_or_default();
            description.push_str(&format!("{{{rendered} => "));
            properties.push(entry);
        }
        if let Some(entry) = preview_entry(context, intrinsics, "value", value, 0) {
            let rendered = entry["value"].as_str().unwrap_or_default();
            if key.is_some() {
                description.push_str(&format!("{rendered}}}"));
            } else {
                // Set rows render the bare value (V8 shows `0: 1`).
                description.push_str(rendered);
            }
            properties.push(entry);
        }
        json!({
            "name": index.to_string(),
            "value": {
                "type": "object",
                "subtype": "internal#entry",
                "className": "Object",
                "description": description,
                "preview": {
                    "type": "object",
                    "subtype": "internal#entry",
                    "description": description,
                    "overflow": false,
                    "properties": properties,
                },
            },
            "configurable": true,
            "enumerable": true,
            "writable": true,
            "isOwn": true,
        })
    };
    match inspection {
        fusor_runtime::CollectionInspection::Entries(entries) => entries
            .iter()
            .enumerate()
            .map(|(index, (key, value))| row(index, Some(key), value))
            .collect(),
        fusor_runtime::CollectionInspection::Values(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| row(index, None, value))
            .collect(),
    }
}

/// Appends the V8-style internal slot entries (`[[PromiseState]]`,
/// `[[ProxyHandler]]`, `[[Entries]]`, ...) for one live object, read through
/// the engine's slot-inspection API. Objects without internal slots
/// contribute nothing.
fn internal_slot_entries(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    object: &JsValue,
    _generate_preview: bool,
) -> Vec<Value> {
    if let Some(inspection) = context.promise_inspection(object).ok().flatten() {
        let state_name = match &inspection {
            PromiseInspection::Pending => "pending",
            PromiseInspection::Fulfilled(_) => "fulfilled",
            PromiseInspection::Rejected(_) => "rejected",
        };
        let mut entries = vec![json!({
            "name": "[[PromiseState]]",
            "value": {"type": "string", "value": state_name, "description": state_name},
        })];
        let result = match inspection {
            PromiseInspection::Pending => json!({"type": "undefined"}),
            PromiseInspection::Fulfilled(value) | PromiseInspection::Rejected(value) => {
                remote_object(context, &mut state.objects, intrinsics, &value, None, false)
            }
        };
        entries.push(json!({"name": "[[PromiseResult]]", "value": result}));
        return entries;
    }
    if let Some(inspection) = context.proxy_inspection(object).ok().flatten() {
        let mut remote = |value: Option<JsValue>| {
            value.map_or(
                json!({"type": "object", "subtype": "null", "value": null}),
                |value| remote_object(context, &mut state.objects, intrinsics, &value, None, false),
            )
        };
        let mut entries = vec![
            json!({"name": "[[ProxyHandler]]", "value": remote(inspection.handler)}),
            json!({"name": "[[ProxyTarget]]", "value": remote(inspection.target)}),
        ];
        if inspection.revoked {
            entries.push(json!({
                "name": "[[IsRevoked]]",
                "value": {"type": "boolean", "value": true, "description": "true"},
            }));
        }
        return entries;
    }
    if let Some(inspection) = context.collection_inspection(object).ok().flatten() {
        let label = intrinsics
            .class_tag(context, object)
            .map(|tag| tag_label(&tag))
            .unwrap_or_else(|_| "Object".to_owned());
        const ENTRY_PREVIEW_CAP: usize = 100;
        let mut rows = Vec::new();
        let overflow = match &inspection {
            fusor_runtime::CollectionInspection::Entries(entries) => {
                for (key, value) in entries.iter().take(ENTRY_PREVIEW_CAP) {
                    if let Some(entry) = preview_entry(context, intrinsics, "key", key, 0) {
                        rows.push(entry);
                    }
                    if let Some(entry) = preview_entry(context, intrinsics, "value", value, 0) {
                        rows.push(entry);
                    }
                }
                entries.len() > ENTRY_PREVIEW_CAP
            }
            fusor_runtime::CollectionInspection::Values(values) => {
                for value in values.iter().take(ENTRY_PREVIEW_CAP) {
                    if let Some(entry) = preview_entry(context, intrinsics, "value", value, 0) {
                        rows.push(entry);
                    }
                }
                values.len() > ENTRY_PREVIEW_CAP
            }
        };
        let count = match &inspection {
            fusor_runtime::CollectionInspection::Entries(entries) => entries.len(),
            fusor_runtime::CollectionInspection::Values(values) => values.len(),
        };
        let description = format!("{label}({count})");
        let entries_id = state.objects.register_entries(object);
        return vec![json!({
            "name": "[[Entries]]",
            "value": {
                "type": "object",
                "subtype": "internal#entries",
                "className": "Object",
                "description": description,
                "objectId": entries_id,
                "preview": {
                    "type": "object",
                    "subtype": "internal#entries",
                    "description": description,
                    "overflow": overflow,
                    "properties": rows,
                },
            },
        })];
    }
    if let Some(inspection) = context.array_buffer_inspection(object).ok().flatten() {
        let mut entries = Vec::new();
        // V8 renders the data block as a bounded byte listing.
        let bytes = context
            .array_buffer_bytes(object, 8)
            .ok()
            .flatten()
            .unwrap_or_default();
        let overflow = inspection.byte_length > bytes.len();
        let byte_rows = bytes
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                json!({"name": index.to_string(), "type": "number", "value": byte.to_string()})
            })
            .collect::<Vec<_>>();
        let data_description = format!(
            "{}({})",
            if inspection.shared {
                "SharedArrayBuffer"
            } else {
                "ArrayBuffer"
            },
            inspection.byte_length
        );
        entries.push(json!({
            "name": "[[ArrayBufferData]]",
            "value": {
                "type": "object",
                "subtype": "internal#arrayBufferData",
                "className": "Object",
                "description": data_description,
                "preview": {
                    "type": "object",
                    "subtype": "internal#arrayBufferData",
                    "description": data_description,
                    "overflow": overflow,
                    "properties": byte_rows,
                },
            },
        }));
        entries.push(json!({
            "name": "[[ArrayBufferByteLength]]",
            "value": number_remote(inspection.byte_length as f64),
        }));
        if let Some(max) = inspection.max_byte_length {
            entries.push(json!({
                "name": "[[ArrayBufferMaxByteLength]]",
                "value": number_remote(max as f64),
            }));
        }
        return entries;
    }
    if let Some(inspection) = context.data_view_inspection(object).ok().flatten() {
        let array_length = context
            .array_buffer_inspection(&inspection.buffer)
            .ok()
            .flatten()
            .map_or(0, |buffer| buffer.byte_length);
        let mut entries = vec![json!({
            "name": "[[ViewedArrayBuffer]]",
            "value": remote_object(context, &mut state.objects, intrinsics, &inspection.buffer, None, false),
        })];
        entries.push(json!({
            "name": "[[ArrayLength]]",
            "value": number_remote(array_length as f64),
        }));
        if let Some(length) = inspection.byte_length {
            entries.push(json!({
                "name": "[[ByteLength]]",
                "value": number_remote(length as f64),
            }));
        }
        entries.push(json!({
            "name": "[[ByteOffset]]",
            "value": number_remote(inspection.byte_offset as f64),
        }));
        return entries;
    }
    if let Some(inspection) = context.typed_array_inspection(object).ok().flatten() {
        let mut entries = Vec::new();
        if let Some(length) = inspection.length {
            entries.push(json!({
                "name": "[[ArrayLength]]",
                "value": number_remote(length as f64),
            }));
        }
        entries.push(json!({
            "name": "[[TypedArrayName]]",
            "value": {"type": "string", "value": inspection.element_name, "description": inspection.element_name},
        }));
        if let Some(length) = inspection.length {
            entries.push(json!({
                "name": "[[TypedArrayLength]]",
                "value": number_remote(length as f64),
            }));
        }
        entries.push(json!({
            "name": "[[ViewedArrayBuffer]]",
            "value": remote_object(context, &mut state.objects, intrinsics, &inspection.buffer, None, false),
        }));
        return entries;
    }
    if let Some(value) = context.date_value(object).ok().flatten() {
        return vec![json!({
            "name": "[[DateValue]]",
            "value": number_remote(value.as_f64()),
        })];
    }
    if let Some(value) = context.boxed_primitive(object).ok().flatten() {
        return vec![json!({
            "name": "[[PrimitiveValue]]",
            "value": remote_object(context, &mut state.objects, intrinsics, &value, None, false),
        })];
    }
    Vec::new()
}

/// Renders one plain-number `RemoteObject` with a matching description.
fn number_remote(number: f64) -> Value {
    json!({"type": "number", "value": number, "description": number.to_string()})
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
    let mut entry = json!({
        "name": name,
        "configurable": configurable,
        "enumerable": enumerable,
        "isOwn": true,
    });
    if is_accessor {
        // V8 never executes getters here — the frontend invokes them itself
        // through Runtime.callFunctionOn — and it always reports both sides,
        // with an undefined RemoteObject for the missing one.
        entry["get"] = match &get {
            Some(get) => remote_object(context, &mut state.objects, intrinsics, get, None, false),
            None => json!({"type": "undefined"}),
        };
        entry["set"] = match &set {
            Some(set) => remote_object(context, &mut state.objects, intrinsics, set, None, false),
            None => json!({"type": "undefined"}),
        };
    } else {
        if accessors_only {
            return None;
        }
        entry["writable"] = Value::Bool(writable);
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
    let limits = ScriptLimits::default();
    // Chrome's eager evaluation (`throwOnSideEffect`) never commits global
    // declarations; executing `let x = ...` there would steal the binding
    // from the real Enter evaluation and make it fail as a redeclaration.
    if params.get("throwOnSideEffect").and_then(Value::as_bool) == Some(true)
        && fusor::has_global_declarations(expression, limits).unwrap_or(false)
    {
        return protocol_result(id, json!({"result": {"type": "undefined"}}));
    }
    let source_name = params
        .get("sourceURL")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        // User-entered console entries get Chrome-style `VM<n>` script
        // names; eager and completion probes share the stable `console`
        // name so the debugger's script list does not churn per keystroke.
        .unwrap_or_else(|| {
            if params.get("userGesture").and_then(Value::as_bool) == Some(true) {
                format!("VM{}", state.next_vm())
            } else {
                "console".to_owned()
            }
        });
    let compiled = match state.script_cache.get(expression) {
        Some(compiled) => compiled,
        None => match fusor::compile_script(expression, &source_name, limits) {
            Ok(compiled) => {
                let compiled = Arc::new(compiled);
                state
                    .script_cache
                    .insert(expression.to_owned(), (*compiled).clone());
                compiled
            }
            Err(error) => {
                let (text, line, column) = script_compile_error_position(&error, expression);
                let uncaught = format!("Uncaught SyntaxError: {text}");
                let exception_remote = syntax_error_remote(
                    context,
                    state,
                    intrinsics,
                    &text,
                    &source_name,
                    line,
                    column,
                );
                return protocol_result(
                    id,
                    json!({
                        "result": {"type": "object", "subtype": "error", "description": uncaught},
                        "exceptionDetails": exception_details(
                            state,
                            &uncaught,
                            line,
                            column,
                            &source_name,
                            single_frame_stack_trace(&source_name, line, column),
                            exception_remote,
                        ),
                    }),
                );
            }
        },
    };
    let return_by_value = params
        .get("returnByValue")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let generate_preview = params
        .get("generatePreview")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let group = params.get("objectGroup").and_then(Value::as_str);
    match fusor::execute_compiled_script(context, &compiled, limits) {
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
            let (text, line, column, stack_trace) =
                script_error_position(context, intrinsics, &error, expression);
            let exception = exception_remote_object(context, state, intrinsics, &error);
            protocol_result(
                id,
                json!({
                    "result": {"type": "object", "subtype": "error", "description": text},
                    "exceptionDetails": exception_details(
                        state, &text, line, column, &source_name, stack_trace, exception,
                    ),
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
    // V8 previews enumerate every own property, enumerable or not — that is
    // why namespace objects like Math show their functions inline. Array
    // previews list index keys only (no `length`) and cap at 100 elements.
    let keys = intrinsics
        .own_string_keys(context, value)
        .unwrap_or_default();
    let array_like = matches!(class.subtype, Some("array") | Some("typedarray"));
    let cap = if array_like { 100 } else { 5 };
    let collection = matches!(
        class.subtype,
        Some("map") | Some("set") | Some("weakmap") | Some("weakset")
    );
    let promise = class.subtype == Some("promise");
    // Collection previews render their live entries (V8 shows
    // `Map(2) {1 => 2, 3 => 4}`), not the `size` field; Promise previews
    // render the state (`Promise {<pending>}`).
    let fields = if collection || promise {
        None
    } else {
        standard_preview_fields(class.subtype)
    };
    let own_count = keys
        .iter()
        .filter(|key| !array_like || is_array_index_key(key))
        .count();
    let fixed_count = if collection {
        collection_preview_count(context, value)
    } else {
        fields.map_or(0, <[&str]>::len) + usize::from(class.subtype == Some("typedarray"))
    };
    let overflow = if array_like {
        own_count > cap
    } else {
        own_count + fixed_count > cap
    };
    let mut properties = if collection {
        collection_preview_rows(context, intrinsics, value, cap)
    } else if promise {
        promise_preview_rows(context, intrinsics, value)
    } else {
        keys.iter()
            .filter(|key| !array_like || is_array_index_key(key))
            .take(if array_like { 100 } else { cap })
            .filter_map(|key| property_preview(context, intrinsics, value, key, depth))
            .collect::<Vec<_>>()
    };
    if let Some(fields) = fields {
        // V8 appends the standard fields after the indices; they live on the
        // prototype, so they are read directly.
        for field in fields {
            if let Some(field_value) = reflect_value(context, intrinsics, value, field)
                && let Some(entry) = preview_entry(context, intrinsics, field, &field_value, depth)
            {
                properties.push(entry);
            }
        }
        if class.subtype == Some("typedarray") {
            properties.push(json!({
                "name": "Symbol(Symbol.toStringTag)",
                "type": "string",
                "value": class.class_name,
            }));
        }
    }
    if !array_like {
        // The cap applies to own and standard fields together.
        properties.truncate(cap);
    }
    let mut preview = json!({
        "type": "object",
        "description": class.label,
        "overflow": overflow,
        "properties": properties,
    });
    if collection {
        // The frontend renders collection previews from the dedicated
        // `entries` field (`Map(2) {1 => 2, 3 => 4}`); the key/value rows
        // above stay for the [[Entries]] internal-property preview.
        preview["entries"] =
            Value::Array(collection_entry_previews(context, intrinsics, value, cap));
    }
    if let Some(subtype) = class.subtype {
        preview["subtype"] = Value::String(subtype.to_owned());
    }
    Some(preview)
}

/// The `EntryPreview` rows of one collection: `{key, value}` pairs for
/// Maps, `{value}` rows for Sets — the shape the frontend renders as
/// `key => value` in collapsed collection previews.
fn collection_entry_previews(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    cap: usize,
) -> Vec<Value> {
    let mut entries = Vec::new();
    let object_preview = |entry: Value| {
        // The frontend renders EntryPreview items from their description.
        let description = entry["value"].as_str().unwrap_or_default();
        let mut preview = json!({
            "type": entry["type"],
            "description": description,
            "value": entry["value"],
        });
        if let Some(subtype) = entry.get("subtype") {
            preview["subtype"] = subtype.clone();
        }
        preview
    };
    match context.collection_inspection(value).ok().flatten() {
        Some(fusor_runtime::CollectionInspection::Entries(rows)) => {
            for (key, entry_value) in rows.iter().take(cap) {
                let Some(key_entry) = preview_entry(context, intrinsics, "key", key, 0) else {
                    continue;
                };
                let Some(value_entry) = preview_entry(context, intrinsics, "value", entry_value, 0)
                else {
                    continue;
                };
                entries.push(json!({
                    "key": object_preview(key_entry),
                    "value": object_preview(value_entry),
                }));
            }
        }
        Some(fusor_runtime::CollectionInspection::Values(values)) => {
            for entry_value in values.iter().take(cap) {
                if let Some(entry) = preview_entry(context, intrinsics, "value", entry_value, 0) {
                    entries.push(json!({"value": object_preview(entry)}));
                }
            }
        }
        None => {}
    }
    entries
}

fn is_array_index_key(key: &str) -> bool {
    key.parse::<u64>()
        .is_ok_and(|index| index.to_string() == key)
}

/// The live entry count of one collection, for the preview's overflow math.
fn collection_preview_count(context: &mut Context<'_>, value: &JsValue) -> usize {
    match context.collection_inspection(value).ok().flatten() {
        Some(fusor_runtime::CollectionInspection::Entries(entries)) => entries.len(),
        Some(fusor_runtime::CollectionInspection::Values(values)) => values.len(),
        None => 0,
    }
}

/// The V8 Promise preview rows: the `[[PromiseState]]` string plus the
/// `[[PromiseResult]]` value preview — the frontend renders them as
/// `Promise {<pending>}` / `Promise {<fulfilled>: 5}`.
fn promise_preview_rows(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
) -> Vec<Value> {
    let inspection = match context.promise_inspection(value).ok().flatten() {
        Some(inspection) => inspection,
        None => return Vec::new(),
    };
    let (state, result): (&str, Option<&JsValue>) = match &inspection {
        PromiseInspection::Pending => ("pending", None),
        PromiseInspection::Fulfilled(value) => ("fulfilled", Some(value)),
        PromiseInspection::Rejected(value) => ("rejected", Some(value)),
    };
    let mut rows = vec![json!({
        "name": "[[PromiseState]]",
        "type": "string",
        "value": state,
    })];
    match result {
        Some(value) => {
            if let Some(entry) = preview_entry(context, intrinsics, "[[PromiseResult]]", value, 0) {
                rows.push(entry);
            }
        }
        None => rows.push(json!({
            "name": "[[PromiseResult]]",
            "type": "undefined",
        })),
    }
    rows
}

/// The V8 entry preview rows of one collection: alternating
/// `key`/`value` PropertyPreviews for Maps, bare `value` rows for Sets.
fn collection_preview_rows(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
    cap: usize,
) -> Vec<Value> {
    let mut rows = Vec::new();
    match context.collection_inspection(value).ok().flatten() {
        Some(fusor_runtime::CollectionInspection::Entries(entries)) => {
            for (key, entry_value) in entries.iter().take(cap) {
                if let Some(entry) = preview_entry(context, intrinsics, "key", key, 0) {
                    rows.push(entry);
                }
                if let Some(entry) = preview_entry(context, intrinsics, "value", entry_value, 0) {
                    rows.push(entry);
                }
            }
        }
        Some(fusor_runtime::CollectionInspection::Values(values)) => {
            for entry_value in values.iter().take(cap) {
                if let Some(entry) = preview_entry(context, intrinsics, "value", entry_value, 0) {
                    rows.push(entry);
                }
            }
        }
        None => {}
    }
    rows
}

/// The fixed preview fields V8 appends for built-in binary view classes.
fn standard_preview_fields(subtype: Option<&str>) -> Option<&'static [&'static str]> {
    match subtype {
        Some("typedarray") => Some(&["buffer", "byteLength", "byteOffset", "length"]),
        Some("arraybuffer") => Some(&["byteLength", "maxByteLength", "resizable", "detached"]),
        Some("dataview") => Some(&["buffer", "byteLength", "byteOffset"]),
        Some("regexp") => Some(&[
            "dotAll",
            "flags",
            "global",
            "hasIndices",
            "ignoreCase",
            "multiline",
            "source",
            "sticky",
            "unicode",
        ]),
        _ => None,
    }
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
    preview_entry(context, intrinsics, key, &value, depth)
}

/// Renders one `PropertyPreview` entry for a live value.
fn preview_entry(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    key: &str,
    value: &JsValue,
    _depth: usize,
) -> Option<Value> {
    let kind = value.kind().ok()?;
    let entry = match kind {
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
            "value": short_number(value),
        }),
        ValueKind::String => json!({
            "name": key,
            "type": "string",
            "value": short_string(value),
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
            // V8 carries the function's name so the frontend renders `ƒ A`;
            // an empty value renders as a bare `ƒ`.
            "value": reflect_string(context, intrinsics, value, "name")
                .filter(|name| !name.is_empty())
                .unwrap_or_default(),
        }),
        ValueKind::Object => {
            let class = object_class(context, intrinsics, value);
            let mut nested = json!({
                "name": key,
                "type": "object",
                "value": class.label,
            });
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
                .unwrap_or_else(|_| "Object".to_owned());
            if tag.eq_ignore_ascii_case("array") {
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
    stack_trace: Value,
    exception: Value,
) -> Value {
    json!({
        "exceptionId": state.next_exception_id(),
        "text": text,
        "lineNumber": line,
        "columnNumber": column,
        "url": url,
        "scriptId": url,
        "stackTrace": stack_trace,
        "exception": exception,
        "executionContextId": 1,
    })
}

/// Renders the thrown exception as an expandable `RemoteObject` when the
/// engine retained the actual value, and as a synthetic error description
/// otherwise.
fn exception_remote_object(
    context: &mut Context<'_>,
    state: &mut InspectState,
    intrinsics: &InspectIntrinsics,
    error: &ScriptEvaluationError,
) -> Value {
    if let ScriptEvaluationError::Runtime(GlobalScriptError::Execution(ExecutionError::Exception(
        exception,
    ))) = error
        && let Some(value) = exception.thrown_value()
    {
        // Exception RemoteObjects carry no preview (V8 shape): the frontend
        // fetches the properties itself and appends the preview description
        // to the entry title otherwise, doubling the message.
        return remote_object(context, &mut state.objects, intrinsics, value, None, false);
    }
    // Escaped engine errors carry no rooted value; materialize the family
    // object with the retained frames as its stack.
    if let ScriptEvaluationError::Runtime(GlobalScriptError::Execution(ExecutionError::Exception(
        exception,
    ))) = error
        && let Some(remote) = engine_error_remote_object(context, state, intrinsics, exception)
    {
        return remote;
    }
    json!({"type": "object", "subtype": "error", "description": error.to_string()})
}

/// Builds the single-frame `StackTrace` V8 reports for console syntax
/// failures: the evaluation frame itself, at the diagnostic position.
fn single_frame_stack_trace(url: &str, line: u64, column: u64) -> Value {
    json!({"callFrames": [{
        "functionName": "",
        "scriptId": url,
        "url": url,
        "lineNumber": line,
        "columnNumber": column,
    }]})
}

/// Builds the CDP `StackTrace` for one escaped JavaScript exception from the
/// engine's retained caller frames, falling back to the throw origin.
fn exception_stack_trace(exception: &fusor_runtime::JsException) -> Value {
    let frame = |source_name: &str, source_text: &str, start: usize| {
        let (line, column) = source_position(source_text, start);
        json!({
            "functionName": "",
            "scriptId": source_name,
            "url": source_name,
            "lineNumber": line,
            "columnNumber": column,
        })
    };
    let frames = if exception.caller_frames().is_empty() {
        vec![frame(
            exception.source_name(),
            exception.source_text(),
            exception.source_span().start() as usize,
        )]
    } else {
        exception
            .caller_frames()
            .iter()
            .map(|entry| {
                frame(
                    entry.source_name(),
                    entry.source_text(),
                    entry.source_span().start() as usize,
                )
            })
            .collect::<Vec<_>>()
    };
    json!({"callFrames": frames})
}

/// Renders a script failure as `(text, line, column, stackTrace)`, extracting
/// the thrown exception's own position and message when the engine retained
/// one.
fn script_error_position(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    error: &ScriptEvaluationError,
    source: &str,
) -> (String, u64, u64, Value) {
    match error {
        ScriptEvaluationError::Runtime(GlobalScriptError::Execution(
            ExecutionError::Exception(exception),
        )) => {
            let (line, column) = source_position(
                exception.source_text(),
                exception.source_span().start() as usize,
            );
            (
                exception_text(context, intrinsics, exception),
                line,
                column,
                exception_stack_trace(exception),
            )
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
            (diagnostic.0, line, column, json!({"callFrames": []}))
        }
        other => (other.to_string(), 0, 0, json!({"callFrames": []})),
    }
}

/// Renders a thrown exception like DevTools does: `Name: message` followed by
/// the retained stack text when the engine installed one.
fn exception_text(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    exception: &fusor_runtime::JsException,
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
    string_value(value).or_else(|| Some(super::format::format_value(value)))
}

/// Renders one function value through `Function.prototype.toString`, the
/// source V8's `RemoteObject` descriptions come from.
fn reflect_function_source(
    context: &mut Context<'_>,
    intrinsics: &InspectIntrinsics,
    value: &JsValue,
) -> Option<String> {
    let result = call(
        context,
        &intrinsics.function_to_string,
        value.clone(),
        vec![],
    )
    .ok()?;
    string_value(&result)
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
    super::format::format_value(value)
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
    use fusor::{ScriptLimits, evaluate_script};
    use fusor_runtime::{Runtime, RuntimeLimits};
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
            assert!(first.starts_with("fusor:"));
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
                ("({})", "Object"),
                ("[]", "Array"),
                ("new Date(0)", "Date"),
                ("/x/", "RegExp"),
                ("new Map()", "Map"),
                ("new Uint8Array(2)", "Uint8Array"),
                ("Temporal.Now.plainDateTimeISO()", "Temporal.PlainDateTime"),
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

            let array = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "[7, 8]", "returnByValue": true}),
            );
            let value = &array["result"]["result"]["value"];
            assert_eq!(value[0].as_f64(), Some(7.0));
            assert_eq!(value[1].as_f64(), Some(8.0));
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
    fn get_properties_lists_accessors_without_executing_getters() {
        with_engine(|context, state, intrinsics| {
            let _ = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "globalThis.__accessor_probes = 0"}),
            );
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ get answer() { globalThis.__accessor_probes += 1; return 42; }, set answer(v) {}, plain: 1 })"}),
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
            let entry = &result[0];
            assert_eq!(entry["get"]["type"], "function");
            assert_eq!(entry["set"]["type"], "function");
            assert!(
                entry.get("value").is_none(),
                "V8 never executes getters in getProperties"
            );
            assert!(entry.get("wasThrown").is_none());
            assert!(
                response["result"].get("internalProperties").is_none(),
                "accessor-only listings omit internalProperties entirely"
            );

            let probes = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "globalThis.__accessor_probes"}),
            );
            assert_eq!(
                probes["result"]["result"]["value"].as_f64(),
                Some(0.0),
                "listing properties must not run getter bodies"
            );
        });
    }

    #[test]
    fn get_properties_marks_missing_accessor_sides_as_undefined() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ set only(v) {} })"}),
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
            let entry = &response["result"]["result"][0];
            assert_eq!(entry["name"], "only");
            assert_eq!(
                entry["get"],
                serde_json::json!({"type": "undefined"}),
                "V8 reports the missing getter as an undefined RemoteObject"
            );
            assert_eq!(entry["set"]["type"], "function");
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
                ("new Map()", "Map(0)", Some("map")),
                ("new Proxy({}, {})", "Proxy", Some("proxy")),
                ("globalThis.Reflect", "Reflect", None),
                ("new Uint8Array(3)", "Uint8Array(3)", Some("typedarray")),
                ("Temporal.Now.plainDateTimeISO()", "PlainDateTime", None),
                ("Temporal.Now.instant()", "Instant", None),
                ("new ArrayBuffer(6)", "ArrayBuffer(6)", Some("arraybuffer")),
                ("/x/g", "/x/g", Some("regexp")),
            ] {
                let value = evaluate(context, source);
                let rendered =
                    remote_object(context, &mut state.objects, intrinsics, &value, None, false);
                assert_eq!(rendered["description"], description, "label for {source}");
                let class_name = if subtype == Some("regexp") {
                    "RegExp"
                } else {
                    description.split('(').next().unwrap_or(description)
                };
                assert_eq!(rendered["className"], class_name, "className for {source}");
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
    fn get_properties_lists_builtin_methods_without_the_experimental_flag() {
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
            let own = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id, "ownProperties": true}),
            );
            let proto_id = own["result"]["internalProperties"][0]["value"]["objectId"]
                .as_str()
                .expect("proto id");
            let proto_response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": proto_id, "ownProperties": true}),
            );
            let names: Vec<&str> = proto_response["result"]["result"]
                .as_array()
                .expect("proto properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert!(
                names.contains(&"hasOwnProperty"),
                "non-enumerable built-in methods are listed without the experimental flag"
            );
        });
    }

    #[test]
    fn previews_include_non_enumerable_properties() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "Math", "generatePreview": true}),
            );
            let preview = &response["result"]["result"]["preview"];
            let properties = preview["properties"]
                .as_array()
                .expect("preview properties");
            assert!(
                properties.len() == 5 && preview["overflow"] == true,
                "Math has dozens of own props; previews cap at five, got {preview:?}"
            );
            let first = &properties[0];
            assert_eq!(first["type"], "function");
            assert_eq!(
                first["value"], "min",
                "function previews carry the function name (V8 renders `ƒ min`)"
            );
            assert!(
                !first["name"].as_str().unwrap_or_default().is_empty(),
                "preview entries carry their property name"
            );
        });
    }

    #[test]
    fn previews_do_not_nest_value_previews() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ b: { c: 1 } })", "generatePreview": true}),
            );
            let properties = response["result"]["result"]["preview"]["properties"]
                .as_array()
                .expect("preview properties");
            assert!(
                properties[0].get("valuePreview").is_none(),
                "V8 never sends valuePreview in these flows, got {properties:?}"
            );
        });
    }

    #[test]
    fn array_previews_list_indices_without_length() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "[1, 2, 3]", "generatePreview": true}),
            );
            let preview = &response["result"]["result"]["preview"];
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(names, vec!["0", "1", "2"], "indices only, no length");
            assert_eq!(preview["overflow"], false);
        });
    }

    #[test]
    fn array_previews_cap_at_one_hundred_elements() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "Array.from({ length: 101 }, (_, index) => index)",
                    "generatePreview": true,
                }),
            );
            let preview = &response["result"]["result"]["preview"];
            assert_eq!(
                preview["properties"].as_array().expect("properties").len(),
                100,
                "array previews cap at 100 elements"
            );
            assert_eq!(preview["overflow"], true);
        });
    }

    #[test]
    fn primitive_property_values_carry_descriptions() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "({ count: 1, label: 'x' })"}),
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
            let entries = response["result"]["result"].as_array().expect("properties");
            assert_eq!(entries[0]["value"]["description"], "1");
            assert_eq!(entries[1]["value"]["description"], "x");
        });
    }

    #[test]
    fn typed_array_previews_include_the_standard_fields() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "new Uint8Array([1, 2, 3, 4, 5, 6])",
                    "generatePreview": true,
                }),
            );
            let preview = &response["result"]["result"]["preview"];
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(
                names,
                vec![
                    "0",
                    "1",
                    "2",
                    "3",
                    "4",
                    "5",
                    "buffer",
                    "byteLength",
                    "byteOffset",
                    "length",
                    "Symbol(Symbol.toStringTag)",
                ],
                "typed arrays preview indices plus the standard fields"
            );
            let buffer = &preview["properties"][6];
            assert_eq!(buffer["type"], "object");
            assert_eq!(buffer["subtype"], "arraybuffer");
            assert_eq!(buffer["value"], "ArrayBuffer(6)");
            let tag = &preview["properties"][10];
            assert_eq!(tag["type"], "string");
            assert_eq!(tag["value"], "Uint8Array");
        });
    }

    #[test]
    fn buffer_previews_include_the_standard_fields() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "new ArrayBuffer(6)", "generatePreview": true}),
            );
            let preview = &response["result"]["result"]["preview"];
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(
                names,
                vec!["byteLength", "maxByteLength", "resizable", "detached"],
                "ArrayBuffer previews list the standard fields"
            );
            assert_eq!(preview["properties"][0]["value"], "6");
            assert_eq!(preview["properties"][2]["value"], "false");

            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "new DataView(new ArrayBuffer(8))", "generatePreview": true}),
            );
            let result = &response["result"]["result"];
            assert_eq!(result["description"], "DataView(8)");
            let names: Vec<&str> = result["preview"]["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(
                names,
                vec!["buffer", "byteLength", "byteOffset"],
                "DataView previews list the standard fields"
            );
            assert_eq!(
                result["preview"]["properties"][0]["value"],
                "ArrayBuffer(8)"
            );
        });
    }

    #[test]
    fn collection_and_regexp_previews_include_standard_fields() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "new Map([['a', 1]])", "generatePreview": true}),
            );
            let preview = &response["result"]["result"]["preview"];
            // V8 renders the live entries, not the size field:
            // `Map(1) {a => 1}`.
            assert_eq!(preview["description"], "Map(1)");
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(names, vec!["key", "value"], "one key/value entry row");
            assert_eq!(preview["properties"][0]["value"], "a");
            assert_eq!(preview["properties"][1]["value"], "1");
            assert_eq!(preview["overflow"], false);

            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "/x/", "generatePreview": true}),
            );
            let preview = &response["result"]["result"]["preview"];
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(
                names,
                vec!["lastIndex", "dotAll", "flags", "global", "hasIndices"],
                "the five-property cap applies to own and standard fields together"
            );
            assert_eq!(preview["overflow"], true);
        });
    }

    #[test]
    fn prototypes_may_be_functions() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "globalThis.__fn_proto = (function base() {}); globalThis.__fn_derived = Object.setPrototypeOf((function derived() {}), globalThis.__fn_proto); globalThis.__fn_derived",
                }),
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
            let internal = response["result"]["internalProperties"]
                .as_array()
                .expect("internal properties");
            assert_eq!(internal[0]["name"], "[[Prototype]]");
            assert_eq!(
                internal[0]["value"]["type"], "function",
                "function prototypes are reported too"
            );
        });
    }

    /// Evaluates one console expression and reads its `internalProperties`.
    fn internal_properties(
        context: &mut Context<'_>,
        state: &mut InspectState,
        intrinsics: &InspectIntrinsics,
        expression: &str,
    ) -> Vec<Value> {
        let evaluated = protocol(
            context,
            state,
            intrinsics,
            "Runtime.evaluate",
            serde_json::json!({"expression": expression}),
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
        response["result"]["internalProperties"]
            .as_array()
            .expect("internal properties")
            .clone()
    }

    fn internal_by_name<'a>(internal: &'a [Value], name: &str) -> &'a Value {
        internal
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("missing internal property {name}: {internal:?}"))
    }

    #[test]
    fn promise_previews_report_the_state() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "new Promise(() => {})", "generatePreview": true}),
            );
            let preview = &evaluated["result"]["result"]["preview"];
            assert_eq!(preview["subtype"], "promise");
            let names: Vec<&str> = preview["properties"]
                .as_array()
                .expect("preview properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("name"))
                .collect();
            assert_eq!(names, vec!["[[PromiseState]]", "[[PromiseResult]]"]);
            assert_eq!(preview["properties"][0]["value"], "pending");
            assert_eq!(preview["properties"][1]["type"], "undefined");

            let fulfilled = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "Promise.resolve(5)", "generatePreview": true}),
            );
            let preview = &fulfilled["result"]["result"]["preview"];
            assert_eq!(preview["properties"][0]["value"], "fulfilled");
            assert_eq!(preview["properties"][1]["value"], "5");
        });
    }

    #[test]
    fn promise_internal_properties_report_state_and_result() {
        with_engine(|context, state, intrinsics| {
            let internal = internal_properties(context, state, intrinsics, "Promise.resolve(41)");
            assert_eq!(
                internal_by_name(&internal, "[[PromiseState]]")["value"]["value"],
                "fulfilled"
            );
            let result = &internal_by_name(&internal, "[[PromiseResult]]")["value"];
            assert_eq!(result["type"], "number");
            assert_eq!(result["value"].as_f64(), Some(41.0));

            let pending = internal_properties(context, state, intrinsics, "new Promise(() => {})");
            assert_eq!(
                internal_by_name(&pending, "[[PromiseState]]")["value"]["value"],
                "pending"
            );
            assert_eq!(
                internal_by_name(&pending, "[[PromiseResult]]")["value"]["type"],
                "undefined",
                "an unsettled promise reports an undefined result"
            );
        });
    }

    #[test]
    fn proxy_internal_properties_report_handler_and_target() {
        with_engine(|context, state, intrinsics| {
            let internal =
                internal_properties(context, state, intrinsics, "new Proxy({ x: 1 }, { y: 2 })");
            assert!(
                internal_by_name(&internal, "[[ProxyHandler]]")["value"]["objectId"].is_string(),
                "the handler carries a registered objectId"
            );
            assert!(
                internal_by_name(&internal, "[[ProxyTarget]]")["value"]["objectId"].is_string(),
                "the target carries a registered objectId"
            );
            assert!(
                internal
                    .iter()
                    .all(|entry| entry["name"] != "[[IsRevoked]]"),
                "live proxies omit the revocation flag"
            );

            let revoked = internal_properties(
                context,
                state,
                intrinsics,
                "(() => { const pair = Proxy.revocable({}, {}); pair.revoke(); return pair.proxy; })()",
            );
            assert_eq!(
                internal_by_name(&revoked, "[[ProxyHandler]]")["value"]["subtype"],
                "null",
                "revocation clears the handler"
            );
            assert_eq!(
                internal_by_name(&revoked, "[[IsRevoked]]")["value"]["value"],
                true,
                "revoked proxies report the flag"
            );
        });
    }

    #[test]
    fn collection_internal_properties_report_entries() {
        with_engine(|context, state, intrinsics| {
            let internal =
                internal_properties(context, state, intrinsics, "new Map([['a', 1], ['b', 2]])");
            let entries = &internal_by_name(&internal, "[[Entries]]")["value"];
            assert_eq!(entries["subtype"], "internal#entries");
            assert_eq!(entries["description"], "Map(2)");
            let properties = entries["preview"]["properties"].as_array().expect("rows");
            assert_eq!(properties.len(), 4, "two key/value rows");
            assert_eq!(properties[0]["name"], "key");
            assert_eq!(properties[0]["value"], "a");
            assert_eq!(properties[1]["name"], "value");
            assert_eq!(properties[1]["value"], "1");
            assert_eq!(properties[2]["name"], "key");
            assert_eq!(properties[2]["value"], "b");

            let set_internal = internal_properties(context, state, intrinsics, "new Set([7])");
            let set_entries = &internal_by_name(&set_internal, "[[Entries]]")["value"];
            assert_eq!(set_entries["description"], "Set(1)");
            let set_rows = set_entries["preview"]["properties"]
                .as_array()
                .expect("rows");
            assert_eq!(set_rows.len(), 1, "one value row");
            assert_eq!(set_rows[0]["name"], "value");
            assert_eq!(set_rows[0]["value"], "7");
        });
    }

    #[test]
    fn collection_entries_expand_through_the_registered_view() {
        with_engine(|context, state, intrinsics| {
            let internal = internal_properties(context, state, intrinsics, "new Map([['a', 1]])");
            let entries = &internal_by_name(&internal, "[[Entries]]")["value"];
            let entries_id = entries["objectId"]
                .as_str()
                .expect("the entries value carries an expandable objectId")
                .to_owned();
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": entries_id}),
            );
            let rows = response["result"]["result"].as_array().expect("entry rows");
            assert_eq!(rows.len(), 1, "one row per live entry");
            assert_eq!(rows[0]["name"], "0");
            let entry = &rows[0]["value"];
            assert_eq!(entry["subtype"], "internal#entry");
            let row_properties = entry["preview"]["properties"]
                .as_array()
                .expect("row preview");
            assert_eq!(row_properties.len(), 2, "key and value previews");
            assert_eq!(row_properties[0]["name"], "key");
            assert_eq!(row_properties[0]["value"], "a");
            assert_eq!(row_properties[1]["name"], "value");
            assert_eq!(row_properties[1]["value"], "1");
            assert!(
                rows[0]["isOwn"].as_bool().expect("isOwn"),
                "entry rows present as own properties"
            );

            let set_internal = internal_properties(context, state, intrinsics, "new Set([7])");
            let set_entries_id =
                internal_by_name(&set_internal, "[[Entries]]")["value"]["objectId"]
                    .as_str()
                    .expect("entries objectId")
                    .to_owned();
            let set_response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": set_entries_id}),
            );
            let set_rows = set_response["result"]["result"]
                .as_array()
                .expect("set rows");
            let set_row_properties = set_rows[0]["value"]["preview"]["properties"]
                .as_array()
                .expect("set row preview");
            assert_eq!(
                set_row_properties.len(),
                1,
                "set rows preview the value only"
            );
            assert_eq!(set_row_properties[0]["name"], "value");
            assert_eq!(set_row_properties[0]["value"], "7");
        });
    }

    #[test]
    fn binary_view_internal_properties_report_the_view_slots() {
        with_engine(|context, state, intrinsics| {
            let internal =
                internal_properties(context, state, intrinsics, "new Uint8Array([1, 2, 3])");
            assert_eq!(
                internal_by_name(&internal, "[[ArrayLength]]")["value"]["value"].as_f64(),
                Some(3.0)
            );
            assert_eq!(
                internal_by_name(&internal, "[[TypedArrayLength]]")["value"]["value"].as_f64(),
                Some(3.0)
            );
            assert_eq!(
                internal_by_name(&internal, "[[TypedArrayName]]")["value"]["value"],
                "Uint8Array"
            );
            assert!(
                internal_by_name(&internal, "[[ViewedArrayBuffer]]")["value"]["objectId"]
                    .is_string()
            );

            let buffer_internal =
                internal_properties(context, state, intrinsics, "new ArrayBuffer(16)");
            assert_eq!(
                internal_by_name(&buffer_internal, "[[ArrayBufferByteLength]]")["value"]["value"]
                    .as_f64(),
                Some(16.0)
            );
            assert_eq!(
                internal_by_name(&buffer_internal, "[[ArrayBufferData]]")["value"]["subtype"],
                "internal#arrayBufferData"
            );

            let view_internal = internal_properties(
                context,
                state,
                intrinsics,
                "new DataView(new ArrayBuffer(8), 2, 4)",
            );
            assert_eq!(
                internal_by_name(&view_internal, "[[ByteOffset]]")["value"]["value"].as_f64(),
                Some(2.0)
            );
            assert_eq!(
                internal_by_name(&view_internal, "[[ByteLength]]")["value"]["value"].as_f64(),
                Some(4.0)
            );
            assert!(
                internal_by_name(&view_internal, "[[ViewedArrayBuffer]]")["value"]["objectId"]
                    .is_string()
            );
        });
    }

    #[test]
    fn date_and_boxed_internal_properties_report_the_payloads() {
        with_engine(|context, state, intrinsics| {
            let internal = internal_properties(context, state, intrinsics, "new Date(1234)");
            assert_eq!(
                internal_by_name(&internal, "[[DateValue]]")["value"]["value"].as_f64(),
                Some(1234.0)
            );

            let boxed = internal_properties(context, state, intrinsics, "new Number(5)");
            assert_eq!(
                internal_by_name(&boxed, "[[PrimitiveValue]]")["value"]["value"].as_f64(),
                Some(5.0)
            );
            let boxed_string = internal_properties(context, state, intrinsics, "new String('x')");
            assert_eq!(
                internal_by_name(&boxed_string, "[[PrimitiveValue]]")["value"]["value"],
                "x"
            );
        });
    }

    #[test]
    fn class_instances_label_with_the_constructor_name() {
        with_engine(|context, state, intrinsics| {
            evaluate(context, "class A { name = 1 }");
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "new A()", "generatePreview": true}),
            );
            let result = &evaluated["result"]["result"];
            assert_eq!(
                result["description"], "A",
                "the instance description names the class, not Object"
            );
            assert_eq!(
                result["preview"]["description"], "A",
                "the preview carries the constructor name"
            );
        });
    }

    #[test]
    fn uncaught_exceptions_render_exception_thrown_events() {
        with_engine(|context, state, intrinsics| {
            let source = "throw new Error('boom')";
            let error = evaluate_script(context, source, "<repl>:7", ScriptLimits::default())
                .expect_err("throws");
            let classified = CliException::from_script_error(&error, source);
            let event = exception_thrown_event(
                context,
                state,
                Some(intrinsics),
                &classified,
                source,
                "<repl>:7",
            );
            assert_eq!(event["method"], "Runtime.exceptionThrown");
            let details = &event["params"]["exceptionDetails"];
            assert!(
                details["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("boom")),
                "the event names the thrown message"
            );
            assert!(details["exceptionId"].is_u64());
            assert_eq!(details["url"], "<repl>:7");
            assert!(
                event["params"]["timestamp"].is_u64(),
                "the event carries the wall-clock timestamp"
            );

            let syntax_source = "function (";
            let syntax_error =
                evaluate_script(context, syntax_source, "<repl>:8", ScriptLimits::default())
                    .expect_err("syntax");
            let classified = CliException::from_script_error(&syntax_error, syntax_source);
            let event = exception_thrown_event(
                context,
                state,
                Some(intrinsics),
                &classified,
                syntax_source,
                "<repl>:8",
            );
            assert_eq!(event["method"], "Runtime.exceptionThrown");
            let details = &event["params"]["exceptionDetails"];
            assert!(
                details["text"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "SyntaxError entries render the diagnostic text"
            );
            assert_eq!(
                details["exception"]["subtype"], "error",
                "the exception renders as an error RemoteObject"
            );

            let message = CliException::Message("module link failed".to_owned());
            let event = exception_thrown_event(context, state, None, &message, "", "<repl>:9");
            let details = &event["params"]["exceptionDetails"];
            assert_eq!(details["text"], "Uncaught module link failed");
            assert_eq!(details["url"], "<repl>:9");
        });
    }

    #[test]
    fn console_evaluations_report_exceptions_in_the_response_alone() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "1 +"}),
            );
            let details = &response["result"]["exceptionDetails"];
            assert!(
                details["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Uncaught SyntaxError:")),
                "the response carries the V8-prefixed diagnostic: {details}"
            );
            assert_eq!(
                details["exception"]["className"], "SyntaxError",
                "the exception object names the concrete family"
            );
            assert!(
                details["exception"]["objectId"].is_string(),
                "the exception object is expandable"
            );

            let clean = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "1 + 1"}),
            );
            assert!(
                clean["result"].get("exceptionDetails").is_none(),
                "successful evaluations carry no exceptionDetails"
            );
        });
    }

    #[test]
    fn function_property_previews_carry_the_name() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "(() => { globalThis.__A = class A { name = 1 }; return { constructor: globalThis.__A }; })()",
                    "generatePreview": true,
                }),
            );
            let preview = &evaluated["result"]["result"]["preview"];
            let function_entry = preview["properties"]
                .as_array()
                .expect("properties")
                .iter()
                .find(|entry| entry["name"] == "constructor")
                .expect("constructor entry");
            assert_eq!(function_entry["type"], "function");
            assert_eq!(
                function_entry["value"], "A",
                "function previews carry the name so the frontend renders ƒ A"
            );
        });
    }

    #[test]
    fn non_indexed_properties_only_filters_array_indices() {
        with_engine(|context, state, intrinsics| {
            let evaluated = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "({ 0: 'a', 1: 'b', length: 2, foo: 9 })"
                }),
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
                serde_json::json!({"objectId": object_id, "ownProperties": true, "nonIndexedPropertiesOnly": true}),
            );
            let names: Vec<&str> = response["result"]["result"]
                .as_array()
                .expect("properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(
                names,
                vec!["length", "foo"],
                "index keys are omitted for range-grouped displays"
            );

            // The default listing still includes every own key.
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.getProperties",
                serde_json::json!({"objectId": object_id, "ownProperties": true}),
            );
            let names: Vec<&str> = response["result"]["result"]
                .as_array()
                .expect("properties")
                .iter()
                .map(|entry| entry["name"].as_str().expect("property name"))
                .collect();
            assert_eq!(names, vec!["0", "1", "length", "foo"]);
        });
    }

    #[test]
    fn console_entries_use_incrementing_vm_source_names() {
        with_engine(|context, state, intrinsics| {
            let first = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw 1", "userGesture": true}),
            );
            let second = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw 2", "userGesture": true}),
            );
            let eager = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "throw 3", "userGesture": false}),
            );
            assert_eq!(first["result"]["exceptionDetails"]["url"], "VM0");
            assert_eq!(second["result"]["exceptionDetails"]["url"], "VM1");
            assert_eq!(
                eager["result"]["exceptionDetails"]["url"], "console",
                "eager probes keep the shared console name"
            );
        });
    }

    #[test]
    fn exception_details_carry_the_stack_trace_and_exception_object() {
        with_engine(|context, state, intrinsics| {
            let response = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "function boom() { throw new Error('kapow') } boom()",
                    "userGesture": true,
                }),
            );
            let details = &response["result"]["exceptionDetails"];
            assert!(
                details["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("kapow"))
            );
            let frames = details["stackTrace"]["callFrames"]
                .as_array()
                .expect("call frames");
            assert!(!frames.is_empty(), "thrown errors carry their stack");
            assert!(frames[0]["lineNumber"].is_u64());
            assert!(frames[0]["columnNumber"].is_u64());
            assert_eq!(frames[0]["url"], "VM0");
            let exception = &details["exception"];
            assert_eq!(exception["subtype"], "error");
            assert!(
                exception["objectId"].is_string(),
                "the real exception object is expandable in the console"
            );
        });
    }

    #[test]
    fn evaluations_reuse_compiled_bytecode() {
        with_engine(|context, state, intrinsics| {
            let expression = "40 + 2";
            let first = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": expression}),
            );
            let second = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": expression}),
            );
            assert_eq!(first["result"]["result"]["value"].as_f64(), Some(42.0));
            assert_eq!(second["result"]["result"]["value"].as_f64(), Some(42.0));
            assert_eq!(
                state.script_cache.hits(),
                1,
                "identical expressions compile once"
            );
        });
    }

    #[test]
    fn eager_evaluation_does_not_commit_global_declarations() {
        with_engine(|context, state, intrinsics| {
            let eager = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "let eager_c = class {}",
                    "throwOnSideEffect": true,
                }),
            );
            assert_eq!(
                eager["result"]["result"],
                serde_json::json!({"type": "undefined"}),
                "declaration sources are not executed by side-effect probes"
            );
            assert!(eager["result"].get("exceptionDetails").is_none());

            let committed = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "let eager_c = class {}",
                    "userGesture": true,
                }),
            );
            assert!(
                committed["result"].get("exceptionDetails").is_none(),
                "the real evaluation declares without a redeclaration error, got {committed:?}"
            );
            assert_eq!(
                committed["result"]["result"]["type"], "undefined",
                "declaration statements complete with undefined"
            );

            let value = protocol(
                context,
                state,
                intrinsics,
                "Runtime.evaluate",
                serde_json::json!({"expression": "eager_c"}),
            );
            assert_eq!(value["result"]["result"]["type"], "function");
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
                serde_json::json!({"objectId": "fusor:999999"}),
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
            assert!(
                response["result"].get("internalProperties").is_none(),
                "accessor-only listings omit internalProperties entirely (V8 omits the key)"
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
                serde_json::json!({"expression": "globalThis.__fusor_scope_test = 1"}),
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
                names.contains(&"__fusor_scope_test"),
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
            assert_eq!(response["result"]["id"], "fusor");
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
            assert_eq!(rendered["className"], "Object", "V8-style className");
            assert!(
                rendered["objectId"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("fusor:")),
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
            assert_eq!(rendered["className"], "Function");
            assert!(
                rendered["description"]
                    .as_str()
                    .is_some_and(|text| text.contains("named")),
                "function descriptions include the name"
            );

            let native = evaluate(context, "Math.abs");
            let rendered = remote_object(
                context,
                &mut state.objects,
                intrinsics,
                &native,
                None,
                false,
            );
            assert!(
                rendered["description"].as_str().is_some_and(|text| text
                    .starts_with("function abs()")
                    && text.contains("[native code]")),
                "native function descriptions match V8's toString shape, got {rendered:?}"
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
                serde_json::json!({"type": "number", "value": 42.0, "description": "42"})
            );
        });
    }
}
