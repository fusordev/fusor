//! Differential checks for the Oxc/QuickJS syntax boundary.

use crate::{ProgramOutput, Status};
use crate::{
    collect_javascript_files, run_program_with_arguments, run_program_with_arguments_bounded,
    validate_executable,
};
use quickjs_frontend::{
    Allocator, CompilationGoal, FrontendOptions, GlobalScriptGoal, ParseMode, parse,
};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const EXPECTED_ORACLE_BANNER: &str = "QuickJS version 2026-06-04";
const MAX_ORACLE_VERSION_STREAM_BYTES: usize = 16 * 1024;
const ASYNC_SCRIPT_ORACLE: &str =
    "const source = std.loadFile(scriptArgs[0]); std.evalScript(source, { async: true });";
const STRICT_ASYNC_SCRIPT_ORACLE: &str = r#"const raw = std.loadFile(scriptArgs[0]); const split = Number(scriptArgs[1]); const separator = scriptArgs[2] === "1" ? "\n" : ""; const source = raw.slice(0, split) + separator + '"use strict";\n' + raw.slice(split); std.evalScript(source, { async: true });"#;

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
    goal: ParserGoal,
    candidate_expectation: Expectation,
    oracle_expectation: Expectation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParserGoal {
    Script,
    Module,
    StrictScript,
    AsyncScript,
    StrictAsyncScript,
}

impl ParserGoal {
    const fn candidate_options(self) -> FrontendOptions<'static> {
        match self {
            Self::Script => FrontendOptions::new(ParseMode::Script),
            Self::Module => FrontendOptions::new(ParseMode::Module),
            Self::StrictScript => FrontendOptions::for_goal(CompilationGoal::GlobalScript(
                GlobalScriptGoal::new().with_forced_strict(true),
            )),
            Self::AsyncScript => FrontendOptions::for_goal(CompilationGoal::GlobalScript(
                GlobalScriptGoal::new().with_top_level_await(true),
            )),
            Self::StrictAsyncScript => FrontendOptions::for_goal(CompilationGoal::GlobalScript(
                GlobalScriptGoal::new()
                    .with_forced_strict(true)
                    .with_top_level_await(true),
            )),
        }
    }
}

