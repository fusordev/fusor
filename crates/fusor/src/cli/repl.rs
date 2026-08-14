//! ESM REPL.
//!
//! A session owns one realm. Each entry is evaluated in one of two ways:
//!
//! - Entries without top-level `import`/`export` syntax are evaluated as
//!   classic scripts; their completion value is printed and successfully
//!   instantiated global bindings persist in the realm for later entries.
//! - Entries with module syntax are evaluated as modules named
//!   `file://<cwd>/__repl_entry_<n>.mjs`, so relative imports resolve against
//!   the current working directory through the [`NodeLikeResolver`]. Every
//!   single-line `import` statement from a successful module entry is
//!   accumulated into a session prefix that is prepended to later module
//!   entries, approximating the incrementally extended module environment of
//!   docs/MODULES.md's "ESM REPL".
//!
//! This is documented host sugar, not a spec module record: each module entry
//! gathers a fresh module graph (the facade is single-shot per call), so
//! imported modules are re-registered and re-evaluated on every module entry,
//! script entries cannot see module-scoped bindings, and only single-line
//! imports are accumulated.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    thread,
};

use fusor::{ModuleEvaluationError, ScriptEvaluationError, ScriptLimits, evaluate_preloaded_module_graph, evaluate_script};
use fusor_host::r#loop::HostLoop;
use fusor_host::ops::set_print_sink;
use fusor_host::overlay::{CoreOverlay, HostRuntime};
use tokio::sync::mpsc;

use crate::{cli::overlay::CliOverlay, cli::resolver::NodeLikeResolver, report_error};
use crate::cdp::{
    self as cdp,
    format::format_value,
    inspector,
};

/// Runs the REPL on stdin/stdout. Returns the process exit code.
///
/// Module entries load their static graph asynchronously on the caller's
/// Tokio runtime; script entries stay fully synchronous.
pub(crate) async fn run(inspect_port: Option<u16>, inspect_break: bool) -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => crate::cli::resolver::normalize_path(&cwd),
        Err(error) => {
            eprintln!("fusor: cannot determine the current directory: {error}");
            return 2;
        }
    };
    // The REPL assembles the same "core overlay + CLI overlay" host the run
    // path uses (§9 step 4); the CLI overlay's init script provides `print`.
    let mut host = match HostRuntime::builder()
        .with_overlay(CoreOverlay)
        .with_overlay(CliOverlay)
        .build()
    {
        Ok(host) => host,
        Err(error) => {
            eprintln!("fusor: cannot assemble the host runtime: {error}");
            return 2;
        }
    };
    let (debug_session, debug_requests) = if let Some(port) = inspect_port {
        let (engine_sender, engine_requests) = mpsc::unbounded_channel();
        let session = cdp::DebugSession::new(engine_sender);
        if inspect_break {
            session.request_initial_pause();
        }
        let debugger_hook: Arc<dyn fusor_runtime::DebuggerHook> = session.clone();
        host.runtime_mut().set_debugger_hook(debugger_hook);
        let bound_port = match cdp::start(port, Arc::clone(&session)) {
            Ok(port) => port,
            Err(error) => {
                eprintln!("fusor: cannot start CDP inspector on 127.0.0.1:{port}: {error}");
                return 2;
            }
        };
        eprintln!("fusor inspector listening on ws://127.0.0.1:{bound_port}/devtools/page/fusor");
        (Some(session), Some(engine_requests))
    } else {
        (None, None)
    };
    let mut host_loop = match host.into_loop() {
        Ok(host_loop) => host_loop,
        Err(error) => {
            eprintln!("fusor: cannot install the host event loop: {error}");
            return 2;
        }
    };
    let resolver = Rc::new(RefCell::new(NodeLikeResolver::new(
        cwd.clone(),
        vec!["fusor".to_owned()],
    )));
    let entry_prefix = format!("file://{}", cwd.display());

    eprintln!("fusor REPL (host sugar, not a spec module record). .exit or Ctrl-D to quit.");

    if let (Some(session), Some(debug_requests)) = (debug_session, debug_requests) {
        // The inspector path installs a suppression-gated print sink: eager
        // evaluation with `throwOnSideEffect` flips the flag so probes stay
        // silent (§7.5).
        let print_suppressed: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let flag = Rc::clone(&print_suppressed);
        let _ = set_print_sink(Box::new(move |line: &str| {
            if !flag.get() {
                println!("{line}");
            }
        }));
        return run_with_inspector(
            &mut host_loop,
            debug_requests,
            session,
            resolver,
            entry_prefix,
            print_suppressed,
        )
        .await;
    }

    let mut editor = match rustyline::DefaultEditor::new() {
        Ok(editor) => editor,
        Err(error) => {
            eprintln!("fusor: cannot initialize the line editor: {error}");
            return 1;
        }
    };
    let mut imports: Vec<String> = Vec::new();
    let mut entry_index = 0_u64;
    let mut pending = String::new();
    loop {
        let prompt = if pending.is_empty() { "fusor> " } else { "...> " };
        let line = match editor.readline(prompt) {
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
                line
            }
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => {
                println!();
                return 0;
            }
            Err(error) => {
                eprintln!("fusor: cannot read input: {error}");
                return 1;
            }
        };
        if pending.is_empty() && line.trim() == ".exit" {
            return 0;
        }
        pending.push_str(&line);
        pending.push('\n');
        if !braces_balanced(&pending) {
            continue;
        }
        let entry = std::mem::take(&mut pending);
        if entry.trim().is_empty() {
            continue;
        }
        entry_index += 1;
        evaluate_repl_entry(
            &mut host_loop,
            &entry,
            &resolver,
            &entry_prefix,
            &mut imports,
            entry_index,
            true,
        )
        .await;
        // The loop's uncaught/unhandled default paths request the exit code
        // (§7.2); honor the request and leave the session.
        if let Some(code) = host_loop.pending_exit_code() {
            return u8::try_from(code).unwrap_or(1);
        }
    }
}

