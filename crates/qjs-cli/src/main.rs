//! `qjs`: Node-like module runner and ESM REPL for the pure-Rust `QuickJS` port.

#![forbid(unsafe_code)]

mod builtins;
mod format;
mod imports;
mod repl;
mod resolver;

use std::{error::Error, path::Path, process::ExitCode};

use quickjs::{ScriptLimits, evaluate_module, evaluate_script};
use quickjs_runtime::{Runtime, RuntimeLimits};

use crate::resolver::NodeLikeResolver;

const USAGE: &str = "\
usage:
  qjs <file> [args...]        evaluate <file> as an ES module (default)
  qjs run [--script] <file>   same, with an explicit subcommand
  qjs --script <file>         evaluate <file> as a classic script
  qjs repl                    start the ESM REPL (.exit or Ctrl-D to quit)";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse_arguments(&arguments) {
        Ok(Command::Repl) => ExitCode::from(repl::run()),
        Ok(Command::Run { file, as_script, argv }) => ExitCode::from(run_file(&file, as_script, argv)),
        Ok(Command::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("qjs: {message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

enum Command {
    Repl,
    Run { file: String, as_script: bool, argv: Vec<String> },
    Help,
}

fn parse_arguments(arguments: &[String]) -> Result<Command, String> {
    if arguments.is_empty() {
        return Ok(Command::Repl);
    }
    if arguments[0] == "-h" || arguments[0] == "--help" {
        return Ok(Command::Help);
    }
    let rest: &[String] = if arguments[0] == "repl" {
        return Ok(Command::Repl);
    } else if arguments[0] == "run" {
        &arguments[1..]
    } else {
        arguments
    };
    let mut as_script = false;
    let mut index = 0;
    while let Some(argument) = rest.get(index) {
        match argument.as_str() {
            "--script" => as_script = true,
            "--module" => as_script = false,
            _ => break,
        }
        index += 1;
    }
    let file = rest
        .get(index)
        .ok_or_else(|| "missing <file> to evaluate".to_owned())?
        .clone();
    let argv = rest[index + 1..].to_vec();
    Ok(Command::Run { file, as_script, argv })
}

fn run_file(file: &str, as_script: bool, argv: Vec<String>) -> u8 {
    let path = match std::fs::canonicalize(Path::new(file)) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("qjs: cannot resolve '{file}': {error}");
            return 2;
        }
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("qjs: cannot read '{}': {error}", path.display());
            return 2;
        }
    };
    let mut runtime = match Runtime::try_new(RuntimeLimits::default()) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("qjs: cannot create the runtime: {error}");
            return 2;
        }
    };
    let realm = match runtime.create_realm() {
        Ok(realm) => realm,
        Err(error) => {
            eprintln!("qjs: cannot create a realm: {error}");
            return 2;
        }
    };
    let mut context = match runtime.context(&realm) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("qjs: cannot create a context: {error}");
            return 2;
        }
    };
    let display_name = path.display().to_string();
    if as_script {
        match evaluate_script(&mut context, &source, &display_name, ScriptLimits::default()) {
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
        let mut process_argv = vec!["qjs".to_owned(), display_name];
        process_argv.extend(argv);
        let mut resolver = NodeLikeResolver::new(cwd, process_argv);
        let limits = ScriptLimits::default();
        match evaluate_module(&mut context, &source, &root_name, &mut resolver, limits)
            .and_then(|_| imports::drain_pending_imports(&mut context, &mut resolver, limits))
        {
            Ok(()) => 0,
            Err(error) => {
                report_error(&root_name, &error);
                1
            }
        }
    }
}

/// Prints an error and its `source` chain to stderr.
pub(crate) fn report_error(origin: &str, error: &dyn Error) {
    eprintln!("{origin}: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default_module_run() {
        let command = parse_arguments(&["entry.mjs".to_owned(), "a".to_owned()]).expect("parse");
        let Command::Run { file, as_script, argv } = command else {
            panic!("expected a run command");
        };
        assert_eq!(file, "entry.mjs");
        assert!(!as_script);
        assert_eq!(argv, vec!["a".to_owned()]);
    }

    #[test]
    fn parses_script_mode_and_subcommands() {
        let command = parse_arguments(&["run".to_owned(), "--script".to_owned(), "s.js".to_owned()])
            .expect("parse");
        let Command::Run { as_script, .. } = command else {
            panic!("expected a run command");
        };
        assert!(as_script);
        assert!(matches!(
            parse_arguments(&["repl".to_owned()]).expect("parse"),
            Command::Repl
        ));
        assert!(matches!(parse_arguments(&[]).expect("parse"), Command::Repl));
        assert!(parse_arguments(&["run".to_owned()]).is_err());
    }
}
