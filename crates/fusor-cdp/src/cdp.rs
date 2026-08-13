//! Chrome DevTools Protocol transport: loopback HTTP discovery, WebSocket
//! framing, and the shared debugger session.
//!
//! The engine is runtime-local and synchronous. This module consequently keeps
//! network I/O on OS threads and exchanges JSON-only messages with the owning
//! REPL task. The debugger hook itself owns pausing and resuming at verified VM
//! instruction boundaries, so no engine value crosses a thread boundary.

use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use fusor_runtime::{Context, DebugExecutionSnapshot, DebuggerHook};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tokio::sync::mpsc as tokio_mpsc;

const TARGET_ID: &str = "fusor";
const WEBSOCKET_PATH: &str = "/devtools/page/fusor";
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// A protocol request that requires the runtime-owning task.
pub struct EngineRequest {
    pub message: Value,
    pub response: mpsc::Sender<Value>,
}

/// Shared state for the CDP transport and the engine debugger hook.
pub struct DebugSession {
    engine_sender: Option<tokio_mpsc::UnboundedSender<EngineRequest>>,
    state: Mutex<DebugState>,
    resume: Condvar,
    pause_requested: AtomicBool,
    /// Whether the runtime-owning task is servicing an engine-bound CDP
    /// request. The debugger hook suppresses pauses while this is set: a
    /// breakpoint reached inside a console evaluation would otherwise
    /// deadlock the request channel.
    servicing_engine_request: AtomicBool,
    next_breakpoint: AtomicU64,
    next_exception: AtomicU64,
    next_client: AtomicU64,
}

struct DebugState {
    paused: bool,
    breakpoints: Vec<Breakpoint>,
    step_mode: Option<StepMode>,
    paused_depth: usize,
    pending_pause: Option<DebugExecutionSnapshot>,
    debugger_enabled: bool,
    scripts: HashMap<String, Script>,
    event_sender: Option<mpsc::Sender<OutboundFrame>>,
    active_client: Option<u64>,
}

enum StepMode {
    Into,
    Over,
    Out,
}

struct Breakpoint {
    id: String,
    url: String,
    line: u64,
    column: Option<u64>,
}

#[derive(Clone)]
struct Script {
    id: String,
    url: String,
    source: String,
}

enum OutboundFrame {
    Json(Value),
    Pong(Vec<u8>),
}

impl DebugSession {
    pub fn without_engine() -> Arc<Self> {
        Arc::new(Self {
            engine_sender: None,
            state: Mutex::new(DebugState {
                paused: false,
                breakpoints: Vec::new(),
                step_mode: None,
                paused_depth: 0,
                pending_pause: None,
                debugger_enabled: false,
                scripts: HashMap::new(),
                event_sender: None,
                active_client: None,
            }),
            resume: Condvar::new(),
            pause_requested: AtomicBool::new(false),
            servicing_engine_request: AtomicBool::new(false),
            next_breakpoint: AtomicU64::new(1),
            next_exception: AtomicU64::new(1),
            next_client: AtomicU64::new(1),
        })
    }

    #[must_use]
    pub fn new(engine_sender: tokio_mpsc::UnboundedSender<EngineRequest>) -> Arc<Self> {
        Arc::new(Self {
            engine_sender: Some(engine_sender),
            state: Mutex::new(DebugState {
                paused: false,
                breakpoints: Vec::new(),
                step_mode: None,
                paused_depth: 0,
                pending_pause: None,
                debugger_enabled: false,
                scripts: HashMap::new(),
                event_sender: None,
                active_client: None,
            }),
            resume: Condvar::new(),
            pause_requested: AtomicBool::new(false),
            servicing_engine_request: AtomicBool::new(false),
            next_breakpoint: AtomicU64::new(1),
            next_exception: AtomicU64::new(1),
            next_client: AtomicU64::new(1),
        })
    }

    pub fn request_initial_pause(&self) {
        self.pause_requested.store(true, Ordering::Release);
    }

