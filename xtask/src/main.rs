//! Repository development tasks.

#![forbid(unsafe_code)]

mod control_flow_differential;
mod dynamic_function_differential;
mod number_radix_differential;
mod parser_diagnostics;
mod parser_differential;
mod parser_productions;
mod test262;

use control_flow_differential::{
    ASYNC_FUNCTION_CANDIDATE_WORKER_COMMAND, AsyncFunctionDifferentialOptions,
    AsyncGeneratorDifferentialOptions, CANDIDATE_WORKER_COMMAND, CallSpreadDifferentialOptions,
    ControlFlowDifferentialOptions, DEFAULT_ASYNC_FUNCTION_CORPUS, DEFAULT_ASYNC_GENERATOR_CORPUS,
    DEFAULT_CALL_SPREAD_CORPUS, DEFAULT_CONTROL_FLOW_CORPUS, DEFAULT_ERROR_CORPUS,
    DEFAULT_FUNCTION_APPLY_CORPUS, DEFAULT_FUNCTION_BIND_CORPUS, DEFAULT_GENERATOR_CORPUS,
    DEFAULT_ITERATOR_CORPUS, DEFAULT_MAP_CORPUS, DEFAULT_OBJECT_LEGACY_CORPUS,
    DEFAULT_PROMISE_CORE_CORPUS, DEFAULT_SET_CORPUS, DEFAULT_STRING_HTML_CORPUS,
    DEFAULT_STRING_REPLACE_ALL_CORPUS, DEFAULT_STRING_SPLIT_CORPUS,
    DEFAULT_WEAK_COLLECTIONS_CORPUS, DEFAULT_WEAK_REFERENCES_CORPUS, ErrorDifferentialOptions,
    FunctionApplyDifferentialOptions, FunctionBindDifferentialOptions,
    GeneratorDifferentialOptions, IteratorDifferentialOptions, MAX_CONTROL_FLOW_TIMEOUT_MS,
    MapDifferentialOptions, ObjectLegacyDifferentialOptions, PromiseCoreDifferentialOptions,
    SetDifferentialOptions, StringHtmlDifferentialOptions, StringReplaceAllDifferentialOptions,
    StringSplitDifferentialOptions, WeakCollectionsDifferentialOptions,
    WeakReferencesDifferentialOptions, run_async_function_differential,
    run_async_generator_differential, run_call_spread_differential,
    run_control_flow_candidate_worker, run_control_flow_differential, run_error_differential,
    run_function_apply_differential, run_function_bind_differential, run_generator_differential,
    run_iterator_differential, run_map_differential, run_object_legacy_differential,
    run_promise_core_differential, run_set_differential, run_string_html_differential,
    run_string_replace_all_differential, run_string_split_differential,
    run_weak_collections_differential, run_weak_references_differential,
};
use dynamic_function_differential::{
    DynamicFunctionDifferentialOptions, run_dynamic_function_differential,
};
use number_radix_differential::{
    DEFAULT_NUMBER_RADIX_CORPUS, NumberRadixDifferentialOptions, run_number_radix_differential,
};
use parser_differential::{ParserDifferentialOptions, run_parser_differential};
use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use test262::{Test262Options, run_test262};

const DEFAULT_CORPUS: &str = "tests/differential";
const DEFAULT_PARSER_CORPUS: &str = "tests/parser";
const DEFAULT_DYNAMIC_FUNCTION_CORPUS: &str = "tests/dynamic-function";
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

