//! Differential checks for the Oxc/QuickJS syntax boundary.

use crate::{ProgramOutput, Status};
use crate::{
    collect_javascript_files, run_program_with_arguments, run_program_with_arguments_bounded,
    validate_executable,
};
use quickjs_frontend::{Allocator, FrontendOptions, ParseMode, parse};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const EXPECTED_ORACLE_BANNER: &str = "QuickJS version 2026-06-04";
const MAX_ORACLE_VERSION_STREAM_BYTES: usize = 16 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ParserDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Expectation {
    Accept,
    Reject,
}

impl fmt::Display for Expectation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        })
    }
}

impl Expectation {
    const fn matches(self, accepted: bool) -> bool {
        matches!(
            (self, accepted),
            (Self::Accept, true) | (Self::Reject, false)
        )
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ParserFixture {
    path: PathBuf,
    mode: ParseMode,
    expectation: Expectation,
}

pub(crate) fn run_parser_differential(options: &ParserDifferentialOptions) -> Result<bool, String> {
    validate_executable(&options.oracle, "parser oracle")?;
    validate_oracle_release(&options.oracle, options.timeout)?;
    let fixtures = collect_parser_fixtures(&options.corpus)?;

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    for fixture in &fixtures {
        let candidate = observe_candidate(fixture)?;
        let oracle = observe_oracle(&options.oracle, fixture, options.timeout)?;

        if fixture.expectation.matches(candidate.accepted)
            && fixture.expectation.matches(oracle.accepted)
        {
            passed += 1;
        } else {
            failures.push(format_failure(fixture, &oracle, &candidate));
        }
    }

    if failures.is_empty() {
        println!(
            "parser differential: {passed}/{} fixtures match",
            fixtures.len()
        );
        return Ok(true);
    }

    for failure in &failures {
        eprintln!("{failure}");
    }
    eprintln!(
        "parser differential: {passed}/{} fixtures match; {} mismatch(es)",
        fixtures.len(),
        failures.len()
    );
    Ok(false)
}

fn collect_parser_fixtures(corpus: &Path) -> Result<Vec<ParserFixture>, String> {
    let mut paths = Vec::new();
    collect_javascript_files(corpus, &mut paths)
        .map_err(|error| format!("failed to read parser corpus {}: {error}", corpus.display()))?;
    paths.sort();

    if paths.is_empty() {
        return Err(format!(
            "parser corpus {} contains no .js or .mjs files",
            corpus.display()
        ));
    }

    paths
        .into_iter()
        .map(|path| classify_fixture(corpus, path))
        .collect()
}

fn classify_fixture(corpus: &Path, path: PathBuf) -> Result<ParserFixture, String> {
    let relative = path.strip_prefix(corpus).map_err(|_| {
        format!(
            "parser fixture {} is outside corpus {}",
            path.display(),
            corpus.display()
        )
    })?;
    let mut components = relative.components();
    let expectation = match components.next().and_then(|part| part.as_os_str().to_str()) {
        Some("accept") => Expectation::Accept,
        Some("reject") => Expectation::Reject,
        _ => {
            return Err(format!(
                "parser fixture {} must be under accept/ or reject/",
                path.display()
            ));
        }
    };
    let mode = match components.next().and_then(|part| part.as_os_str().to_str()) {
        Some("script") => ParseMode::Script,
        Some("module") => ParseMode::Module,
        _ => {
            return Err(format!(
                "parser fixture {} must be under script/ or module/",
                path.display()
            ));
        }
    };

    Ok(ParserFixture {
        path,
        mode,
        expectation,
    })
}

#[derive(Debug)]
struct Observation {
    accepted: bool,
    detail: String,
}

fn observe_candidate(fixture: &ParserFixture) -> Result<Observation, String> {
    let source = fs::read_to_string(&fixture.path).map_err(|error| {
        format!(
            "failed to read parser fixture {} as UTF-8: {error}",
            fixture.path.display()
        )
    })?;
    let allocator = Allocator::new();
    match parse(&allocator, &source, FrontendOptions::new(fixture.mode)) {
        Ok(_) => Ok(Observation {
            accepted: true,
            detail: "accepted".to_owned(),
        }),
        Err(error) => {
            let messages = error
                .diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            Ok(Observation {
                accepted: false,
                detail: format!("{}: {messages}", error.stage()),
            })
        }
    }
}

fn observe_oracle(
    executable: &Path,
    fixture: &ParserFixture,
    timeout: Duration,
) -> Result<Observation, String> {
    let mode = match fixture.mode {
        ParseMode::Script => OsStr::new("--script"),
        ParseMode::Module => OsStr::new("--module"),
    };
    let output =
        run_program_with_arguments(executable, &[mode, fixture.path.as_os_str()], timeout)?;
    classify_oracle_output(&fixture.path, &output)
}

fn validate_oracle_release(executable: &Path, timeout: Duration) -> Result<(), String> {
    let output = run_program_with_arguments_bounded(
        executable,
        &[OsStr::new("--help")],
        timeout,
        MAX_ORACLE_VERSION_STREAM_BYTES,
    )?;
    if output.status == Status::TimedOut {
        return Err(format!(
            "parser oracle {} timed out while reporting its version",
            executable.display()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout
        .lines()
        .chain(stderr.lines())
        .any(|line| line.trim() == EXPECTED_ORACLE_BANNER)
    {
        Ok(())
    } else {
        Err(format!(
            "parser oracle {} is not the pinned release; expected banner `{EXPECTED_ORACLE_BANNER}`",
            executable.display()
        ))
    }
}

fn classify_oracle_output(fixture: &Path, output: &ProgramOutput) -> Result<Observation, String> {
    match &output.status {
        Status::Exited(Some(0)) => Ok(Observation {
            accepted: true,
            detail: "accepted".to_owned(),
        }),
        Status::Exited(Some(_))
            if String::from_utf8_lossy(&output.stderr)
                .trim_start()
                .starts_with("SyntaxError:") =>
        {
            Ok(Observation {
                accepted: false,
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
        _ => Err(format!(
            "parser oracle could not classify {}: status={:?}; stderr={}; stdout={}",
            fixture.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        )),
    }
}

fn format_failure(
    fixture: &ParserFixture,
    oracle: &Observation,
    candidate: &Observation,
) -> String {
    format!(
        "--- {}\nexpected: {}\nQuickJS: {} ({})\nOxc front end: {} ({})",
        fixture.path.display(),
        fixture.expectation,
        if oracle.accepted { "accept" } else { "reject" },
        oracle.detail,
        if candidate.accepted {
            "accept"
        } else {
            "reject"
        },
        candidate.detail
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Expectation, classify_fixture, classify_oracle_output, collect_parser_fixtures,
        observe_candidate,
    };
    use crate::{ProgramOutput, Status};
    use quickjs_frontend::ParseMode;
    use std::path::{Path, PathBuf};

    #[test]
    fn classifies_expectation_and_parse_mode_from_directories() {
        let corpus = Path::new("tests/parser");
        assert_eq!(
            classify_fixture(
                corpus,
                PathBuf::from("tests/parser/reject/module/new-syntax.mjs"),
            ),
            Ok(super::ParserFixture {
                path: PathBuf::from("tests/parser/reject/module/new-syntax.mjs"),
                mode: ParseMode::Module,
                expectation: Expectation::Reject,
            })
        );
    }

    #[test]
    fn in_process_frontend_matches_every_declared_expectation() {
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/parser");
        let fixtures = collect_parser_fixtures(&corpus).expect("valid parser corpus");

        let mismatches = fixtures
            .iter()
            .filter_map(|fixture| {
                let observation = observe_candidate(fixture).expect("read fixture");
                (!fixture.expectation.matches(observation.accepted)).then(|| {
                    format!(
                        "{} expected {} but frontend {}: {}",
                        fixture.path.display(),
                        fixture.expectation,
                        if observation.accepted {
                            "accepted"
                        } else {
                            "rejected"
                        },
                        observation.detail
                    )
                })
            })
            .collect::<Vec<_>>();

        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn oracle_only_counts_quickjs_syntax_errors_as_rejections() {
        let fixture = Path::new("fixture.js");
        let syntax_error = ProgramOutput {
            status: Status::Exited(Some(1)),
            stdout: Vec::new(),
            stderr: b"SyntaxError: unexpected token\n".to_vec(),
        };
        let runtime_error = ProgramOutput {
            status: Status::Exited(Some(1)),
            stdout: Vec::new(),
            stderr: b"ReferenceError: missing\n".to_vec(),
        };

        assert!(
            !classify_oracle_output(fixture, &syntax_error)
                .unwrap()
                .accepted
        );
        assert!(classify_oracle_output(fixture, &runtime_error).is_err());
    }

    #[test]
    fn oracle_timeout_and_signal_are_infrastructure_failures() {
        let fixture = Path::new("fixture.js");
        for status in [Status::TimedOut, Status::Exited(None)] {
            let output = ProgramOutput {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
            assert!(classify_oracle_output(fixture, &output).is_err());
        }
    }
}