    fn attach_client(&self, sender: mpsc::Sender<OutboundFrame>) -> u64 {
        let client_id = self.next_client.fetch_add(1, Ordering::Relaxed);
        let mut state = lock_state(&self.state);
        state.event_sender = Some(sender.clone());
        state.active_client = Some(client_id);
        if state.debugger_enabled {
            for script in state.scripts.values() {
                let _ = sender.send(OutboundFrame::Json(script_parsed(script)));
            }
            if let Some(snapshot) = &state.pending_pause {
                let _ = sender.send(OutboundFrame::Json(paused_event(snapshot)));
            }
        }
        self.resume.notify_all();
        client_id
    }

    pub(crate) fn detach_client(&self, client_id: u64) {
        let mut state = lock_state(&self.state);
        if state.active_client != Some(client_id) {
            return;
        }
        state.event_sender = None;
        state.active_client = None;
        state.paused = false;
        state.pending_pause = None;
        self.resume.notify_all();
    }

    fn handle_protocol(&self, message: Value) -> Value {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        match method {
            "Runtime.enable" => {
                self.emit(json!({
                    "method": "Runtime.executionContextCreated",
                    "params": {"context": {"id": 1, "origin": "", "name": "fusor", "auxData": {"isDefault": true}}}
                }));
                protocol_result(id, json!({}))
            }
            "Runtime.disable" => protocol_result(id, json!({})),
            "Debugger.disable" => {
                lock_state(&self.state).debugger_enabled = false;
                protocol_result(id, json!({}))
            }
            "Runtime.runIfWaitingForDebugger" => {
                let mut state = lock_state(&self.state);
                state.paused = false;
                state.pending_pause = None;
                self.pause_requested.store(false, Ordering::Release);
                self.resume.notify_all();
                protocol_result(id, json!({}))
            }
            "Debugger.enable" => {
                let mut state = lock_state(&self.state);
                state.debugger_enabled = true;
                if let Some(sender) = &state.event_sender {
                    for script in state.scripts.values() {
                        let _ = sender.send(OutboundFrame::Json(script_parsed(script)));
                    }
                    if let Some(snapshot) = &state.pending_pause {
                        let _ = sender.send(OutboundFrame::Json(paused_event(snapshot)));
                    }
                }
                protocol_result(id, json!({"debuggerId": TARGET_ID}))
            }
            "Debugger.pause" => {
                self.pause_requested.store(true, Ordering::Release);
                protocol_result(id, json!({}))
            }
            "Debugger.resume" => {
                let mut state = lock_state(&self.state);
                state.paused = false;
                state.pending_pause = None;
                state.step_mode = None;
                self.resume.notify_all();
                protocol_result(id, json!({}))
            }
            "Debugger.stepInto" => {
                let mut state = lock_state(&self.state);
                state.paused = false;
                state.pending_pause = None;
                state.step_mode = Some(StepMode::Into);
                self.resume.notify_all();
                protocol_result(id, json!({}))
            }
            "Debugger.stepOver" => {
                let mut state = lock_state(&self.state);
                state.paused = false;
                state.pending_pause = None;
                state.step_mode = Some(StepMode::Over);
                self.resume.notify_all();
                protocol_result(id, json!({}))
            }
            "Debugger.stepOut" => {
                let mut state = lock_state(&self.state);
                state.paused = false;
                state.pending_pause = None;
                state.step_mode = Some(StepMode::Out);
                self.resume.notify_all();
                protocol_result(id, json!({}))
            }
            "Debugger.setBreakpointByUrl" => self.set_breakpoint_by_url(id, &params),
            "Debugger.removeBreakpoint" => self.remove_breakpoint(id, &params),
            "Debugger.getScriptSource" => self.get_script_source(id, &params),
            "Runtime.compileScript" => self.compile_script_request(id, &params),
            method if is_engine_bound_method(method) => self.forward_to_engine(id, message),
            _ => protocol_error(id, -32601, &format!("unsupported CDP method: {method}")),
        }
    }

