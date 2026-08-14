//! `fusor`: Node-like module runner and ESM REPL for the Experimental JavaScript Engine.
//!
//! The binary target of the `fusor` package; the CLI modules (module loader,
//! builtin table, REPL) live in [`cli`](crate::cli) and the `DevTools` CDP
//! server in [`cdp`](crate::cdp), kept out of the facade library (`fusor`).

#![forbid(unsafe_code)]

mod cdp;
mod cli;

use std::{
    cell::RefCell, error::Error, io::Write as _, path::Path, process::ExitCode, rc::Rc, sync::Arc,
};

use fusor::{ScriptLimits, evaluate_preloaded_module_graph, evaluate_script};

use crate::cli::resolver::NodeLikeResolver;

const USAGE: &str = "\
usage:
  fusor <file> [args...]        evaluate <file> as an ES module (default)
  fusor run [--script] [--inspect[=PORT]] [--inspect-brk[=PORT]] <file>
                                run with optional CDP (default port 9229)
  fusor --script <file>         evaluate <file> as a classic script
  fusor repl [--inspect[=PORT]] [--inspect-brk[=PORT]]
                                start the ESM REPL with optional CDP (default port 9229)";

/// The CLI runs on a multi-threaded Tokio runtime (`rt-multi-thread`): the
/// timer driver lives on a dedicated worker and reliably wakes the parked
/// task, which the single-threaded flavor did not do while the REPL's
/// line-editor thread blocked on stdin. The engine is synchronous and its
/// GC'd types are not `Send`, so all engine interaction stays on one
/// [`tokio::task::LocalSet`] pinned to this thread — only pure data
/// (paths, file bytes) crosses await boundaries inside the module
/// loader/drain.
#[tokio::main]
async fn main() -> ExitCode {
    let local = tokio::task::LocalSet::new();
    local.run_until(cli_main()).await
}