#[allow(
    clippy::too_many_lines,
    reason = "each differential task keeps one visible success/failure boundary"
)]
fn main() -> ExitCode {
    match Args::parse(env::args_os().skip(1)) {
        Ok(Args::Help) => {
            print_usage();
            ExitCode::SUCCESS
        }
        Ok(Args::Differential(options)) => match run_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::ParserDifferential(options)) => match run_parser_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::DynamicFunctionDifferential(options)) => {
            match run_dynamic_function_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::NumberRadixDifferential(options)) => {
            match run_number_radix_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::ControlFlowDifferential(options)) => {
            match run_control_flow_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::AsyncFunctionDifferential(options)) => {
            match run_async_function_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::AsyncGeneratorDifferential(options)) => {
            match run_async_generator_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::ErrorDifferential(options)) => match run_error_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::FunctionApplyDifferential(options)) => {
            match run_function_apply_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::FunctionBindDifferential(options)) => {
            match run_function_bind_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::GeneratorDifferential(options)) => match run_generator_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::IteratorDifferential(options)) => match run_iterator_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::CallSpreadDifferential(options)) => match run_call_spread_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::ObjectLegacyDifferential(options)) => {
            match run_object_legacy_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::PromiseCoreDifferential(options)) => {
            match run_promise_core_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::StringHtmlDifferential(options)) => match run_string_html_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::StringReplaceAllDifferential(options)) => {
            match run_string_replace_all_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::StringSplitDifferential(options)) => {
            match run_string_split_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::MapDifferential(options)) => match run_map_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::SetDifferential(options)) => match run_set_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::WeakCollectionsDifferential(options)) => {
            match run_weak_collections_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::WeakReferencesDifferential(options)) => {
            match run_weak_references_differential(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Args::Test262(options)) => match run_test262(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::ControlFlowCandidateWorker { read_async_result }) => {
            match run_control_flow_candidate_worker(read_async_result) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("xtask: {error}\n");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    println!(
        "\
Usage:
  cargo xtask differential --oracle PATH --candidate PATH [OPTIONS]
  cargo xtask parser-differential --oracle PATH [OPTIONS]
  cargo xtask dynamic-function-differential --oracle QJSC_PATH [OPTIONS]
  cargo xtask number-radix-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask async-function-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask async-generator-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask control-flow-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask error-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask function-apply-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask function-bind-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask generator-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask iterator-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask call-spread-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask object-legacy-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask promise-core-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask string-html-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask string-replace-all-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask string-split-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask map-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask set-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask weak-collections-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask weak-references-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask test262 --suite TEST262_PATH [OPTIONS]
Options:
  --corpus PATH       Corpus directory (runtime default: {DEFAULT_CORPUS};
                      parser default: {DEFAULT_PARSER_CORPUS};
                      dynamic Function default: {DEFAULT_DYNAMIC_FUNCTION_CORPUS};
                      Number radix manifest default: {DEFAULT_NUMBER_RADIX_CORPUS};
                      async-function manifest default: {DEFAULT_ASYNC_FUNCTION_CORPUS};
                      async-generator manifest default: {DEFAULT_ASYNC_GENERATOR_CORPUS};
                      control-flow manifest default: {DEFAULT_CONTROL_FLOW_CORPUS};
                      Error manifest default: {DEFAULT_ERROR_CORPUS};
                      Function.prototype.apply manifest default: {DEFAULT_FUNCTION_APPLY_CORPUS};
                      generator manifest default: {DEFAULT_GENERATOR_CORPUS};
                      iterator manifest default: {DEFAULT_ITERATOR_CORPUS};
                      call-spread manifest default: {DEFAULT_CALL_SPREAD_CORPUS};
                      legacy Object manifest default: {DEFAULT_OBJECT_LEGACY_CORPUS};
                      Promise core manifest default: {DEFAULT_PROMISE_CORE_CORPUS};
                      Annex B String HTML manifest default: {DEFAULT_STRING_HTML_CORPUS};
                      String replaceAll manifest default: {DEFAULT_STRING_REPLACE_ALL_CORPUS};
                      String split manifest default: {DEFAULT_STRING_SPLIT_CORPUS};
                      Map manifest default: {DEFAULT_MAP_CORPUS};
                      Set manifest default: {DEFAULT_SET_CORPUS};
                      weak collections manifest default: {DEFAULT_WEAK_COLLECTIONS_CORPUS};
                      weak references manifest default: {DEFAULT_WEAK_REFERENCES_CORPUS})
  --timeout-ms N      Differential process/Test262 case timeout (default: {DEFAULT_TIMEOUT_MS})
  --baseline PATH     Test262 baseline artifacts (default: tests/test262/upstream)
  --filter PATH       Restrict Test262 to one file or subtree below test/
  --admit-feature N   Admit one baseline-skipped feature within --filter
  --admit-intl402     Admit ECMA-402 policy skips within an Intl-only --filter
  --limit N           Stop Test262 inventory after N matching source files
  --report PATH       Write the deterministic Test262 JSON report
  --inventory-only    Verify and classify Test262 without executing cases
  --instruction-fuel N
                      Per-Script Test262 interpreter fuel (default: 10000000)
  --jobs N           Parallel Test262 workers (default: available CPU parallelism)
  --progress-every N Print aggregate Test262 progress every N completed cases
  -v, --verbose       Stream selection, skip counts, and each case result to the CI log
  -h, --help          Show this help
"
    );
    print_oracle_usage();
}

fn print_oracle_usage() {
    println!(
        "\
Dynamic Function --oracle must be the pinned QuickJS 2026-06-04 qjsc compiler.
It is invoked with -c only; generated code is never compiled or executed.
Number radix --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter.
Async function --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Async generator --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Control flow --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Error --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Function.prototype.apply --oracle must be the pinned QuickJS 2026-06-04 qjs
interpreter; its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Generator --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Iterator --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Call spread --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Legacy Object --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Promise core --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Annex B String HTML --oracle must be the pinned QuickJS 2026-06-04 qjs
interpreter; its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
String replaceAll --oracle must be the pinned QuickJS 2026-06-04 qjs
interpreter; its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
String split --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Map --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Set --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Weak collections --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Weak references --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
"
    );
}

#[derive(Debug, Eq, PartialEq)]
enum Args {
    Help,
    Differential(DifferentialOptions),
    ParserDifferential(ParserDifferentialOptions),
    DynamicFunctionDifferential(DynamicFunctionDifferentialOptions),
    NumberRadixDifferential(NumberRadixDifferentialOptions),
    AsyncFunctionDifferential(AsyncFunctionDifferentialOptions),
    AsyncGeneratorDifferential(AsyncGeneratorDifferentialOptions),
    ControlFlowDifferential(ControlFlowDifferentialOptions),
    ErrorDifferential(ErrorDifferentialOptions),
    FunctionApplyDifferential(FunctionApplyDifferentialOptions),
    FunctionBindDifferential(FunctionBindDifferentialOptions),
    GeneratorDifferential(GeneratorDifferentialOptions),
    IteratorDifferential(IteratorDifferentialOptions),
    CallSpreadDifferential(CallSpreadDifferentialOptions),
    ObjectLegacyDifferential(ObjectLegacyDifferentialOptions),
    PromiseCoreDifferential(PromiseCoreDifferentialOptions),
    StringHtmlDifferential(StringHtmlDifferentialOptions),
    StringReplaceAllDifferential(StringReplaceAllDifferentialOptions),
    StringSplitDifferential(StringSplitDifferentialOptions),
    MapDifferential(MapDifferentialOptions),
    SetDifferential(SetDifferentialOptions),
    WeakCollectionsDifferential(WeakCollectionsDifferentialOptions),
    WeakReferencesDifferential(WeakReferencesDifferentialOptions),
    Test262(Test262Options),
    ControlFlowCandidateWorker { read_async_result: bool },
}

impl Args {
    fn parse(
        arguments: impl Iterator<Item = impl Into<std::ffi::OsString>>,
    ) -> Result<Self, String> {
        let mut arguments = arguments.map(Into::into);
        let Some(command) = arguments.next() else {
            return Ok(Self::Help);
        };

        if command == "-h" || command == "--help" {
            return Ok(Self::Help);
        }
        let arguments = arguments.collect::<Vec<_>>();
        if command == CANDIDATE_WORKER_COMMAND {
            return if arguments.is_empty() {
                Ok(Self::ControlFlowCandidateWorker {
                    read_async_result: false,
                })
            } else {
                Err("internal control-flow candidate worker accepts no arguments".to_owned())
            };
        }
        if command == ASYNC_FUNCTION_CANDIDATE_WORKER_COMMAND {
            return if arguments.is_empty() {
                Ok(Self::ControlFlowCandidateWorker {
                    read_async_result: true,
                })
            } else {
                Err("internal async-function candidate worker accepts no arguments".to_owned())
            };
        }
        if contains_help(&arguments) {
            return Ok(Self::Help);
        }
        Self::parse_command(&command, arguments)
    }

    fn parse_command(command: &OsStr, arguments: Vec<std::ffi::OsString>) -> Result<Self, String> {
        match command.to_string_lossy().as_ref() {
            "differential" => {
                parse_differential_options(arguments.into_iter()).map(Self::Differential)
            }
            "parser-differential" => parse_parser_differential_options(arguments.into_iter())
                .map(Self::ParserDifferential),
            "dynamic-function-differential" => {
                parse_dynamic_function_differential_options(arguments.into_iter())
                    .map(Self::DynamicFunctionDifferential)
            }
            "number-radix-differential" => {
                parse_number_radix_differential_options(arguments.into_iter())
                    .map(Self::NumberRadixDifferential)
            }
            "async-function-differential" => {
                parse_async_function_differential_options(arguments.into_iter())
                    .map(Self::AsyncFunctionDifferential)
            }
            "async-generator-differential" => {
                parse_async_generator_differential_options(arguments.into_iter())
                    .map(Self::AsyncGeneratorDifferential)
            }
            "control-flow-differential" => {
                parse_control_flow_differential_options(arguments.into_iter())
                    .map(Self::ControlFlowDifferential)
            }
            "error-differential" => {
                parse_error_differential_options(arguments.into_iter()).map(Self::ErrorDifferential)
            }
            "function-apply-differential" => {
                parse_function_apply_differential_options(arguments.into_iter())
                    .map(Self::FunctionApplyDifferential)
            }
            "function-bind-differential" => {
                parse_function_bind_differential_options(arguments.into_iter())
                    .map(Self::FunctionBindDifferential)
            }
            "generator-differential" => parse_generator_differential_options(arguments.into_iter())
                .map(Self::GeneratorDifferential),
            "iterator-differential" => parse_iterator_differential_options(arguments.into_iter())
                .map(Self::IteratorDifferential),
            "call-spread-differential" => {
                parse_call_spread_differential_options(arguments.into_iter())
                    .map(Self::CallSpreadDifferential)
            }
            "object-legacy-differential" => {
                parse_object_legacy_differential_options(arguments.into_iter())
                    .map(Self::ObjectLegacyDifferential)
            }
            "promise-core-differential" => {
                parse_promise_core_differential_options(arguments.into_iter())
                    .map(Self::PromiseCoreDifferential)
            }
            "string-html-differential" => {
                parse_string_html_differential_options(arguments.into_iter())
                    .map(Self::StringHtmlDifferential)
            }
            "string-replace-all-differential" => {
                parse_string_replace_all_differential_options(arguments.into_iter())
                    .map(Self::StringReplaceAllDifferential)
            }
            "string-split-differential" => {
                parse_string_split_differential_options(arguments.into_iter())
                    .map(Self::StringSplitDifferential)
            }
            "map-differential" => {
                parse_map_differential_options(arguments.into_iter()).map(Self::MapDifferential)
            }
            "set-differential" => {
                parse_set_differential_options(arguments.into_iter()).map(Self::SetDifferential)
            }
            "weak-collections-differential" => {
                parse_weak_collections_differential_options(arguments.into_iter())
                    .map(Self::WeakCollectionsDifferential)
            }
            "weak-references-differential" => {
                parse_weak_references_differential_options(arguments.into_iter())
                    .map(Self::WeakReferencesDifferential)
            }
            "test262" => test262::parse_options(arguments.into_iter()).map(Self::Test262),
            unknown => Err(format!("unknown task `{unknown}`")),
        }
    }
}

fn contains_help(arguments: &[std::ffi::OsString]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
}

fn parse_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<DifferentialOptions, String> {
    let mut oracle = None;
    let mut candidate = None;
    let mut corpus = PathBuf::from(DEFAULT_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--candidate" => candidate = Some(required_path(&mut arguments, "--candidate")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => timeout = required_timeout(&mut arguments)?,
            unknown => return Err(format!("unknown differential option `{unknown}`")),
        }
    }

    Ok(DifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        candidate: candidate.ok_or("missing required --candidate PATH")?,
        corpus,
        timeout,
    })
}