    /// Handles one engine-bound protocol request on the runtime-owning task.
    ///
    /// The servicing flag suppresses debugger pauses for the duration of the
    /// handler: evaluation triggered by inspection (including accessor
    /// getters and Proxy traps) must not pause, or the engine task would
    /// block inside the debugger hook while the transport task waits for
    /// this response. `throwOnSideEffect` requests (the frontend's eager
    /// evaluation) additionally raise the caller's print-suppression flag so
    /// host output stays silent during a side-effect probe.
    pub fn handle_engine_protocol(
        &self,
        context: &mut Context<'_>,
        state: &mut crate::inspector::InspectState,
        intrinsics: &crate::inspector::InspectIntrinsics,
        suppress_print: &std::cell::Cell<bool>,
        message: Value,
    ) -> Value {
        if lock_state(&self.state).paused {
            return protocol_error(
                message.get("id").cloned().unwrap_or(Value::Null),
                -32000,
                "runtime evaluation is unavailable while the debugger is paused",
            );
        }
        let suppress = message
            .get("params")
            .and_then(|params| params.get("throwOnSideEffect"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let previous = suppress_print.replace(suppress);
        self.servicing_engine_request.store(true, Ordering::Release);
        let response = crate::inspector::handle_engine_request(context, state, intrinsics, message);
        self.servicing_engine_request
            .store(false, Ordering::Release);
        suppress_print.set(previous);
        response
    }

    fn compile_script_request(&self, id: Value, params: &Value) -> Value {
        let Some(expression) = params.get("expression").and_then(Value::as_str) else {
            return protocol_error(
                id,
                -32602,
                "Runtime.compileScript requires params.expression",
            );
        };
        let source_name = params
            .get("sourceURL")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            // The frontend sends an empty sourceURL for console syntax
            // checks; the engine requires a non-empty display name.
            .unwrap_or("console");
        match fusor::compile_script(expression, source_name, fusor::ScriptLimits::default()) {
            Ok(_) => protocol_result(id, json!({"scriptId": source_name})),
            Err(error) => {
                let (text, line, column) = script_compile_error_position(&error, expression);
                let exception_id = self.next_exception.fetch_add(1, Ordering::Relaxed);
                protocol_result(
                    id,
                    json!({
                        "scriptId": source_name,
                        "exceptionDetails": {
                            "exceptionId": exception_id,
                            "text": text,
                            "lineNumber": line,
                            "columnNumber": column,
                            "url": source_name,
                            "exception": {"type": "object", "subtype": "error", "description": text},
                        }
                    }),
                )
            }
        }
    }

    fn forward_to_engine(&self, id: Value, message: Value) -> Value {
        if lock_state(&self.state).paused {
            return protocol_error(
                id,
                -32000,
                "Runtime.evaluate is unavailable while the debugger is paused",
            );
        }
        let (response_sender, response_receiver) = mpsc::channel();
        let Some(engine_sender) = &self.engine_sender else {
            return protocol_error(
                id,
                -32000,
                "Runtime.evaluate is unavailable for this target",
            );
        };
        if engine_sender
            .send(EngineRequest {
                message,
                response: response_sender,
            })
            .is_err()
        {
            return protocol_error(id, -32000, "debug target is unavailable");
        }
        response_receiver
            .recv()
            .unwrap_or_else(|_| protocol_error(id, -32000, "debug target stopped responding"))
    }

    fn set_breakpoint_by_url(&self, id: Value, params: &Value) -> Value {
        let Some(url) = params.get("url").and_then(Value::as_str) else {
            return protocol_error(
                id,
                -32602,
                "Debugger.setBreakpointByUrl requires params.url",
            );
        };
        let Some(line) = params.get("lineNumber").and_then(Value::as_u64) else {
            return protocol_error(
                id,
                -32602,
                "Debugger.setBreakpointByUrl requires params.lineNumber",
            );
        };
        let column = params.get("columnNumber").and_then(Value::as_u64);
        let breakpoint_id = format!(
            "breakpoint:{}",
            self.next_breakpoint.fetch_add(1, Ordering::Relaxed)
        );
        let mut state = lock_state(&self.state);
        state.breakpoints.push(Breakpoint {
            id: breakpoint_id.clone(),
            url: url.to_owned(),
            line,
            column,
        });
        let locations = state
            .scripts
            .values()
            .filter(|script| script.url == url)
            .flat_map(|script| breakpoint_locations(script, line, column))
            .collect::<Vec<_>>();
        protocol_result(
            id,
            json!({"breakpointId": breakpoint_id, "locations": locations}),
        )
    }

    fn remove_breakpoint(&self, id: Value, params: &Value) -> Value {
        let Some(breakpoint_id) = params.get("breakpointId").and_then(Value::as_str) else {
            return protocol_error(
                id,
                -32602,
                "Debugger.removeBreakpoint requires params.breakpointId",
            );
        };
        let mut state = lock_state(&self.state);
        state
            .breakpoints
            .retain(|breakpoint| breakpoint.id != breakpoint_id);
        protocol_result(id, json!({}))
    }

    fn get_script_source(&self, id: Value, params: &Value) -> Value {
        let Some(script_id) = params.get("scriptId").and_then(Value::as_str) else {
            return protocol_error(
                id,
                -32602,
                "Debugger.getScriptSource requires params.scriptId",
            );
        };
        let state = lock_state(&self.state);
        let Some(script) = state.scripts.get(script_id) else {
            return protocol_error(id, -32000, "unknown scriptId");
        };
        protocol_result(id, json!({"scriptSource": script.source}))
    }

    fn emit(&self, message: Value) {
        if let Some(sender) = lock_state(&self.state).event_sender.as_ref() {
            let _ = sender.send(OutboundFrame::Json(message));
        }
    }

    /// Queues one protocol event for the attached client, when present.
    pub fn emit_event(&self, message: Value) {
        self.emit(message);
    }
}

impl DebuggerHook for DebugSession {
    fn on_instruction(&self, snapshot: &DebugExecutionSnapshot) {
        let current = snapshot.location();
        let script = Script {
            id: current.source_name().to_owned(),
            url: current.source_name().to_owned(),
            source: current.source_text().to_owned(),
        };
        let mut state = lock_state(&self.state);
        let is_new_script = !state.scripts.contains_key(&script.id);
        if is_new_script {
            state.scripts.insert(script.id.clone(), script.clone());
            if state.debugger_enabled {
                if let Some(sender) = &state.event_sender {
                    let _ = sender.send(OutboundFrame::Json(script_parsed(&script)));
                }
            }
        }
        // Line and column computation only runs when a breakpoint could
        // match; console evaluation crosses thousands of instruction
        // boundaries and must not pay UTF-16 scanning per step.
        let breakpoint_hit = breakpoint_hit(
            &state.breakpoints,
            current.source_name(),
            current.source_text(),
            current.source_span().start() as usize,
        );
        let stack_depth = snapshot.stack().len();
        let step_ready = match state.step_mode {
            Some(StepMode::Into) => true,
            Some(StepMode::Over) => stack_depth <= state.paused_depth,
            Some(StepMode::Out) => stack_depth < state.paused_depth,
            None => false,
        };
        let pause_requested = self.pause_requested.swap(false, Ordering::AcqRel);
        let should_pause = !self.servicing_engine_request.load(Ordering::Acquire)
            && (pause_requested
                || (state.event_sender.is_some()
                    && (snapshot.is_debugger_statement() || breakpoint_hit || step_ready)));
        if !should_pause {
            return;
        }
        state.paused = true;
        state.paused_depth = stack_depth;
        state.step_mode = None;
        state.pending_pause = Some(snapshot.clone());
        if state.debugger_enabled {
            if let Some(sender) = &state.event_sender {
                let _ = sender.send(OutboundFrame::Json(paused_event(snapshot)));
            }
        }
        while state.paused {
            state = match self.resume.wait(state) {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
        }
        if state.debugger_enabled {
            if let Some(sender) = &state.event_sender {
                let _ = sender.send(OutboundFrame::Json(
                    json!({"method": "Debugger.resumed", "params": {}}),
                ));
            }
        }
    }
}

/// Starts the loopback-only CDP discovery and WebSocket server.
pub fn start(port: u16, session: Arc<DebugSession>) -> io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || accept_loop(listener, session));
    Ok(port)
}

fn accept_loop(listener: TcpListener, session: Arc<DebugSession>) {
    for stream in listener.incoming().flatten() {
        let session = Arc::clone(&session);
        thread::spawn(move || {
            let _ = serve_connection(stream, session);
        });
    }
}

fn serve_connection(mut stream: TcpStream, session: Arc<DebugSession>) -> io::Result<()> {
    let request = read_http_request(&mut stream)?;
    let Some((method, path, headers)) = parse_http_request(&request) else {
        return write_http_response(&mut stream, 400, "text/plain", "bad request");
    };
    if method != "GET" {
        return write_http_response(&mut stream, 405, "text/plain", "method not allowed");
    }
    let port = stream.local_addr()?.port();
    match path {
        "/json/version" => write_json_response(&mut stream, version_metadata(port)),
        "/json" | "/json/list" => write_json_response(&mut stream, json!([target_metadata(port)])),
        WEBSOCKET_PATH
            if headers
                .get("upgrade")
                .is_some_and(|value| value.eq_ignore_ascii_case("websocket")) =>
        {
            upgrade_websocket(&mut stream, &headers)?;
            serve_websocket(stream, session)
        }
        _ => write_http_response(&mut stream, 404, "text/plain", "not found"),
    }
}

fn version_metadata(port: u16) -> Value {
    json!({
        "Browser": "Project Fusor",
        "Protocol-Version": "1.3",
        "User-Agent": "Fusor with Chrome DevTools Protocol",
        "V8-Version": "v0-fusor",
        "webSocketDebuggerUrl": websocket_url(port),
    })
}

fn target_metadata(port: u16) -> Value {
    json!({
        "description": "QuickJS Rust engine",
        "devtoolsFrontendUrl": format!("/devtools/inspector.html?ws=127.0.0.1:{port}{WEBSOCKET_PATH}"),
        "id": TARGET_ID,
        "title": "fusor",
        "type": "node",
        "url": "fusor://repl",
        "webSocketDebuggerUrl": websocket_url(port),
    })
}

fn websocket_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}{WEBSOCKET_PATH}")
}

