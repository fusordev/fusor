//! End-to-end CLI tests: run-path and REPL-path through the `fusor` binary.

use std::{
    fs,
    io::{BufRead, BufReader, Read as _, Write as _},
    net::TcpStream,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    time::Duration,
};

/// A minimal RFC 6455 client: unmasked server frames are read as JSON text,
/// client frames are masked as the protocol requires.
struct WebSocketClient {
    stream: TcpStream,
}

impl WebSocketClient {
    fn connect(port: u16) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to inspector");
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("read timeout");
        write!(
            stream,
            "GET /devtools/page/fusor HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        )
        .expect("handshake write");
        let mut header = Vec::new();
        let mut byte = [0_u8; 1];
        while header.len() < 4 || !header.windows(4).any(|window| window == b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("handshake read");
            header.push(byte[0]);
        }
        assert!(
            header.starts_with(b"HTTP/1.1 101"),
            "expected a 101 upgrade, got {}",
            String::from_utf8_lossy(&header)
        );
        Self { stream }
    }

    fn send(&mut self, message: &serde_json::Value) {
        let payload = serde_json::to_vec(message).expect("encode frame");
        let mask = [0x11_u8, 0x22, 0x33, 0x44];
        let mut frame = vec![0x81];
        // The mask bit lives in the length byte: client frames are always
        // masked per RFC 6455.
        match payload.len() {
            length @ 0..=125 => frame.push(0x80 | length as u8),
            length @ 126..=65_535 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(length as u16).to_be_bytes());
            }
            length => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(length as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]));
        self.stream.write_all(&frame).expect("frame write");
    }

    /// Reads server frames until one matches the predicate; other frames are
    /// ignored. Server frames are unmasked.
    fn recv_until(&mut self, matches: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        loop {
            let mut header = [0_u8; 2];
            self.stream.read_exact(&mut header).expect("frame header");
            let length = match header[1] & 0x7F {
                126 => {
                    let mut extended = [0_u8; 2];
                    self.stream.read_exact(&mut extended).expect("extended length");
                    u16::from_be_bytes(extended) as usize
                }
                127 => {
                    let mut extended = [0_u8; 8];
                    self.stream.read_exact(&mut extended).expect("extended length");
                    u64::from_be_bytes(extended) as usize
                }
                length => length as usize,
            };
            let mut payload = vec![0_u8; length];
            self.stream.read_exact(&mut payload).expect("frame payload");
            if header[0] & 0x0F != 0x1 {
                continue;
            }
            let message: serde_json::Value =
                serde_json::from_slice(&payload).expect("decode frame");
            if matches(&message) {
                return message;
            }
        }
    }
}

/// Spawns `fusor repl --inspect=0`, waits for the inspector startup line on
/// stderr, and returns the bound port alongside the child.
fn spawn_inspected_repl() -> (std::process::Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg("repl")
        .arg("--inspect=0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fusor repl");
    let stderr = child.stderr.take().expect("stderr");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(port) = line
                .split("ws://127.0.0.1:")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .and_then(|port| port.parse::<u16>().ok())
            {
                let _ = sender.send(port);
            }
        }
    });
    let port = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the inspector must report its bound port");
    (child, port)
}