fn parse_parser_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<ParserDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_PARSER_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => timeout = required_timeout(&mut arguments)?,
            unknown => {
                return Err(format!("unknown parser-differential option `{unknown}`"));
            }
        }
    }

    Ok(ParserDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_dynamic_function_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<DynamicFunctionDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_DYNAMIC_FUNCTION_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => timeout = required_timeout(&mut arguments)?,
            unknown => {
                return Err(format!(
                    "unknown dynamic-function-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(DynamicFunctionDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_number_radix_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<NumberRadixDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_NUMBER_RADIX_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => timeout = required_timeout(&mut arguments)?,
            unknown => {
                return Err(format!(
                    "unknown number-radix-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(NumberRadixDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_control_flow_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<ControlFlowDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_CONTROL_FLOW_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "control-flow --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown control-flow-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(ControlFlowDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_async_function_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<AsyncFunctionDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_ASYNC_FUNCTION_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "async-function --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown async-function-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(AsyncFunctionDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_async_generator_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<AsyncGeneratorDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_ASYNC_GENERATOR_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "async-generator --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown async-generator-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(AsyncGeneratorDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_error_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<ErrorDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_ERROR_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "error --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!("unknown error-differential option `{unknown}`"));
            }
        }
    }

    Ok(ErrorDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_function_apply_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<FunctionApplyDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_FUNCTION_APPLY_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "function-apply --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown function-apply-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(FunctionApplyDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_function_bind_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<FunctionBindDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_FUNCTION_BIND_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "function-bind --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown function-bind-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(FunctionBindDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_generator_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<GeneratorDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_GENERATOR_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "generator --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!("unknown generator-differential option `{unknown}`"));
            }
        }
    }

    Ok(GeneratorDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_iterator_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<IteratorDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_ITERATOR_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "iterator --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!("unknown iterator-differential option `{unknown}`"));
            }
        }
    }

    Ok(IteratorDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_call_spread_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<CallSpreadDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_CALL_SPREAD_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "call-spread --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown call-spread-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(CallSpreadDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_object_legacy_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<ObjectLegacyDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_OBJECT_LEGACY_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "object-legacy --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown object-legacy-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(ObjectLegacyDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_promise_core_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<PromiseCoreDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_PROMISE_CORE_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "promise-core --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown promise-core-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(PromiseCoreDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_string_html_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<StringHtmlDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_STRING_HTML_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "string-html --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown string-html-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(StringHtmlDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_string_replace_all_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<StringReplaceAllDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_STRING_REPLACE_ALL_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "string-replace-all --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown string-replace-all-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(StringReplaceAllDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_string_split_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<StringSplitDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_STRING_SPLIT_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "string-split --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown string-split-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(StringSplitDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_map_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<MapDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_MAP_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "map --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!("unknown map-differential option `{unknown}`"));
            }
        }
    }

    Ok(MapDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_set_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<SetDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_SET_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "set --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!("unknown set-differential option `{unknown}`"));
            }
        }
    }

    Ok(SetDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_weak_collections_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<WeakCollectionsDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_WEAK_COLLECTIONS_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "weak-collections --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown weak-collections-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(WeakCollectionsDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn parse_weak_references_differential_options(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<WeakReferencesDifferentialOptions, String> {
    let mut oracle = None;
    let mut corpus = PathBuf::from(DEFAULT_WEAK_REFERENCES_CORPUS);
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--oracle" => oracle = Some(required_path(&mut arguments, "--oracle")?),
            "--corpus" => corpus = required_path(&mut arguments, "--corpus")?,
            "--timeout-ms" => {
                timeout = required_timeout(&mut arguments)?;
                let milliseconds = timeout.as_millis();
                if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
                    return Err(format!(
                        "weak-references --timeout-ms must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS}"
                    ));
                }
            }
            unknown => {
                return Err(format!(
                    "unknown weak-references-differential option `{unknown}`"
                ));
            }
        }
    }

    Ok(WeakReferencesDifferentialOptions {
        oracle: oracle.ok_or("missing required --oracle PATH")?,
        corpus,
        timeout,
    })
}