/// The CLI body: argument parsing plus the run/REPL dispatch, executed on
/// the pinned local task set above.
async fn cli_main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse_arguments(&arguments) {
        Ok(Command::Repl {
            inspect_port,
            inspect_break,
        }) => ExitCode::from(cli::repl::run(inspect_port, inspect_break).await),
        Ok(Command::Run {
            file,
            as_script,
            argv,
            inspect_port,
            inspect_break,
        }) => ExitCode::from(run_file(&file, as_script, argv, inspect_port, inspect_break).await),
        Ok(Command::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("fusor: {message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

enum Command {
    Repl {
        inspect_port: Option<u16>,
        inspect_break: bool,
    },
    Run {
        file: String,
        as_script: bool,
        argv: Vec<String>,
        inspect_port: Option<u16>,
        inspect_break: bool,
    },
    Help,
}

fn parse_arguments(arguments: &[String]) -> Result<Command, String> {
    if arguments.is_empty() {
        return Ok(Command::Repl {
            inspect_port: None,
            inspect_break: false,
        });
    }
    if arguments[0] == "-h" || arguments[0] == "--help" {
        return Ok(Command::Help);
    }
    let rest: &[String] = if arguments[0] == "repl" {
        return parse_repl_arguments(&arguments[1..]);
    } else if arguments[0] == "run" {
        &arguments[1..]
    } else {
        arguments
    };
    let mut as_script = false;
    let mut inspect_port = None;
    let mut inspect_break = false;
    let mut index = 0;
    while let Some(argument) = rest.get(index) {
        match argument.as_str() {
            "--script" => as_script = true,
            "--module" => as_script = false,
            "--inspect" => inspect_port = Some(9229),
            "--inspect-brk" => {
                inspect_port = Some(9229);
                inspect_break = true;
            }
            _ if argument.starts_with("--inspect-brk=") => {
                let port = argument.trim_start_matches("--inspect-brk=");
                inspect_port = Some(
                    port.parse::<u16>()
                        .map_err(|_| format!("invalid --inspect-brk port '{port}'"))?,
                );
                inspect_break = true;
            }
            _ if argument.starts_with("--inspect=") => {
                let port = argument.trim_start_matches("--inspect=");
                inspect_port = Some(
                    port.parse::<u16>()
                        .map_err(|_| format!("invalid --inspect port '{port}'"))?,
                );
            }
            _ => break,
        }
        index += 1;
    }
    let file = rest
        .get(index)
        .ok_or_else(|| "missing <file> to evaluate".to_owned())?
        .clone();
    let argv = rest[index + 1..].to_vec();
    Ok(Command::Run {
        file,
        as_script,
        argv,
        inspect_port,
        inspect_break,
    })
}

fn parse_repl_arguments(arguments: &[String]) -> Result<Command, String> {
    let mut inspect_port = None;
    let mut inspect_break = false;
    for argument in arguments {
        if argument == "--inspect" {
            inspect_port = Some(9229);
        } else if argument == "--inspect-brk" {
            inspect_port = Some(9229);
            inspect_break = true;
        } else if let Some(port) = argument.strip_prefix("--inspect-brk=") {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("invalid --inspect-brk port '{port}'"))?;
            inspect_port = Some(port);
            inspect_break = true;
        } else if let Some(port) = argument.strip_prefix("--inspect=") {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("invalid --inspect port '{port}'"))?;
            inspect_port = Some(port);
        } else {
            return Err(format!("unknown repl option '{argument}'"));
        }
    }
    Ok(Command::Repl {
        inspect_port,
        inspect_break,
    })
}

async fn run_file(
    file: &str,
    as_script: bool,
    argv: Vec<String>,
    inspect_port: Option<u16>,
    inspect_break: bool,
) -> u8 {
    let path = match std::fs::canonicalize(Path::new(file)) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("fusor: cannot resolve '{file}': {error}");
            return 2;
        }
    };
    let source = match tokio::fs::read_to_string(&path).await {
        Ok(source) => source,
        Err(error) => {
            eprintln!("fusor: cannot read '{}': {error}", path.display());
            return 2;
        }
    };
    // The CLI assembles "core overlay + CLI overlay" through fusor-host
    // (§9 step 4): no hand-installed host globals — the CLI overlay's init
    // script shims `print` over `Fusor.ops.op_core_print`.
    let mut host = match fusor_host::overlay::HostRuntime::builder()
        .with_overlay(fusor_host::overlay::CoreOverlay)
        .with_overlay(cli::overlay::CliOverlay)
        .build()
    {
        Ok(host) => host,
        Err(error) => {
            eprintln!("fusor: cannot assemble the host runtime: {error}");
            return 2;
        }
    };
    let debug_session = if let Some(port) = inspect_port {
        let session = cdp::DebugSession::without_engine();
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
        Some((
            session,
            Rc::new(RefCell::new(cdp::inspector::InspectState::new())),
        ))
    } else {
        None
    };
    let mut host_loop = match host.into_loop() {
        Ok(host_loop) => host_loop,
        Err(error) => {
            eprintln!("fusor: cannot install the host event loop: {error}");
            return 2;
        }
    };

    let display_name = path.display().to_string();
    if as_script {
        // Evaluation happens inside a host-loop turn (§6); pending timers
        // fire before the process exits.
        let outcome: Rc<RefCell<Option<Result<(), fusor::ScriptEvaluationError>>>> =
            Rc::new(RefCell::new(None));
        let outcome_slot = Rc::clone(&outcome);
        let source = source.clone();
        let name = display_name.clone();
        let emission = debug_session
            .as_ref()
            .map(|(session, inspect)| (Arc::clone(session), Rc::clone(inspect)));
        host_loop.post_event(Box::new(move |context| {
            let result =
                evaluate_script(context, &source, &name, ScriptLimits::default()).map(|_| ());
            if let Err(error) = &result
                && let Some((session, inspect)) = &emission
            {
                let event = cdp::inspector::exception_thrown_event(
                    context,
                    &mut inspect.borrow_mut(),
                    None,
                    &cdp::inspector::CliException::from_script_error(error, &source),
                    &source,
                    &name,
                );
                session.emit_event(event);
            }
            *outcome_slot.borrow_mut() = Some(result);
            Ok(())
        }));
        if let Err(error) = host_loop.run_one_turn() {
            report_execution(&display_name, error);
            return 1;
        }
        match outcome.replace(None) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                report_script_error(&display_name, error);
                return 1;
            }
            None => return 1,
        }
        // Real-time idle wait (V8 semantics, §6.3): pending timers keep the
        // process alive — sleep until the next deadline, advance the virtual
        // clock to match, and run the firing turn.
        loop {
            if !host_loop.alive() {
                break;
            }
            let Some(remaining) = host_loop.next_deadline_in() else {
                break;
            };
            tokio::time::sleep(remaining).await;
            if let Err(error) = host_loop.advance_time(remaining) {
                report_execution(&display_name, error);
                return 1;
            }
            if let Err(error) = host_loop.run_one_turn() {
                report_execution(&display_name, error);
                return 1;
            }
        }
        // The loop's uncaught/unhandled default paths request the exit code
        // (§7.2); honor it on the way out.
        if let Some(code) = host_loop.pending_exit_code() {
            return u8::try_from(code).unwrap_or(1);
        }
        0
    } else {
        // The root key is canonical: the absolute, lexically normalized path
        // prefixed with `file://`, matching the keys the resolver issues.
        let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new("/").to_path_buf());
        let root_path = cli::resolver::normalize_path(&cwd.join(&path));
        let root_name = format!("file://{}", root_path.display());
        let mut process_argv = vec!["fusor".to_owned(), display_name.clone()];
        process_argv.extend(argv);
        let resolver = Rc::new(RefCell::new(NodeLikeResolver::new(cwd, process_argv)));
        let limits = ScriptLimits::default();
        // Load the static graph asynchronously (concurrent per-level reads on
        // this runtime), evaluate it inside a turn, then drain parked dynamic
        // `import()` loads across turns.
        let edges =
            match cli::loader::gather_static_graph(&resolver.borrow(), &source, &root_name, limits)
                .await
            {
                Ok(edges) => edges,
                Err(error) => {
                    emit_run_exception(
                        &mut host_loop,
                        &debug_session,
                        cdp::inspector::CliException::from_module_error(&error, &source),
                        source.clone(),
                        root_name.clone(),
                    );
                    report_module_error(&root_name, error);
                    return 1;
                }
            };
        let outcome: Rc<RefCell<Option<Result<(), fusor::ModuleEvaluationError>>>> =
            Rc::new(RefCell::new(None));
        let outcome_slot = Rc::clone(&outcome);
        let evaluation_source = source.clone();
        let name = root_name.clone();
        let emission = debug_session
            .as_ref()
            .map(|(session, inspect)| (Arc::clone(session), Rc::clone(inspect)));
        host_loop.post_event(Box::new(move |context| {
            let result =
                evaluate_preloaded_module_graph(context, &evaluation_source, &name, edges, limits)
                    .map(|_| ());
            if let Err(error) = &result
                && let Some((session, inspect)) = &emission
            {
                let event = cdp::inspector::exception_thrown_event(
                    context,
                    &mut inspect.borrow_mut(),
                    None,
                    &cdp::inspector::CliException::from_module_error(error, &evaluation_source),
                    &evaluation_source,
                    &name,
                );
                session.emit_event(event);
            }
            *outcome_slot.borrow_mut() = Some(result);
            Ok(())
        }));
        if let Err(error) = host_loop.run_one_turn() {
            report_execution(&root_name, error);
            return 1;
        }
        match outcome.replace(None) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                report_module_error(&root_name, error);
                return 1;
            }
            None => return 1,
        }
        if let Err(error) =
            cli::imports::drain_pending_imports(&mut host_loop, &resolver, limits).await
        {
            emit_run_exception(
                &mut host_loop,
                &debug_session,
                cdp::inspector::CliException::Message(error.to_string()),
                source.clone(),
                root_name.clone(),
            );
            report_module_error(&root_name, error);
            return 1;
        }
        // A top-level-await graph settles asynchronously while the drain runs
        // its continuations; a rejection recorded on the root is the
        // evaluation failure.
        let error_slot: Rc<RefCell<Option<fusor::ModuleEvaluationError>>> =
            Rc::new(RefCell::new(None));
        let slot = Rc::clone(&error_slot);
        let name = root_name.clone();
        let emission = debug_session
            .as_ref()
            .map(|(session, inspect)| (Arc::clone(session), Rc::clone(inspect)));
        host_loop.post_event(Box::new(move |context| {
            let error = fusor::module_evaluation_error(context, &name);
            if let Some(error) = &error
                && let Some((session, inspect)) = &emission
            {
                let event = cdp::inspector::exception_thrown_event(
                    context,
                    &mut inspect.borrow_mut(),
                    None,
                    &cdp::inspector::CliException::from_module_error(error, &source),
                    &source,
                    &name,
                );
                session.emit_event(event);
            }
            *slot.borrow_mut() = error;
            Ok(())
        }));
        if let Err(error) = host_loop.run_one_turn() {
            report_execution(&root_name, error);
            return 1;
        }
        if let Some(error) = error_slot.replace(None) {
            report_module_error(&root_name, error);
            return 1;
        }
        // Real-time idle wait (V8 semantics, §6.3): pending timers keep the
        // process alive — sleep until the next deadline, advance the virtual
        // clock to match, and run the firing turn.
        loop {
            if !host_loop.alive() {
                break;
            }
            let Some(remaining) = host_loop.next_deadline_in() else {
                break;
            };
            tokio::time::sleep(remaining).await;
            if let Err(error) = host_loop.advance_time(remaining) {
                report_execution(&root_name, error);
                return 1;
            }
            if let Err(error) = host_loop.run_one_turn() {
                report_execution(&root_name, error);
                return 1;
            }
        }
        // The loop's uncaught/unhandled default paths request the exit code
        // (§7.2); honor it on the way out.
        if let Some(code) = host_loop.pending_exit_code() {
            return u8::try_from(code).unwrap_or(1);
        }
        0
    }
}