#[test]
fn repl_forwards_uncaught_errors_to_the_inspector() {
    // V8-aligned: a terminal entry that throws renders in the attached
    // DevTools console as a `Runtime.exceptionThrown` event, not only as a
    // stderr report.
    let (mut child, port) = spawn_inspected_repl();
    let mut client = WebSocketClient::connect(port);
    client.send(&serde_json::json!({"id": 1, "method": "Runtime.enable", "params": {}}));
    client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.executionContextCreated")
    });

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"throw new Error('cdp-boom')\n")
        .expect("write the throwing entry");

    let event = client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.exceptionThrown")
    });
    let details = &event["params"]["exceptionDetails"];
    assert!(
        details["text"]
            .as_str()
            .is_some_and(|text| text.contains("cdp-boom")),
        "the event names the thrown message: {details}"
    );
    assert!(details["exceptionId"].is_u64());

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b".exit\n")
        .expect("write exit");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn repl_forwards_syntax_errors_to_the_inspector() {
    let (mut child, port) = spawn_inspected_repl();
    let mut client = WebSocketClient::connect(port);
    client.send(&serde_json::json!({"id": 1, "method": "Runtime.enable", "params": {}}));
    client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.executionContextCreated")
    });

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"1 +\n")
        .expect("write the syntax-error entry");

    let event = client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.exceptionThrown")
    });
    let details = &event["params"]["exceptionDetails"];
    assert!(
        details["text"].as_str().is_some_and(|text| !text.is_empty()),
        "the syntax error renders its diagnostic text: {details}"
    );
    assert_eq!(
        details["exception"]["subtype"], "error",
        "the exception renders as an error RemoteObject"
    );

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b".exit\n")
        .expect("write exit");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn console_evaluations_expand_map_entries_over_the_wire() {
    // The `[[Entries]]` handle must be expandable end to end: the internal
    // property carries a registered objectId, and getProperties on it
    // returns the internal#entry rows the frontend renders as `key => value`.
    let (mut child, port) = spawn_inspected_repl();
    let mut client = WebSocketClient::connect(port);
    client.send(&serde_json::json!({"id": 1, "method": "Runtime.enable", "params": {}}));
    client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.executionContextCreated")
    });

    client.send(&serde_json::json!({
        "id": 2,
        "method": "Runtime.evaluate",
        "params": {"expression": "new Map([['a', 1]])", "objectGroup": "console"},
    }));
    let evaluated = client.recv_until(|message| message.get("id").and_then(|id| id.as_i64()) == Some(2));
    let map_id = evaluated["result"]["result"]["objectId"]
        .as_str()
        .expect("map objectId")
        .to_owned();
    client.send(&serde_json::json!({
        "id": 3,
        "method": "Runtime.getProperties",
        "params": {"objectId": map_id, "ownProperties": true},
    }));
    let properties = client.recv_until(|message| message.get("id").and_then(|id| id.as_i64()) == Some(3));
    let internal = properties["result"]["internalProperties"]
        .as_array()
        .expect("internal properties");
    let entries = internal
        .iter()
        .find(|entry| entry["name"] == "[[Entries]]")
        .expect("[[Entries]] entry")
        .clone();
    assert_eq!(entries["value"]["description"], "Map(1)");
    let entries_id = entries["value"]["objectId"]
        .as_str()
        .expect("expandable entries objectId")
        .to_owned();

    client.send(&serde_json::json!({
        "id": 4,
        "method": "Runtime.getProperties",
        "params": {"objectId": entries_id},
    }));
    let expanded = client.recv_until(|message| message.get("id").and_then(|id| id.as_i64()) == Some(4));
    let rows = expanded["result"]["result"].as_array().expect("entry rows");
    assert_eq!(rows.len(), 1, "one live entry row: {rows:?}");
    assert_eq!(rows[0]["name"], "0");
    assert_eq!(rows[0]["value"]["subtype"], "internal#entry");
    let row_preview = rows[0]["value"]["preview"]["properties"]
        .as_array()
        .expect("row preview");
    assert_eq!(row_preview[0]["name"], "key");
    assert_eq!(row_preview[0]["value"], "a");
    assert_eq!(row_preview[1]["name"], "value");
    assert_eq!(row_preview[1]["value"], "1");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b".exit\n")
        .expect("write exit");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn console_evaluations_emit_exception_thrown_events() {
    // V8-aligned: a DevTools-console evaluation failure surfaces twice —
    // the response carries exceptionDetails and an exceptionThrown event
    // renders the red console entry. This is the `Runtime.evaluate` path
    // the frontend uses, distinct from the terminal REPL path above.
    let (mut child, port) = spawn_inspected_repl();
    let mut client = WebSocketClient::connect(port);
    client.send(&serde_json::json!({"id": 1, "method": "Runtime.enable", "params": {}}));
    client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.executionContextCreated")
    });

    client.send(&serde_json::json!({
        "id": 2,
        "method": "Runtime.evaluate",
        "params": {"expression": "1 +", "replMode": true, "userGesture": true},
    }));
    let event = client.recv_until(|message| {
        message.get("method").and_then(|method| method.as_str())
            == Some("Runtime.exceptionThrown")
    });
    let details = &event["params"]["exceptionDetails"];
    assert!(
        details["text"].as_str().is_some_and(|text| !text.is_empty()),
        "the event renders the syntax diagnostic: {details}"
    );

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b".exit\n")
        .expect("write exit");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "fusor-cli-e2e-test-{}-{}-{tag}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).expect("create temp dir");
    directory
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes a static diamond graph (entry -> b, c -> d) plus a dynamic import
/// and a `node:assert` builtin import into `directory`.
fn write_diamond_fixture(directory: &std::path::Path, assertion: &str) {
    fs::write(
        directory.join("d.mjs"),
        "globalThis.dEvaluations = (globalThis.dEvaluations ?? 0) + 1;\nexport const d = 1;",
    )
    .expect("write d");
    fs::write(
        directory.join("b.mjs"),
        "import { d } from './d.mjs';\nexport const b = d + 1;",
    )
    .expect("write b");
    fs::write(
        directory.join("c.mjs"),
        "import { d } from './d.mjs';\nexport const c = d + 2;",
    )
    .expect("write c");
    fs::write(
        directory.join("entry.mjs"),
        format!(
            "import {{ b }} from './b.mjs';\n\
             import {{ c }} from './c.mjs';\n\
             import assert from 'node:assert';\n\
             import('./d.mjs').then(({{ d }}) => {{ assert.strictEqual(b + c + d, 6); }});\n\
             assert.strictEqual(globalThis.dEvaluations, 1, 'd must evaluate once');\n\
             {assertion}"
        ),
    )
    .expect("write entry");
}