fn read_http_request(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        if bytes.len() >= MAX_HTTP_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP header too large",
            ));
        }
        let mut next = [0_u8; 1024];
        let read = stream.read(&mut next)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete HTTP request",
            ));
        }
        bytes.extend_from_slice(&next[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(bytes);
        }
    }
}

fn parse_http_request(request: &[u8]) -> Option<(&str, &str, HashMap<String, String>)> {
    let text = std::str::from_utf8(request).ok()?;
    let mut lines = text.split("\r\n");
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?.split('?').next()?;
    if parts.next().is_none() {
        return None;
    }
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Some((method, path, headers))
}

fn write_json_response(stream: &mut TcpStream, value: Value) -> io::Result<()> {
    let body = serde_json::to_string(&value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_http_response(stream, 200, "application/json; charset=UTF-8", &body)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

fn upgrade_websocket(stream: &mut TcpStream, headers: &HashMap<String, String>) -> io::Result<()> {
    let key = headers
        .get("sec-websocket-key")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing WebSocket key"))?;
    let mut sha1 = Sha1::new();
    sha1.update(key.as_bytes());
    sha1.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = STANDARD.encode(sha1.finalize());
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(response.as_bytes())
}

fn serve_websocket(stream: TcpStream, session: Arc<DebugSession>) -> io::Result<()> {
    let (outbound_sender, outbound_receiver) = mpsc::channel();
    let client_id = session.attach_client(outbound_sender.clone());
    let mut writer = stream.try_clone()?;
    let writer_handle = thread::spawn(move || {
        while let Ok(frame) = outbound_receiver.recv() {
            let result = match frame {
                OutboundFrame::Json(message) => write_websocket_json(&mut writer, &message),
                OutboundFrame::Pong(payload) => write_websocket_frame(&mut writer, 0xA, &payload),
            };
            if result.is_err() {
                break;
            }
        }
    });
    let mut reader = stream;
    let result = loop {
        let Some(frame) = read_websocket_frame(&mut reader)? else {
            break Ok(());
        };
        match frame {
            WebSocketFrame::Close => break Ok(()),
            WebSocketFrame::Ping(payload) => {
                if outbound_sender.send(OutboundFrame::Pong(payload)).is_err() {
                    break Ok(());
                }
            }
            WebSocketFrame::Text(text) => {
                let response = match serde_json::from_str::<Value>(&text) {
                    Ok(message) => session.handle_protocol(message),
                    Err(error) => {
                        protocol_error(Value::Null, -32700, &format!("invalid JSON: {error}"))
                    }
                };
                if outbound_sender.send(OutboundFrame::Json(response)).is_err() {
                    break Ok(());
                }
            }
        }
    };
    session.detach_client(client_id);
    drop(outbound_sender);
    let _ = writer_handle.join();
    result
}

/// Methods whose handlers need live engine values and therefore run on the
/// runtime-owning task through the engine request channel.
fn is_engine_bound_method(method: &str) -> bool {
    matches!(
        method,
        "Runtime.evaluate"
            | "Runtime.callFunctionOn"
            | "Runtime.getProperties"
            | "Runtime.releaseObject"
            | "Runtime.releaseObjectGroup"
            | "Runtime.globalLexicalScopeNames"
            | "Runtime.getIsolateId"
            | "Runtime.getHeapUsage"
    )
}

/// Renders a compile failure as `(text, line, column)`, extracting the
/// frontend diagnostic span when one is attached.
pub(crate) fn script_compile_error_position(
    error: &fusor::ScriptCompileError,
    source: &str,
) -> (String, u64, u64) {
    match error {
        fusor::ScriptCompileError::Frontend(frontend) => {
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

pub(crate) fn protocol_result(id: Value, result: Value) -> Value {
    json!({"id": id, "result": result})
}

pub(crate) fn protocol_error(id: Value, code: i64, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

/// Tests whether any breakpoint matches the given verified source position.
///
/// The position is described by byte offset so the line/column conversion
/// (UTF-16 scanning) only runs when at least one breakpoint exists.
fn breakpoint_hit(
    breakpoints: &[Breakpoint],
    source_name: &str,
    source_text: &str,
    span_start: usize,
) -> bool {
    if breakpoints.is_empty() {
        return false;
    }
    let (line, column) = source_position(source_text, span_start);
    breakpoints.iter().any(|breakpoint| {
        breakpoint.url == source_name
            && breakpoint.line == line
            && breakpoint.column.is_none_or(|expected| expected <= column)
    })
}

fn script_parsed(script: &Script) -> Value {
    json!({
        "method": "Debugger.scriptParsed",
        "params": {
            "scriptId": script.id,
            "url": script.url,
            "startLine": 0,
            "startColumn": 0,
            "endLine": source_position(&script.source, script.source.len()).0,
            "endColumn": source_position(&script.source, script.source.len()).1,
            "executionContextId": 1,
            "hash": "",
            "isLiveEdit": false,
            "sourceMapURL": "",
            "hasSourceURL": false,
            "length": script.source.len(),
            "scriptLanguage": "JavaScript"
        }
    })
}

fn paused_event(snapshot: &DebugExecutionSnapshot) -> Value {
    let call_frames = snapshot
        .stack()
        .iter()
        .enumerate()
        .map(|(index, location)| {
            let (line, column) = source_position(location.source_text(), location.source_span().start() as usize);
            json!({
                "callFrameId": index.to_string(),
                "functionName": "",
                "functionLocation": {"scriptId": location.source_name(), "lineNumber": line, "columnNumber": column},
                "location": {"scriptId": location.source_name(), "lineNumber": line, "columnNumber": column},
                "url": location.source_name(),
                "scopeChain": [],
                "this": {"type": "undefined"}
            })
        })
        .collect::<Vec<_>>();
    json!({
        "method": "Debugger.paused",
        "params": {"callFrames": call_frames, "reason": "other", "hitBreakpoints": []}
    })
}

fn breakpoint_locations(script: &Script, line: u64, column: Option<u64>) -> Vec<Value> {
    let mut lines = script.source.lines().enumerate();
    let Some((index, text)) = lines.nth(line as usize) else {
        return Vec::new();
    };
    column
        .is_none_or(|column| column <= utf16_len(text))
        .then(|| {
            json!({"scriptId": script.id, "lineNumber": index as u64, "columnNumber": column.unwrap_or(0)})
        })
        .into_iter()
        .collect()
}

pub(crate) fn source_position(source: &str, byte_offset: usize) -> (u64, u64) {
    let offset = byte_offset.min(source.len());
    let prefix = source.get(..offset).unwrap_or_default();
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u64;
    let column_text = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, column)| column);
    (line, utf16_len(column_text))
}

fn utf16_len(value: &str) -> u64 {
    value.encode_utf16().count() as u64
}

enum WebSocketFrame {
    Close,
    Ping(Vec<u8>),
    Text(String),
}

fn read_websocket_frame(stream: &mut TcpStream) -> io::Result<Option<WebSocketFrame>> {
    let mut header = [0_u8; 2];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let finished = header[0] & 0x80 != 0;
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    if !finished || !masked {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported WebSocket frame",
        ));
    }
    let mut length = u64::from(header[1] & 0x7f);
    if length == 126 {
        let mut bytes = [0_u8; 2];
        stream.read_exact(&mut bytes)?;
        length = u64::from(u16::from_be_bytes(bytes));
    } else if length == 127 {
        let mut bytes = [0_u8; 8];
        stream.read_exact(&mut bytes)?;
        length = u64::from_be_bytes(bytes);
    }
    if length > MAX_FRAME_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WebSocket frame too large",
        ));
    }
    let mut mask = [0_u8; 4];
    stream.read_exact(&mut mask)?;
    let mut payload = vec![0_u8; length as usize];
    stream.read_exact(&mut payload)?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % mask.len()];
    }
    match opcode {
        0x1 => String::from_utf8(payload)
            .map(WebSocketFrame::Text)
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 text frame")),
        0x8 => Ok(Some(WebSocketFrame::Close)),
        0x9 => Ok(Some(WebSocketFrame::Ping(payload))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported WebSocket opcode",
        )),
    }
}