/// Forwards one uncaught run-path evaluation failure to the attached
/// inspector session as a `Runtime.exceptionThrown` event (V8-aligned), from
/// inside a posted host-loop turn.
///
/// The module runner has no inspection intrinsics, so the event renders the
/// engine-side message without an expandable exception object. Best-effort:
/// the run path exits right after reporting, so the transport thread wins
/// the write race in practice but no delivery guarantee exists.
fn emit_run_exception(
    host_loop: &mut fusor_host::r#loop::HostLoop,
    emission: &Option<(
        Arc<cdp::DebugSession>,
        Rc<RefCell<cdp::inspector::InspectState>>,
    )>,
    exception: cdp::inspector::CliException,
    source: String,
    name: String,
) {
    let Some((session, inspect)) = emission else {
        return;
    };
    let session = Arc::clone(session);
    let inspect = Rc::clone(inspect);
    host_loop.post_event(Box::new(move |context| {
        let event = cdp::inspector::exception_thrown_event(
            context,
            &mut inspect.borrow_mut(),
            None,
            &exception,
            &source,
            &name,
        );
        session.emit_event(event);
        Ok(())
    }));
    let _ = host_loop.run_one_turn();
}

/// Renders one non-execution top-level error through the unified §7.5 miette
/// pipeline (`process::diagnostics`) as a message diagnostic with the CLI
/// origin prefix, honoring the environment color policy.
pub(crate) fn report_error(origin: &str, error: &dyn Error) {
    let policy = fusor_host::process::diagnostics::ColorPolicy::from_env();
    let rendered = fusor_host::process::diagnostics::render_diagnostic(
        fusor_host::process::diagnostics::MessageDiagnostic::new(format!("{origin}: {error}")),
        policy,
    );
    eprint!("{rendered}");
    // Piped stderr is block-buffered: flush the report so it appears
    // promptly; the renderer guarantees the trailing newline.
    let _ = std::io::stderr().flush();
}