fn required_timeout(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<Duration, String> {
    let value = required_value(arguments, "--timeout-ms")?;
    let milliseconds = value
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|_| "--timeout-ms must be a non-negative integer".to_owned())?;
    Ok(Duration::from_millis(milliseconds))
}

fn required_path(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    required_value(arguments, option).map(PathBuf::from)
}

fn required_value(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    option: &str,
) -> Result<std::ffi::OsString, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

#[derive(Debug, Eq, PartialEq)]
struct DifferentialOptions {
    oracle: PathBuf,
    candidate: PathBuf,
    corpus: PathBuf,
    timeout: Duration,
}

fn run_differential(options: &DifferentialOptions) -> Result<bool, String> {
    validate_executable(&options.oracle, "oracle")?;
    validate_executable(&options.candidate, "candidate")?;

    let mut fixtures = Vec::new();
    collect_javascript_files(&options.corpus, &mut fixtures).map_err(|error| {
        format!(
            "failed to read corpus {}: {error}",
            options.corpus.display()
        )
    })?;
    fixtures.sort();

    if fixtures.is_empty() {
        return Err(format!(
            "corpus {} contains no .js or .mjs files",
            options.corpus.display()
        ));
    }

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    for fixture in &fixtures {
        let oracle = run_program(&options.oracle, fixture, options.timeout)?;
        let candidate = run_program(&options.candidate, fixture, options.timeout)?;
        if oracle == candidate {
            passed += 1;
        } else {
            failures.push(format_mismatch(fixture, &oracle, &candidate));
        }
    }

    if failures.is_empty() {
        println!("differential: {passed}/{} fixtures match", fixtures.len());
        return Ok(true);
    }

    for failure in &failures {
        eprintln!("{failure}");
    }
    eprintln!(
        "differential: {passed}/{} fixtures match; {} mismatch(es)",
        fixtures.len(),
        failures.len()
    );
    Ok(false)
}

