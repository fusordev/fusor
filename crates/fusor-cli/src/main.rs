//! `fusor`: Node-like module runner and ESM REPL for the Experimental JavaScript Engine.

#![forbid(unsafe_code)]

mod builtins;
mod imports;
mod loader;
mod repl;
mod resolver;

use std::{error::Error, path::Path, process::ExitCode, sync::Arc};

use fusor::{ScriptLimits, evaluate_preloaded_module_graph, evaluate_script};
use fusor_cdp::{self as cdp, format::format_argument};
use fusor_runtime::{Runtime, RuntimeLimits};

use crate::resolver::NodeLikeResolver;

const USAGE: &str = "\
usage:
  fusor <file> [args...]        evaluate <file> as an ES module (default)
  fusor run [--script] [--inspect[=PORT]] [--inspect-brk[=PORT]] <file>
                                run with optional CDP (default port 9229)
  fusor --script <file>         evaluate <file> as a classic script
  fusor repl [--inspect[=PORT]] [--inspect-brk[=PORT]]
                                start the ESM REPL with optional CDP (default port 9229)";

/// The whole CLI runs on one shared `current_thread` Tokio runtime. The
/// engine is synchronous and its GC'd types are not `Send`, so all engine
/// calls stay on this main task; only pure data (paths, file bytes) crosses
/// await boundaries inside the module loader/drain.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse_arguments(&arguments) {
        Ok(Command::Repl {
            inspect_port,
            inspect_break,
        }) => ExitCode::from(repl::run(inspect_port, inspect_break).await),
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
    let mut runtime = match Runtime::try_new(RuntimeLimits::default()) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fusor: cannot create the runtime: {error}");
            return 2;
        }
    };
    let _debug_session = if let Some(port) = inspect_port {
        let session = cdp::DebugSession::without_engine();
        if inspect_break {
            session.request_initial_pause();
        }
        let debugger_hook: Arc<dyn fusor_runtime::DebuggerHook> = session.clone();
        runtime.set_debugger_hook(debugger_hook);
        let bound_port = match cdp::start(port, Arc::clone(&session)) {
            Ok(port) => port,
            Err(error) => {
                eprintln!("fusor: cannot start CDP inspector on 127.0.0.1:{port}: {error}");
                return 2;
            }
        };
        eprintln!("fusor inspector listening on ws://127.0.0.1:{bound_port}/devtools/page/fusor");
        Some(session)
    } else {
        None
    };
    let realm = match runtime.create_realm() {
        Ok(realm) => realm,
        Err(error) => {
            eprintln!("fusor: cannot create a realm: {error}");
            return 2;
        }
    };
    let mut context = match runtime.context(&realm) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("fusor: cannot create a context: {error}");
            return 2;
        }
    };

    let print = match context.create_host_function("print", |ctx, call| {
        let rendered: Vec<String> = call.arguments().iter().map(format_argument).collect();
        println!("{}", rendered.join(" "));
        Ok(ctx.undefined_value())
    }) {
        Ok(function) => function,
        Err(error) => {
            eprintln!("fusor: cannot install print: {error}");
            return 1;
        }
    };
    if let Err(error) = context.set_global("print", print.as_value()) {
        eprintln!("fusor: cannot install the print global: {error}");
        return 1;
    }

    let display_name = path.display().to_string();
    if as_script {
        match evaluate_script(
            &mut context,
            &source,
            &display_name,
            ScriptLimits::default(),
        ) {
            Ok(_) => 0,
            Err(error) => {
                report_error(&display_name, &error);
                1
            }
        }
    } else {
        // The root key is canonical: the absolute, lexically normalized path
        // prefixed with `file://`, matching the keys the resolver issues.
        let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new("/").to_path_buf());
        let root_path = resolver::normalize_path(&cwd.join(&path));
        let root_name = format!("file://{}", root_path.display());
        let mut process_argv = vec!["fusor".to_owned(), display_name];
        process_argv.extend(argv);
        let mut resolver = NodeLikeResolver::new(cwd, process_argv);
        let limits = ScriptLimits::default();
        // Load the static graph asynchronously (concurrent per-level reads on
        // this runtime), evaluate it synchronously, then drain parked
        // dynamic `import()` loads.
        let result = match loader::gather_static_graph(&resolver, &source, &root_name, limits).await
        {
            Ok(edges) => {
                evaluate_preloaded_module_graph(&mut context, &source, &root_name, edges, limits)
                    .map(|_| ())
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                match imports::drain_pending_imports(&mut context, &mut resolver, limits).await {
                    // A top-level-await graph settles asynchronously while the
                    // drain runs its continuations; a rejection recorded on the
                    // root is the evaluation failure.
                    Ok(()) => match fusor::module_evaluation_error(&context, &root_name) {
                        Some(error) => {
                            report_error(&root_name, &error);
                            1
                        }
                        None => 0,
                    },
                    Err(error) => {
                        report_error(&root_name, &error);
                        1
                    }
                }
            }
            Err(error) => {
                report_error(&root_name, &error);
                1
            }
        }
    }
}

/// Prints a top-level error through `miette`.
///
/// The facade errors (`ScriptEvaluationError`/`ModuleEvaluationError`) wrap the
/// leaf failure in several Rust typing layers (`…` → `GlobalScriptError` →
/// `ExecutionError`) whose `Display` all delegate to the same message. Those
/// layers are not a user-facing cause chain, so they are deliberately not
/// walked — reporting the top-level error once avoids the duplicate
/// `caused by:` lines a naive `source()` walk would emit.
pub(crate) fn report_error(origin: &str, error: &dyn Error) {
    let report = miette::Report::msg(format!("{origin}: {error}"));
    eprintln!("{report:?}");
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