/// Runs the REPL with CDP requests and stdin entries multiplexed on the owning
/// runtime task. The input thread transports only owned source text. The
/// caller installed the suppression-gated print sink (§9: `print` is the CLI
/// overlay's shim, not a host-installed global). Every engine interaction
/// (CDP protocol handling and entry evaluation) happens inside a host-loop
/// turn (§6).
async fn run_with_inspector(
    host_loop: &mut HostLoop,
    mut debug_requests: mpsc::UnboundedReceiver<cdp::EngineRequest>,
    session: Arc<cdp::DebugSession>,
    resolver: Rc<RefCell<NodeLikeResolver>>,
    entry_prefix: String,
    print_suppressed: Rc<Cell<bool>>,
) -> u8 {
    let (input_sender, mut input_receiver) = mpsc::unbounded_channel();
    thread::spawn(move || {
        let mut editor = match rustyline::DefaultEditor::new() {
            Ok(editor) => editor,
            Err(error) => {
                eprintln!("fusor: cannot initialize the line editor: {error}");
                let _ = input_sender.send(None);
                return;
            }
        };
        loop {
            match editor.readline("fusor> ") {
                Ok(line) => {
                    let _ = editor.add_history_entry(line.as_str());
                    if input_sender.send(Some(line)).is_err() {
                        return;
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted) => continue,
                Err(rustyline::error::ReadlineError::Eof) => {
                    let _ = input_sender.send(None);
                    return;
                }
                Err(error) => {
                    eprintln!("fusor: cannot read input: {error}");
                    let _ = input_sender.send(None);
                    return;
                }
            }
        }
    });

    let mut entry_index = 0_u64;
    let mut imports = Vec::new();
    let mut pending = String::new();

    // CDP inspection state lives in one owner (this task); the engine-side
    // intrinsics are created inside a turn and held across turns.
    let inspect: Rc<RefCell<inspector::InspectState>> =
        Rc::new(RefCell::new(inspector::InspectState::new()));
    let intrinsics: Rc<RefCell<Option<inspector::InspectIntrinsics>>> =
        Rc::new(RefCell::new(None));
    let intrinsic_failure: Rc<RefCell<Option<inspector::InspectSetupError>>> =
        Rc::new(RefCell::new(None));
    let intrinsic_slot = Rc::clone(&intrinsics);
    let failure = Rc::clone(&intrinsic_failure);
    host_loop.post_event(Box::new(move |context| {
        match inspector::InspectIntrinsics::new(context) {
            Ok(created) => *intrinsic_slot.borrow_mut() = Some(created),
            Err(error) => *failure.borrow_mut() = Some(error),
        }
        Ok(())
    }));
    if let Err(error) = host_loop.run_one_turn() {
        eprintln!("fusor: cannot prepare CDP inspection: {error}");
        return 1;
    }
    if let Some(error) = intrinsic_failure.replace(None) {
        eprintln!("fusor: cannot prepare CDP inspection: {error}");
        return 1;
    }
    if intrinsics.borrow().is_none() {
        eprintln!("fusor: cannot prepare CDP inspection: the intrinsic setup did not run");
        return 1;
    }

    loop {
        tokio::select! {
            Some(request) = debug_requests.recv() => {
                let response_slot: Rc<RefCell<Option<serde_json::Value>>> =
                    Rc::new(RefCell::new(None));
                let slot = Rc::clone(&response_slot);
                let session = Arc::clone(&session);
                let inspect = Rc::clone(&inspect);
                let intrinsics = Rc::clone(&intrinsics);
                let suppressed = Rc::clone(&print_suppressed);
                let message = request.message;
                host_loop.post_event(Box::new(move |context| {
                    let response = session.handle_engine_protocol(
                        context,
                        &mut inspect.borrow_mut(),
                        intrinsics.borrow().as_ref().expect("CDP intrinsics prepared"),
                        &suppressed,
                        message,
                    );
                    *slot.borrow_mut() = Some(response);
                    Ok(())
                }));
                if let Err(error) = host_loop.run_one_turn() {
                    eprintln!("fusor: CDP request failed: {error}");
                    continue;
                }
                let _ = request
                    .response
                    .send(response_slot.replace(None).expect("CDP response rendered"));
            }
            Some(line) = input_receiver.recv() => {
                let Some(line) = line else {
                    println!();
                    return 0;
                };
                if line.trim() == ".exit" && pending.is_empty() {
                    return 0;
                }
                pending.push_str(&line);
                pending.push('\n');
                if !braces_balanced(&pending) {
                    continue;
                }
                let entry = std::mem::take(&mut pending);
                if entry.trim().is_empty() {
                    continue;
                }
                entry_index += 1;
                evaluate_repl_entry(
                    host_loop,
                    &entry,
                    &resolver,
                    &entry_prefix,
                    &mut imports,
                    entry_index,
                    true,
                ).await;
                // The loop's uncaught/unhandled default paths request the exit
                // code (§7.2); honor the request and leave the session.
                if let Some(code) = host_loop.pending_exit_code() {
                    return u8::try_from(code).unwrap_or(1);
                }
            }
            else => return 0,
        }
    }
}

async fn evaluate_repl_entry(
    host_loop: &mut HostLoop,
    entry: &str,
    resolver: &Rc<RefCell<NodeLikeResolver>>,
    entry_prefix: &str,
    imports: &mut Vec<String>,
    entry_index: u64,
    print_completion: bool,
) {
    let limits = ScriptLimits::default();
    if has_module_syntax(entry) {
        let mut source = imports.join("\n");
        if !source.is_empty() {
            source.push('\n');
        }
        source.push_str(entry);
        let name = format!("{entry_prefix}/__repl_entry_{entry_index}.mjs");
        let gathered = match crate::cli::loader::gather_static_graph(
            &resolver.borrow(),
            &source,
            &name,
            limits,
        )
        .await
        {
            Ok(edges) => edges,
            Err(error) => {
                report_error("module entry", &error);
                return;
            }
        };
        // Evaluation happens inside a turn (§6); the completion value is a
        // rooted handle, safe to print after the turn.
        let completion: Rc<RefCell<Option<fusor_runtime::JsValue>>> =
            Rc::new(RefCell::new(None));
        let value_slot = Rc::clone(&completion);
        let outcome: Rc<RefCell<Option<Result<(), ModuleEvaluationError>>>> =
            Rc::new(RefCell::new(None));
        let outcome_slot = Rc::clone(&outcome);
        let source = source.clone();
        let evaluate_name = name.clone();
        host_loop.post_event(Box::new(move |context| {
            let result =
                evaluate_preloaded_module_graph(context, &source, &evaluate_name, gathered, limits)
                    .map(|value| *value_slot.borrow_mut() = Some(value));
            *outcome_slot.borrow_mut() = Some(result);
            Ok(())
        }));
        if let Err(error) = host_loop.run_one_turn() {
            report_error("module entry", &error);
            return;
        }
        match outcome.replace(None) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                report_error("module entry", &error);
                return;
            }
            None => return,
        }
        if let Err(error) =
            crate::cli::imports::drain_pending_imports(host_loop, resolver, limits).await
        {
            report_error("module entry", &error);
        }
        // Probe the module's evaluation error inside a turn.
        let error_slot: Rc<RefCell<Option<ModuleEvaluationError>>> = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&error_slot);
        let probe_name = name.clone();
        host_loop.post_event(Box::new(move |context| {
            *slot.borrow_mut() = fusor::module_evaluation_error(context, &probe_name);
            Ok(())
        }));
        if let Err(error) = host_loop.run_one_turn() {
            report_error("module entry", &error);
            return;
        }
        if let Some(error) = error_slot.replace(None) {
            report_error("module entry", &error);
        }
        // Pending timers fire before the next prompt (virtual clock).
        if let Err(error) = host_loop.run_until_idle() {
            report_error("module entry", &error);
        }
        if print_completion {
            if let Some(value) = completion.replace(None) {
                println!("{}", format_value(&value));
            }
        }
        imports.extend(extract_imports(entry));
    } else {
        let name = format!("<repl>:{entry_index}");
        let completion: Rc<RefCell<Option<fusor_runtime::JsValue>>> =
            Rc::new(RefCell::new(None));
        let value_slot = Rc::clone(&completion);
        let outcome: Rc<RefCell<Option<Result<(), ScriptEvaluationError>>>> =
            Rc::new(RefCell::new(None));
        let outcome_slot = Rc::clone(&outcome);
        let entry = entry.to_owned();
        host_loop.post_event(Box::new(move |context| {
            let result = evaluate_script(context, &entry, &name, limits)
                .map(|value| *value_slot.borrow_mut() = Some(value));
            *outcome_slot.borrow_mut() = Some(result);
            Ok(())
        }));
        if let Err(error) = host_loop.run_one_turn() {
            report_error("script entry", &error);
            return;
        }
        match outcome.replace(None) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                report_error("script entry", &error);
                return;
            }
            None => return,
        }
        if let Err(error) =
            crate::cli::imports::drain_pending_imports(host_loop, resolver, limits).await
        {
            report_error("script entry", &error);
        }
        if let Err(error) = host_loop.run_until_idle() {
            report_error("script entry", &error);
        }
        if print_completion {
            if let Some(value) = completion.replace(None) {
                println!("{}", format_value(&value));
            }
        }
    }
}