pub(crate) fn validate_executable(path: &Path, role: &str) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("{role} executable {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "{role} executable {} is not a file",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn collect_javascript_files(
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_javascript_files(&path, output)?;
        } else if file_type.is_file()
            && matches!(path.extension().and_then(OsStr::to_str), Some("js" | "mjs"))
        {
            output.push(path);
        }
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ProgramOutput {
    pub(crate) status: Status,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Exited(Option<i32>),
    TimedOut,
}

fn run_program(
    executable: &Path,
    fixture: &Path,
    timeout: Duration,
) -> Result<ProgramOutput, String> {
    run_program_with_arguments(executable, &[fixture.as_os_str()], timeout)
}

pub(crate) fn run_program_with_arguments(
    executable: &Path,
    arguments: &[&OsStr],
    timeout: Duration,
) -> Result<ProgramOutput, String> {
    run_program_with_arguments_inner(executable, arguments, None, timeout, None)
}

pub(crate) fn run_program_with_arguments_bounded(
    executable: &Path,
    arguments: &[&OsStr],
    timeout: Duration,
    max_stream_bytes: usize,
) -> Result<ProgramOutput, String> {
    run_program_with_arguments_inner(executable, arguments, None, timeout, Some(max_stream_bytes))
}

pub(crate) fn run_program_with_arguments_bounded_input(
    executable: &Path,
    arguments: &[&OsStr],
    input: &[u8],
    timeout: Duration,
    max_stream_bytes: usize,
) -> Result<ProgramOutput, String> {
    run_program_with_arguments_inner(
        executable,
        arguments,
        Some(input),
        timeout,
        Some(max_stream_bytes),
    )
}