#[test]
fn run_path_executes_a_static_and_dynamic_graph() {
    let directory = temp_dir("run");
    let _cleanup = Cleanup(directory.clone());
    write_diamond_fixture(&directory, "");

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg("run")
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn fusor");
    assert!(
        output.status.success(),
        "fusor run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_path_failing_assertion_exits_non_zero() {
    let directory = temp_dir("run-fail");
    let _cleanup = Cleanup(directory.clone());
    write_diamond_fixture(
        &directory,
        "assert.strictEqual(b, 99, 'deliberate failure');",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn fusor");
    assert!(
        !output.status.success(),
        "failing assertion must exit non-zero"
    );
}

#[test]
fn repl_evaluates_a_module_entry_with_a_static_import() {
    let directory = temp_dir("repl");
    let _cleanup = Cleanup(directory.clone());
    fs::write(directory.join("answer.mjs"), "export const answer = 42;").expect("write dep");

    let mut child = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg("repl")
        .current_dir(&directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fusor repl");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(
            b"import { answer } from './answer.mjs'; globalThis.a43 = answer + 1;\na43;\n.exit\n",
        )
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("43"),
        "expected 43 in repl output: {stdout}"
    );
}

#[test]
fn repl_drives_the_host_loop_for_set_immediate() {
    // The REPL runs on the host event loop (§6): `Fusor.ops` timer ops work
    // per entry turn — the immediate callback fires before the next entry
    // evaluates.
    let mut child = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fusor repl");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(
            b"Fusor.ops.op_set_immediate(function () { globalThis.immediate = 7; });\nimmediate;\n.exit\n",
        )
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("7"),
        "expected 7 in repl output: {stdout}"
    );
}

#[test]
fn run_path_drives_the_host_loop_for_set_immediate() {
    // The module runner drives host-loop turns after evaluation: an
    // `op_set_immediate` callback scheduled at the top level fires before
    // the process exits.
    let directory = temp_dir("immediate");
    let _cleanup = Cleanup(directory.clone());
    fs::write(
        directory.join("entry.mjs"),
        "Fusor.ops.op_set_immediate(function () { print(42); });\n",
    )
    .expect("write entry");

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn fusor");
    assert!(
        output.status.success(),
        "fusor run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "expected 42 on stdout: {stdout}"
    );
}