fn strict_async_oracle_insertion(source: &str) -> (usize, bool) {
    if !source.starts_with("#!") {
        return (0, false);
    }

    for (relative_offset, character) in source[2..].char_indices() {
        if matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            let mut byte_offset = 2 + relative_offset + character.len_utf8();
            if character == '\r' && source[byte_offset..].starts_with('\n') {
                byte_offset += 1;
            }
            return (source[..byte_offset].encode_utf16().count(), false);
        }
    }

    (source.encode_utf16().count(), true)
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

        if fixture.candidate_expectation.matches(candidate.accepted)
            && fixture.oracle_expectation.matches(oracle.accepted)
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
    let (candidate_expectation, oracle_expectation) = match components
        .next()
        .and_then(|part| part.as_os_str().to_str())
    {
        Some("accept") => (Expectation::Accept, Expectation::Accept),
        Some("reject") => (Expectation::Reject, Expectation::Reject),
        Some("candidate-accept") => (Expectation::Accept, Expectation::Reject),
        Some("candidate-reject") => (Expectation::Reject, Expectation::Accept),
        _ => {
            return Err(format!(
                "parser fixture {} must be under accept/, reject/, candidate-accept/, or candidate-reject/",
                path.display()
            ));
        }
    };
    let goal = match components.next().and_then(|part| part.as_os_str().to_str()) {
        Some("script") => ParserGoal::Script,
        Some("module") => ParserGoal::Module,
        Some("strict-script") => ParserGoal::StrictScript,
        Some("async-script") => ParserGoal::AsyncScript,
        Some("strict-async-script") => ParserGoal::StrictAsyncScript,
        _ => {
            return Err(format!(
                "parser fixture {} must be under script/, module/, strict-script/, async-script/, or strict-async-script/",
                path.display()
            ));
        }
    };

    Ok(ParserFixture {
        path,
        goal,
        candidate_expectation,
        oracle_expectation,
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
    match parse(&allocator, &source, fixture.goal.candidate_options()) {
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
    let output = match fixture.goal {
        ParserGoal::Script => run_program_with_arguments(
            executable,
            &[OsStr::new("--script"), fixture.path.as_os_str()],
            timeout,
        )?,
        ParserGoal::Module => run_program_with_arguments(
            executable,
            &[OsStr::new("--module"), fixture.path.as_os_str()],
            timeout,
        )?,
        ParserGoal::StrictScript => run_program_with_arguments(
            executable,
            &[
                OsStr::new("--script"),
                OsStr::new("--strict"),
                fixture.path.as_os_str(),
            ],
            timeout,
        )?,
        ParserGoal::AsyncScript => run_program_with_arguments(
            executable,
            &[
                OsStr::new("--std"),
                OsStr::new("--script"),
                OsStr::new("-e"),
                OsStr::new(ASYNC_SCRIPT_ORACLE),
                fixture.path.as_os_str(),
            ],
            timeout,
        )?,
        ParserGoal::StrictAsyncScript => {
            let source = fs::read_to_string(&fixture.path).map_err(|error| {
                format!(
                    "failed to read parser fixture {} as UTF-8: {error}",
                    fixture.path.display()
                )
            })?;
            let (insertion_index, needs_separator) = strict_async_oracle_insertion(&source);
            let insertion_index = insertion_index.to_string();
            run_program_with_arguments(
                executable,
                &[
                    OsStr::new("--std"),
                    OsStr::new("--script"),
                    OsStr::new("-e"),
                    OsStr::new(STRICT_ASYNC_SCRIPT_ORACLE),
                    fixture.path.as_os_str(),
                    OsStr::new(&insertion_index),
                    OsStr::new(if needs_separator { "1" } else { "0" }),
                ],
                timeout,
            )?
        }
    };
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
        "--- {}\nexpected QuickJS: {}\nexpected Oxc front end: {}\nQuickJS: {} ({})\nOxc front end: {} ({})",
        fixture.path.display(),
        fixture.oracle_expectation,
        fixture.candidate_expectation,
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
        observe_candidate, strict_async_oracle_insertion,
    };
    use crate::{ProgramOutput, Status};
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
                goal: super::ParserGoal::Module,
                candidate_expectation: Expectation::Reject,
                oracle_expectation: Expectation::Reject,
            })
        );
        assert_eq!(
            classify_fixture(
                corpus,
                PathBuf::from("tests/parser/candidate-reject/async-script/difference.js"),
            ),
            Ok(super::ParserFixture {
                path: PathBuf::from("tests/parser/candidate-reject/async-script/difference.js",),
                goal: super::ParserGoal::AsyncScript,
                candidate_expectation: Expectation::Reject,
                oracle_expectation: Expectation::Accept,
            })
        );
    }

    #[test]
    fn strict_async_oracle_insertion_matches_quickjs_hashbang_terminators() {
        for (source, expected) in [
            ("statement;", (0, false)),
            ("#!qjs\nstatement;", (6, false)),
            ("#!qjs\rstatement;", (6, false)),
            ("#!qjs\r\nstatement;", (7, false)),
            ("#!qjs\u{2028}statement;", (6, false)),
            ("#!qjs\u{2029}statement;", (6, false)),
            ("#!🦀\rstatement;", (5, false)),
            ("#!qjs", (5, true)),
        ] {
            assert_eq!(
                strict_async_oracle_insertion(source),
                expected,
                "source {source:?}"
            );
        }
    }

    #[test]
    fn in_process_frontend_matches_every_declared_expectation() {
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/parser");
        let fixtures = collect_parser_fixtures(&corpus).expect("valid parser corpus");

        let mismatches = fixtures
            .iter()
            .filter_map(|fixture| {
                let observation = observe_candidate(fixture).expect("read fixture");
                (!fixture.candidate_expectation.matches(observation.accepted)).then(|| {
                    format!(
                        "{} expected {} but frontend {}: {}",
                        fixture.path.display(),
                        fixture.candidate_expectation,
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
