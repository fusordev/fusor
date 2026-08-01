//! Repository development tasks.

#![forbid(unsafe_code)]

mod control_flow_differential;
mod dynamic_function_differential;
mod number_radix_differential;
mod parser_differential;

use control_flow_differential::{
    CANDIDATE_WORKER_COMMAND, ControlFlowDifferentialOptions, DEFAULT_CONTROL_FLOW_CORPUS,
    DEFAULT_ERROR_CORPUS, DEFAULT_FUNCTION_APPLY_CORPUS, DEFAULT_FUNCTION_BIND_CORPUS,
    DEFAULT_ITERATOR_CORPUS, ErrorDifferentialOptions, FunctionApplyDifferentialOptions,
    FunctionBindDifferentialOptions, IteratorDifferentialOptions, MAX_CONTROL_FLOW_TIMEOUT_MS,
    run_control_flow_candidate_worker, run_control_flow_differential, run_error_differential,
    run_function_apply_differential, run_function_bind_differential, run_iterator_differential,
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

const DEFAULT_CORPUS: &str = "tests/differential";
const DEFAULT_PARSER_CORPUS: &str = "tests/parser";
const DEFAULT_DYNAMIC_FUNCTION_CORPUS: &str = "tests/dynamic-function";
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

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
        Ok(Args::IteratorDifferential(options)) => match run_iterator_differential(&options) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(Args::ControlFlowCandidateWorker) => match run_control_flow_candidate_worker() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("xtask: {error}");
                ExitCode::FAILURE
            }
        },
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
  cargo xtask control-flow-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask error-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask function-apply-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask function-bind-differential --oracle QJS_PATH [OPTIONS]
  cargo xtask iterator-differential --oracle QJS_PATH [OPTIONS]

Options:
  --corpus PATH       Corpus directory (runtime default: {DEFAULT_CORPUS};
                      parser default: {DEFAULT_PARSER_CORPUS};
                      dynamic Function default: {DEFAULT_DYNAMIC_FUNCTION_CORPUS};
                      Number radix manifest default: {DEFAULT_NUMBER_RADIX_CORPUS};
                      control-flow manifest default: {DEFAULT_CONTROL_FLOW_CORPUS};
                      Error manifest default: {DEFAULT_ERROR_CORPUS};
                      Function.prototype.apply manifest default: {DEFAULT_FUNCTION_APPLY_CORPUS};
                      iterator manifest default: {DEFAULT_ITERATOR_CORPUS})
  --timeout-ms N      Per-process timeout (default: {DEFAULT_TIMEOUT_MS})
  -h, --help          Show this help

Dynamic Function --oracle must be the pinned QuickJS 2026-06-04 qjsc compiler.
It is invoked with -c only; generated code is never compiled or executed.
Number radix --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter.
Control flow --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Error --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Function.prototype.apply --oracle must be the pinned QuickJS 2026-06-04 qjs
interpreter; its timeout must be 1..={MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds.
Iterator --oracle must be the pinned QuickJS 2026-06-04 qjs interpreter;
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
    ControlFlowDifferential(ControlFlowDifferentialOptions),
    ErrorDifferential(ErrorDifferentialOptions),
    FunctionApplyDifferential(FunctionApplyDifferentialOptions),
    FunctionBindDifferential(FunctionBindDifferentialOptions),
    IteratorDifferential(IteratorDifferentialOptions),
    ControlFlowCandidateWorker,
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
                Ok(Self::ControlFlowCandidateWorker)
            } else {
                Err("internal control-flow candidate worker accepts no arguments".to_owned())
            };
        }
        if arguments
            .iter()
            .any(|argument| argument == "-h" || argument == "--help")
        {
            return Ok(Self::Help);
        }
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
            "iterator-differential" => parse_iterator_differential_options(arguments.into_iter())
                .map(Self::IteratorDifferential),
            unknown => Err(format!("unknown task `{unknown}`")),
        }
    }
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
    use crate::control_flow_differential::IteratorDifferentialOptions;
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
            Ok(Args::ControlFlowCandidateWorker)
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