#[test]
fn run_path_waits_real_time_for_pending_timers() {
    // A delayed timer keeps the process alive: the runner sleeps until the
    // deadline instead of advancing the virtual clock instantly (§6.3). The
    // callback measures its own latency through `op_core_now`, so parallel
    // test load cannot mask the wait.
    let directory = temp_dir("timeout");
    let _cleanup = Cleanup(directory.clone());
    fs::write(
        directory.join("entry.mjs"),
        "var t0 = Fusor.ops.op_core_now();\n\
         Fusor.ops.op_set_timeout(function () {\n\
             print(Fusor.ops.op_core_now() - t0);\n\
         }, 500);\n",
    )
    .expect("write entry");

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn fusor");
    assert!(
        output.status.success(),
        "fusor run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let observed: f64 = stdout
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("expected the timer latency on stdout: {stdout}"));
    assert!(
        observed >= 400.0,
        "the timer must fire after its delay, not instantly (observed {observed} ms)"
    );
}

#[test]
fn run_path_top_level_await_observes_the_awaited_value() {
    let directory = temp_dir("tla");
    let _cleanup = Cleanup(directory.clone());
    // The awaited value is only observable after the module's asynchronous
    // evaluation completes; the assertion inside the module body is the
    // oracle that the CLI drained it before exiting.
    fs::write(
        directory.join("entry.mjs"),
        "import assert from 'node:assert';\n\
         const x = await Promise.resolve(41);\n\
         assert.strictEqual(x + 1, 42, 'the awaited value must be observed after evaluation');",
    )
    .expect("write entry");

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn fusor");
    assert!(
        output.status.success(),
        "fusor run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_path_rejecting_top_level_await_exits_non_zero() {
    let directory = temp_dir("tla-reject");
    let _cleanup = Cleanup(directory.clone());
    fs::write(
        directory.join("entry.mjs"),
        "await Promise.reject(new Error('boom'));\n",
    )
    .expect("write entry");

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn fusor");
    assert!(
        !output.status.success(),
        "a rejecting top-level await must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("boom"),
        "expected the rejection on stderr: {stderr}"
    );
    assert!(
        stderr.ends_with('\n'),
        "the report must end with a newline: {stderr:?}"
    );
}

#[test]
fn repl_fires_timers_while_waiting_for_input() {
    // V8 semantics: the JS thread stays responsive while the line editor
    // waits — a delayed timer fires on schedule even when no entry arrives.
    let mut child = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fusor repl");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin
            .write_all(
                b"Fusor.ops.op_set_timeout(function () { print('fired'); }, 300);\n",
            )
            .expect("write the timer entry");
        stdin.flush().expect("flush stdin");
    }
    // The REPL now waits for the next line; the timer must fire during the
    // wait, not after the next entry. Poll stdout until the callback's
    // output appears — a fixed sleep window is load-sensitive under
    // parallel test runs.
    let stdout = child.stdout.take().expect("stdout");
    let lines: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let collector = std::sync::Arc::clone(&lines);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            collector.lock().expect("collector").push(line);
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if lines
            .lock()
            .expect("collector")
            .iter()
            .any(|line| line.contains("fired"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the timer must fire while the REPL waits for input: {:?}",
            lines.lock().expect("collector")
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b".exit\n")
        .expect("write exit");
    let output = child.wait().expect("wait for repl");
    assert!(
        output.success(),
        "repl failed"
    );
}

#[test]
fn script_path_prints_through_the_overlay_shim() {
    // The CLI installs no host `print` global (§9): the CLI overlay's init
    // script shims `print` over `Fusor.ops.op_core_print`, so bare `print`
    // reaches stdout through the installable sink.
    let directory = temp_dir("print");
    let _cleanup = Cleanup(directory.clone());
    fs::write(directory.join("print.js"), "print(1 + 2 * 3);\n").expect("write script");

    let output = Command::new(env!("CARGO_BIN_EXE_fusor"))
        .arg("--script")
        .arg(directory.join("print.js"))
        .output()
        .expect("spawn fusor");
    assert!(
        output.status.success(),
        "fusor --script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("7"),
        "expected 7 on stdout: {stdout}"
    );
}
