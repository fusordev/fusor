//! End-to-end CLI tests: run-path and REPL-path through the `qjs` binary.

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
        "qjs-cli-e2e-test-{}-{}-{tag}",
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

    let output = Command::new(env!("CARGO_BIN_EXE_qjs"))
        .arg("run")
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn qjs");
    assert!(
        output.status.success(),
        "qjs run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_path_failing_assertion_exits_non_zero() {
    let directory = temp_dir("run-fail");
    let _cleanup = Cleanup(directory.clone());
    write_diamond_fixture(&directory, "assert.strictEqual(b, 99, 'deliberate failure');");

    let output = Command::new(env!("CARGO_BIN_EXE_qjs"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn qjs");
    assert!(!output.status.success(), "failing assertion must exit non-zero");
}

#[test]
fn repl_evaluates_a_module_entry_with_a_static_import() {
    let directory = temp_dir("repl");
    let _cleanup = Cleanup(directory.clone());
    fs::write(directory.join("answer.mjs"), "export const answer = 42;").expect("write dep");

    let mut child = Command::new(env!("CARGO_BIN_EXE_qjs"))
        .arg("repl")
        .current_dir(&directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn qjs repl");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"import { answer } from './answer.mjs'; globalThis.a43 = answer + 1;\na43;\n.exit\n")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for repl");
    assert!(
        output.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("43"), "expected 43 in repl output: {stdout}");
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

    let output = Command::new(env!("CARGO_BIN_EXE_qjs"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn qjs");
    assert!(
        output.status.success(),
        "qjs run failed: {}",
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

    let output = Command::new(env!("CARGO_BIN_EXE_qjs"))
        .arg(directory.join("entry.mjs"))
        .output()
        .expect("spawn qjs");
    assert!(
        !output.status.success(),
        "a rejecting top-level await must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("boom"), "expected the rejection on stderr: {stderr}");
}