fn run_program_with_arguments_inner(
    executable: &Path,
    arguments: &[&OsStr],
    input: Option<&[u8]>,
    timeout: Duration,
    max_stream_bytes: Option<usize>,
) -> Result<ProgramOutput, String> {
    let input = if let Some(input) = input {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(input.len())
            .map_err(|_| format!("cannot reserve {} bytes for child stdin", input.len()))?;
        bytes.extend_from_slice(input);
        Some(bytes)
    } else {
        None
    };
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to run {}: {error}", executable.display()))?;

    let stdin_writer = if let Some(bytes) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open child stdin".to_owned())?;
        Some(thread::spawn(move || stdin.write_all(&bytes)))
    } else {
        None
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture child stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture child stderr".to_owned())?;
    let stdout_reader = thread::spawn(move || read_all(stdout, max_stream_bytes));
    let stderr_reader = thread::spawn(move || read_all(stderr, max_stream_bytes));

    let status = wait_with_timeout(&mut child, timeout)?;
    if let Some(writer) = stdin_writer {
        let input_result = writer
            .join()
            .map_err(|_| "stdin writer thread panicked".to_owned())?;
        if status != Status::TimedOut {
            input_result.map_err(|error| format!("failed to write child stdin: {error}"))?;
        }
    }
    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;

    Ok(ProgramOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_all(mut pipe: impl Read, max_bytes: Option<usize>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(max_bytes) = max_bytes {
        let requested = max_bytes.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "output byte limit overflowed")
        })?;
        bytes.try_reserve_exact(requested).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("cannot reserve {requested} bytes for child output"),
            )
        })?;
        pipe.take(u64::try_from(requested).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "output byte limit does not fit u64",
            )
        })?)
        .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("child output exceeds the {max_bytes}-byte stream limit"),
            ));
        }
    } else {
        pipe.read_to_end(&mut bytes)?;
    }
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("{stream} reader thread panicked"))?
        .map_err(|error| format!("failed to read child {stream}: {error}"))
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<Status, String> {
    let start = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to wait for child process: {error}"))?
        {
            Some(status) => return Ok(Status::Exited(exit_code(status))),
            None if start.elapsed() >= timeout => {
                child
                    .kill()
                    .map_err(|error| format!("failed to kill timed-out child: {error}"))?;
                child
                    .wait()
                    .map_err(|error| format!("failed to reap timed-out child: {error}"))?;
                return Ok(Status::TimedOut);
            }
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn exit_code(status: ExitStatus) -> Option<i32> {
    status.code()
}

fn format_mismatch(fixture: &Path, oracle: &ProgramOutput, candidate: &ProgramOutput) -> String {
    let mut message = format!("--- {}\n", fixture.display());
    if oracle.status != candidate.status {
        let _ = writeln!(
            message,
            "status: oracle={:?}, candidate={:?}",
            oracle.status, candidate.status
        );
    }
    if oracle.stdout != candidate.stdout {
        write_stream_difference(&mut message, "stdout", &oracle.stdout, &candidate.stdout);
    }
    if oracle.stderr != candidate.stderr {
        write_stream_difference(&mut message, "stderr", &oracle.stderr, &candidate.stderr);
    }
    message
}

fn write_stream_difference(message: &mut String, name: &str, oracle: &[u8], candidate: &[u8]) {
    let _ = writeln!(message, "{name} (oracle):");
    let _ = writeln!(message, "{}", String::from_utf8_lossy(oracle));
    let _ = writeln!(message, "{name} (candidate):");
    let _ = writeln!(message, "{}", String::from_utf8_lossy(candidate));
}

#[cfg(test)]
mod tests {
    use super::{
        Args, DifferentialOptions, Status, collect_javascript_files, read_all,
        run_program_with_arguments_bounded_input, wait_with_timeout,
    };
    use crate::control_flow_differential::ControlFlowDifferentialOptions;
    use crate::control_flow_differential::ErrorDifferentialOptions;
    use crate::control_flow_differential::FunctionApplyDifferentialOptions;
    use crate::control_flow_differential::FunctionBindDifferentialOptions;
    use crate::control_flow_differential::GeneratorDifferentialOptions;
    use crate::control_flow_differential::IteratorDifferentialOptions;
    use crate::control_flow_differential::MapDifferentialOptions;
    use crate::control_flow_differential::ObjectLegacyDifferentialOptions;
    use crate::control_flow_differential::PromiseCoreDifferentialOptions;
    use crate::control_flow_differential::SetDifferentialOptions;
    use crate::control_flow_differential::StringHtmlDifferentialOptions;
    use crate::control_flow_differential::StringReplaceAllDifferentialOptions;
    use crate::control_flow_differential::StringSplitDifferentialOptions;
    use crate::control_flow_differential::WeakCollectionsDifferentialOptions;
    use crate::control_flow_differential::WeakReferencesDifferentialOptions;
    use crate::dynamic_function_differential::DynamicFunctionDifferentialOptions;
    use crate::number_radix_differential::NumberRadixDifferentialOptions;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[test]
    fn parses_differential_options() {
        let arguments = [
            "differential",
            "--oracle",
            "/tmp/oracle",
            "--candidate",
            "/tmp/candidate",
            "--corpus",
            "fixtures",
            "--timeout-ms",
            "250",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::Differential(DifferentialOptions {
                oracle: PathBuf::from("/tmp/oracle"),
                candidate: PathBuf::from("/tmp/candidate"),
                corpus: PathBuf::from("fixtures"),
                timeout: Duration::from_millis(250),
            }))
        );
    }

    #[test]
    fn parses_dynamic_function_differential_options() {
        let arguments = [
            "dynamic-function-differential",
            "--oracle",
            "/tmp/qjsc",
            "--corpus",
            "fixtures",
            "--timeout-ms",
            "125",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::DynamicFunctionDifferential(
                DynamicFunctionDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjsc"),
                    corpus: PathBuf::from("fixtures"),
                    timeout: Duration::from_millis(125),
                }
            ))
        );
    }

    #[test]
    fn parses_number_radix_differential_options() {
        let arguments = [
            "number-radix-differential",
            "--oracle",
            "/tmp/qjs",
            "--corpus",
            "tests/number-radix/custom.json",
            "--timeout-ms",
            "375",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::NumberRadixDifferential(
                NumberRadixDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/number-radix/custom.json"),
                    timeout: Duration::from_millis(375),
                }
            ))
        );
    }

    #[test]
    fn parses_control_flow_differential_options() {
        let arguments = [
            "control-flow-differential",
            "--oracle",
            "/tmp/qjs",
            "--corpus",
            "tests/control-flow/custom.json",
            "--timeout-ms",
            "425",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::ControlFlowDifferential(
                ControlFlowDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/control-flow/custom.json"),
                    timeout: Duration::from_millis(425),
                }
            ))
        );
    }

    #[test]
    fn parses_function_apply_differential_options() {
        let arguments = [
            "function-apply-differential",
            "--oracle",
            "/tmp/qjs",
            "--corpus",
            "tests/function-apply/custom.json",
            "--timeout-ms",
            "450",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::FunctionApplyDifferential(
                FunctionApplyDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/function-apply/custom.json"),
                    timeout: Duration::from_millis(450),
                }
            ))
        );
    }

    #[test]
    fn parses_error_differential_options() {
        let arguments = [
            "error-differential",
            "--oracle",
            "/tmp/qjs",
            "--corpus",
            "tests/error/custom.json",
            "--timeout-ms",
            "440",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::ErrorDifferential(ErrorDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/error/custom.json"),
                timeout: Duration::from_millis(440),
            }))
        );
    }

    #[test]
    fn error_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["error-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::ErrorDifferential(ErrorDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/error/manifest.json"),
                timeout: Duration::from_secs(5),
            }))
        );
    }

    #[test]
    fn rejects_unbounded_error_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "error-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("error --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn function_apply_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["function-apply-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::FunctionApplyDifferential(
                FunctionApplyDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/function-apply/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn function_bind_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["function-bind-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::FunctionBindDifferential(
                FunctionBindDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/function-bind/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_function_apply_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "function-apply-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("function-apply --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn rejects_unbounded_control_flow_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "control-flow-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("control-flow --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn parses_only_the_exact_internal_control_flow_worker_command() {
        assert_eq!(
            Args::parse(
                [OsString::from(
                    crate::control_flow_differential::CANDIDATE_WORKER_COMMAND
                )]
                .into_iter()
            ),
            Ok(Args::ControlFlowCandidateWorker {
                read_async_result: false,
            })
        );
        for argument in ["unexpected", "--help"] {
            assert_eq!(
                Args::parse(
                    [
                        crate::control_flow_differential::CANDIDATE_WORKER_COMMAND,
                        argument,
                    ]
                    .into_iter()
                    .map(OsString::from)
                ),
                Err("internal control-flow candidate worker accepts no arguments".to_owned())
            );
        }
    }

    #[test]
    fn rejects_missing_required_options() {
        let arguments = ["differential"].into_iter().map(OsString::from);
        assert_eq!(
            Args::parse(arguments),
            Err("missing required --oracle PATH".to_owned())
        );
    }

    #[test]
    fn bounded_child_output_rejects_the_first_excess_byte() {
        assert_eq!(
            read_all(Cursor::new(b"1234"), Some(4)).expect("output at limit"),
            b"1234"
        );
        let error = read_all(Cursor::new(b"12345"), Some(4)).expect_err("excess output");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn discovers_javascript_files_recursively_and_ignores_other_files() {
        let root = unique_temp_dir("discovery");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).expect("create fixture directory");
        fs::write(root.join("a.js"), "").expect("write script fixture");
        fs::write(nested.join("b.mjs"), "").expect("write module fixture");
        fs::write(nested.join("notes.txt"), "").expect("write ignored fixture");

        let mut files = Vec::new();
        collect_javascript_files(&root, &mut files).expect("discover fixtures");
        files.sort();
        assert_eq!(files, [root.join("a.js"), nested.join("b.mjs")]);

        fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn terminates_a_timed_out_process() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeping process");

        assert_eq!(
            wait_with_timeout(&mut child, Duration::from_millis(10)),
            Ok(Status::TimedOut)
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_child_input_preserves_the_killable_timeout_path() {
        let arguments = [OsStr::new("-c"), OsStr::new("while :; do :; done")];
        let output = run_program_with_arguments_bounded_input(
            Path::new("sh"),
            &arguments,
            b"return \"bounded\";",
            Duration::from_millis(10),
            32,
        )
        .expect("timed-out child with bounded input");
        assert_eq!(output.status, Status::TimedOut);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "quickjs-xtask-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn parses_iterator_differential_options() {
        let arguments = [
            "iterator-differential",
            "--oracle",
            "/tmp/qjs",
            "--corpus",
            "tests/iterator/custom.json",
            "--timeout-ms",
            "475",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::IteratorDifferential(IteratorDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/iterator/custom.json"),
                timeout: Duration::from_millis(475),
            }))
        );
    }

    #[test]
    fn parses_generator_differential_options() {
        let arguments = [
            "generator-differential",
            "--oracle",
            "/tmp/qjs",
            "--corpus",
            "tests/generator/custom.json",
            "--timeout-ms",
            "465",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::GeneratorDifferential(GeneratorDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/generator/custom.json"),
                timeout: Duration::from_millis(465),
            }))
        );
    }

    #[test]
    fn iterator_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["iterator-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::IteratorDifferential(IteratorDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/iterator/manifest.json"),
                timeout: Duration::from_secs(5),
            }))
        );
    }

    #[test]
    fn object_legacy_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["object-legacy-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::ObjectLegacyDifferential(
                ObjectLegacyDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/object-legacy/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_object_legacy_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "object-legacy-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("object-legacy --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn promise_core_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["promise-core-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::PromiseCoreDifferential(
                PromiseCoreDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/promise-core/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_promise_core_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "promise-core-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("promise-core --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn string_html_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["string-html-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::StringHtmlDifferential(
                StringHtmlDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/string-html/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_string_html_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "string-html-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("string-html --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn string_replace_all_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["string-replace-all-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::StringReplaceAllDifferential(
                StringReplaceAllDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/string-replace-all/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_string_replace_all_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "string-replace-all-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("string-replace-all --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn string_split_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["string-split-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::StringSplitDifferential(
                StringSplitDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/string-split/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_string_split_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "string-split-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("string-split --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn map_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["map-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::MapDifferential(MapDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/map/manifest.json"),
                timeout: Duration::from_secs(5),
            }))
        );
    }

    #[test]
    fn rejects_unbounded_map_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "map-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("map --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn set_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["set-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::SetDifferential(SetDifferentialOptions {
                oracle: PathBuf::from("/tmp/qjs"),
                corpus: PathBuf::from("tests/set/manifest.json"),
                timeout: Duration::from_secs(5),
            }))
        );
    }

    #[test]
    fn rejects_unbounded_set_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "set-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("set --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn weak_collections_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["weak-collections-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::WeakCollectionsDifferential(
                WeakCollectionsDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/weak-collections/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_weak_collections_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "weak-collections-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("weak-collections --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn weak_references_differential_uses_the_pinned_corpus_by_default() {
        let arguments = ["weak-references-differential", "--oracle", "/tmp/qjs"]
            .into_iter()
            .map(OsString::from);

        assert_eq!(
            Args::parse(arguments),
            Ok(Args::WeakReferencesDifferential(
                WeakReferencesDifferentialOptions {
                    oracle: PathBuf::from("/tmp/qjs"),
                    corpus: PathBuf::from("tests/weak-references/manifest.json"),
                    timeout: Duration::from_secs(5),
                }
            ))
        );
    }

    #[test]
    fn rejects_unbounded_weak_references_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "weak-references-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("weak-references --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }

    #[test]
    fn rejects_unbounded_iterator_timeouts() {
        for timeout in ["0", "60001"] {
            let arguments = [
                "iterator-differential",
                "--oracle",
                "/tmp/qjs",
                "--timeout-ms",
                timeout,
            ]
            .into_iter()
            .map(OsString::from);

            assert_eq!(
                Args::parse(arguments),
                Err("iterator --timeout-ms must be between 1 and 60000".to_owned())
            );
        }
    }
}
