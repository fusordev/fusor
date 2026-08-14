//! End-to-end CLI tests: run-path and REPL-path through the `fusor` binary.

use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

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
    // wait, not after the next entry. The window is generous because the
    // suite spawns binaries under parallel load.
    std::thread::sleep(std::time::Duration::from_millis(1200));
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fired"),
        "the timer must fire while the REPL waits for input: stdout={stdout:?} stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
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