/// Returns whether an entry looks like it uses module syntax. Heuristic: any
/// line starting with a static `import`/`export` statement. Dynamic
/// `import(` is an expression and stays on the script path.
fn has_module_syntax(source: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("import ")
            || trimmed.starts_with("import{")
            || trimmed.starts_with("export ")
            || trimmed.starts_with("export{")
    })
}

/// Collects complete single-line `import` statements for the session prefix.
fn extract_imports(source: &str) -> Vec<String> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| {
            (line.starts_with("import ") || line.starts_with("import{")) && line.ends_with(';')
        })
        .map(ToOwned::to_owned)
        .collect()
}

/// Basic multiline heuristic: brackets are balanced once every `(`/`[`/`{`
/// has a match, ignoring brackets inside strings, template literals, and
/// comments. Extra closing brackets count as balanced so the resulting parse
/// error surfaces from the engine instead of hanging the prompt.
fn braces_balanced(source: &str) -> bool {
    let mut depth = 0_i32;
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' | '\'' | '`' => {
                let quote = character;
                let mut escaped = false;
                for inner in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if inner == '\\' {
                        escaped = true;
                    } else if inner == quote {
                        break;
                    }
                }
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for inner in chars.by_ref() {
                        if inner == '\n' {
                            break;
                        }
                    }
                }
                Some('*') => {
                    let mut previous = '\0';
                    for inner in chars.by_ref() {
                        if previous == '*' && inner == '/' {
                            break;
                        }
                        previous = inner;
                    }
                }
                _ => {}
            },
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_heuristic_ignores_strings_and_comments() {
        assert!(!braces_balanced("function f() {"));
        assert!(braces_balanced("function f() {\n}"));
        assert!(braces_balanced("const s = \"} {\";"));
        assert!(braces_balanced("const t = `} ${ 1 }`; // {"));
        assert!(braces_balanced("/* { */ (1)"));
        assert!(braces_balanced("}"));
    }

    #[test]
    fn module_syntax_detection_covers_imports_and_exports() {
        assert!(has_module_syntax("import { a } from './a.mjs';"));
        assert!(has_module_syntax("import './side.mjs';"));
        assert!(has_module_syntax("export const x = 1;"));
        assert!(!has_module_syntax("const x = 1;"));
        assert!(!has_module_syntax("import('./dynamic.mjs');"));
    }

    #[test]
    fn import_extraction_keeps_complete_single_line_imports() {
        let imports = extract_imports(
            "import { a } from './a.mjs';\nimport './b.mjs'\nconst x = 1;\nimport {\n",
        );
        assert_eq!(imports, vec!["import { a } from './a.mjs';".to_owned()]);
    }
}