fn write_websocket_json(stream: &mut TcpStream, value: &Value) -> io::Result<()> {
    let text = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_websocket_frame(stream, 0x1, &text)
}

fn write_websocket_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | opcode);
    match payload.len() {
        length @ 0..=125 => header.push(length as u8),
        length @ 126..=65_535 => {
            header.push(126);
            header.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            header.push(127);
            header.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    stream.write_all(&header)?;
    stream.write_all(payload)
}

fn lock_state(state: &Mutex<DebugState>) -> std::sync::MutexGuard<'_, DebugState> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_websocket_upgrade_request() {
        let request = b"GET /devtools/page/fusor HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nSec-WebSocket-Key: abc\r\n\r\n";
        let (method, path, headers) = parse_http_request(request).expect("parse request");
        assert_eq!(method, "GET");
        assert_eq!(path, WEBSOCKET_PATH);
        assert_eq!(headers.get("upgrade"), Some(&"websocket".to_owned()));
    }

    #[test]
    fn source_positions_use_utf16_columns() {
        assert_eq!(source_position("a\u{1f600}b", 5), (0, 3));
        assert_eq!(source_position("x\ny", 2), (1, 0));
    }

    #[test]
    fn discovery_metadata_uses_the_cdp_websocket_path() {
        assert_eq!(
            version_metadata(9229)["webSocketDebuggerUrl"],
            websocket_url(9229)
        );
        assert_eq!(target_metadata(9229)["id"], TARGET_ID);
    }

    #[test]
    fn compile_script_reports_syntax_errors_without_an_engine() {
        let session = DebugSession::without_engine();
        let response = session.handle_protocol(json!({
            "id": 1,
            "method": "Runtime.compileScript",
            "params": {"expression": "function (", "sourceURL": "broken.js"},
        }));
        assert!(
            response["result"]["exceptionDetails"]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
        assert!(response["result"]["exceptionDetails"]["exceptionId"].is_u64());
    }

    #[test]
    fn compile_script_accepts_valid_scripts_without_an_engine() {
        let session = DebugSession::without_engine();
        let response = session.handle_protocol(json!({
            "id": 2,
            "method": "Runtime.compileScript",
            "params": {"expression": "1 + 1", "sourceURL": "ok.js"},
        }));
        assert_eq!(response["result"]["scriptId"], "ok.js");
        assert!(response["result"]["exceptionDetails"].is_null());
    }

    #[test]
    fn compile_script_tolerates_an_empty_source_url() {
        let session = DebugSession::without_engine();
        let response = session.handle_protocol(json!({
            "id": 3,
            "method": "Runtime.compileScript",
            "params": {"expression": "1 + 1", "sourceURL": ""},
        }));
        assert!(
            response["result"]["exceptionDetails"].is_null(),
            "an empty sourceURL must fall back to a valid name, got {response}"
        );
        assert_eq!(response["result"]["scriptId"], "console");
    }

    #[test]
    fn engine_bound_methods_forward_when_no_engine_is_attached() {
        let session = DebugSession::without_engine();
        for method in [
            "Runtime.evaluate",
            "Runtime.callFunctionOn",
            "Runtime.getProperties",
            "Runtime.releaseObject",
            "Runtime.releaseObjectGroup",
            "Runtime.globalLexicalScopeNames",
            "Runtime.getHeapUsage",
            "Runtime.getIsolateId",
        ] {
            let response = session.handle_protocol(json!({"id": 3, "method": method}));
            assert_eq!(
                response["error"]["code"], -32000,
                "{method} must route through the engine channel"
            );
        }
    }

    #[test]
    fn engine_request_servicing_flag_roundtrips() {
        let session = DebugSession::without_engine();
        assert!(!session.servicing_engine_request.load(Ordering::Acquire));
        session
            .servicing_engine_request
            .store(true, Ordering::Release);
        assert!(session.servicing_engine_request.load(Ordering::Acquire));
        session
            .servicing_engine_request
            .store(false, Ordering::Release);
        assert!(!session.servicing_engine_request.load(Ordering::Acquire));
    }

    #[test]
    fn breakpoint_matching_skips_work_without_breakpoints() {
        assert!(!breakpoint_hit(&[], "script.js", "1 + 1", 0));
    }

    #[test]
    fn breakpoint_matching_uses_utf16_lines_and_columns() {
        let breakpoints = [Breakpoint {
            id: "one".to_owned(),
            url: "script.js".to_owned(),
            line: 1,
            column: Some(2),
        }];
        assert!(breakpoint_hit(
            &breakpoints,
            "script.js",
            "first\n  second()",
            9,
        ));
        assert!(
            !breakpoint_hit(&breakpoints, "other.js", "first\n  second()", 9),
            "a different URL never matches"
        );
        assert!(
            !breakpoint_hit(&breakpoints, "script.js", "first\n  second()", 7),
            "columns before the breakpoint do not match"
        );
    }
}