/// Renders one engine [`ExecutionError`] through the §7.5 pipeline as a
/// [`HostDiagnostic`]: the exception identity, its numeric error code, and
/// frame labels inside the retained source text.
pub(crate) fn report_execution(origin: &str, execution: fusor_runtime::ExecutionError) {
    let _ = origin;
    let policy = fusor_host::process::diagnostics::ColorPolicy::from_env();
    let rendered = fusor_host::process::diagnostics::render_diagnostic(
        fusor_host::process::diagnostics::HostDiagnostic::new(execution),
        policy,
    );
    eprint!("{rendered}");
    let _ = std::io::stderr().flush();
}

/// Renders one script evaluation failure: the facade's `Runtime` arm carries
/// an [`ExecutionError`] (rendered as a [`HostDiagnostic`]); every other arm
/// renders as a message.
pub(crate) fn report_script_error(origin: &str, error: fusor::ScriptEvaluationError) {
    match error {
        fusor::ScriptEvaluationError::Runtime(fusor_runtime::GlobalScriptError::Execution(
            execution,
        )) => report_execution(origin, execution),
        other => report_error(origin, &other),
    }
}

/// Renders one module evaluation failure: the `Execution` arm carries an
/// [`ExecutionError`] (rendered as a [`HostDiagnostic`]); every other arm
/// renders as a message.
pub(crate) fn report_module_error(origin: &str, error: fusor::ModuleEvaluationError) {
    match error {
        fusor::ModuleEvaluationError::Execution(execution) => report_execution(origin, execution),
        other => report_error(origin, &other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default_module_run() {
        let command = parse_arguments(&["entry.mjs".to_owned(), "a".to_owned()]).expect("parse");
        let Command::Run {
            file,
            as_script,
            argv,
            inspect_port,
            inspect_break,
        } = command
        else {
            panic!("expected a run command");
        };
        assert_eq!(file, "entry.mjs");
        assert!(!as_script);
        assert_eq!(argv, vec!["a".to_owned()]);
        assert_eq!(inspect_port, None);
        assert!(!inspect_break);
    }

    #[test]
    fn parses_script_mode_and_subcommands() {
        let command =
            parse_arguments(&["run".to_owned(), "--script".to_owned(), "s.js".to_owned()])
                .expect("parse");
        let Command::Run { as_script, .. } = command else {
            panic!("expected a run command");
        };
        assert!(as_script);
        assert!(matches!(
            parse_arguments(&["repl".to_owned()]).expect("parse"),
            Command::Repl {
                inspect_port: None,
                inspect_break: false
            }
        ));
        assert!(matches!(
            parse_arguments(&[]).expect("parse"),
            Command::Repl {
                inspect_port: None,
                inspect_break: false
            }
        ));
        assert!(matches!(
            parse_arguments(&["repl".to_owned(), "--inspect=9333".to_owned()]).expect("parse"),
            Command::Repl {
                inspect_port: Some(9333),
                inspect_break: false
            }
        ));
        assert!(matches!(
            parse_arguments(&[
                "run".to_owned(),
                "--inspect-brk=9334".to_owned(),
                "entry.js".to_owned()
            ])
            .expect("parse"),
            Command::Run {
                inspect_port: Some(9334),
                inspect_break: true,
                ..
            }
        ));
        assert!(matches!(
            parse_arguments(&["repl".to_owned(), "--inspect-brk".to_owned()]).expect("parse"),
            Command::Repl {
                inspect_port: Some(9229),
                inspect_break: true
            }
        ));
        assert!(parse_arguments(&["run".to_owned()]).is_err());
    }
}
