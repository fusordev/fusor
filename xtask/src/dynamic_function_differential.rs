//! Differential checks for dynamic Function-constructor source preparation.

use crate::{ProgramOutput, Status, run_program_with_arguments_bounded, validate_executable};
use quickjs_frontend::{
    DiagnosticStage, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const EXPECTED_COMPILER_ORACLE_BANNER: &str = "QuickJS Compiler version 2026-06-04";
const MAX_FIXTURES: usize = 256;
const MAX_CORPUS_DEPTH: usize = 16;
const MAX_FIXTURE_BYTES: usize = 64 * 1024;
const MAX_PARAMETER_FRAGMENTS: usize = 256;
const MAX_FRAGMENT_BYTES: usize = 16 * 1024;
const MAX_GENERATED_WRAPPER_BYTES: usize = 32 * 1024;
const MAX_ORACLE_STREAM_BYTES: usize = 16 * 1024;
const MAX_ORACLE_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TEMP_DIRECTORY_ATTEMPTS: u64 = 128;

static TEMP_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DynamicFunctionDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Expectation {
    Accept,
    Reject,
}

impl Expectation {
    const fn matches(self, accepted: bool) -> bool {
        matches!(
            (self, accepted),
            (Self::Accept, true) | (Self::Reject, false)
        )
    }
}

impl fmt::Display for Expectation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct DynamicFunctionFixture {
    path: PathBuf,
    kind: DynamicFunctionKind,
    parameters: Vec<String>,
    body: String,
    expectation: Expectation,
}

#[derive(Debug)]
struct Observation {
    accepted: bool,
    detail: String,
}

#[derive(Debug)]
struct CandidateObservation {
    observation: Observation,
    generated_source: String,
}

pub(crate) fn run_dynamic_function_differential(
    options: &DynamicFunctionDifferentialOptions,
) -> Result<bool, String> {
    validate_executable(&options.oracle, "dynamic-function compiler oracle")?;
    validate_compiler_oracle_release(&options.oracle, options.timeout)?;
    let fixtures = collect_fixtures(&options.corpus)?;

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    for fixture in &fixtures {
        let candidate = observe_candidate(fixture)?;
        let oracle = observe_oracle(
            &options.oracle,
            fixture,
            &candidate.generated_source,
            options.timeout,
        )?;
        if fixture.expectation.matches(candidate.observation.accepted)
            && fixture.expectation.matches(oracle.accepted)
        {
            passed += 1;
        } else {
            failures.push(format_failure(fixture, &oracle, &candidate.observation));
        }
    }

    if failures.is_empty() {
        println!(
            "dynamic-function differential: {passed}/{} fixtures match",
            fixtures.len()
        );
        return Ok(true);
    }

    for failure in &failures {
        eprintln!("{failure}");
    }
    eprintln!(
        "dynamic-function differential: {passed}/{} fixtures match; {} mismatch(es)",
        fixtures.len(),
        failures.len()
    );
    Ok(false)
}

fn collect_fixtures(corpus: &Path) -> Result<Vec<DynamicFunctionFixture>, String> {
    let mut paths = Vec::new();
    collect_json_files(corpus, 0, &mut paths).map_err(|error| {
        format!(
            "failed to read dynamic-function corpus {}: {error}",
            corpus.display()
        )
    })?;
    paths.sort();
    if paths.is_empty() {
        return Err(format!(
            "dynamic-function corpus {} contains no .json fixtures",
            corpus.display()
        ));
    }
    if paths.len() > MAX_FIXTURES {
        return Err(format!(
            "dynamic-function corpus {} contains {} fixtures; the limit is {MAX_FIXTURES}",
            corpus.display(),
            paths.len()
        ));
    }
    paths
        .into_iter()
        .map(|path| read_fixture(corpus, path))
        .collect()
}

fn collect_json_files(
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    if depth > MAX_CORPUS_DEPTH {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("corpus nesting exceeds {MAX_CORPUS_DEPTH} directories"),
        ));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_json_files(&path, depth + 1, output)?;
        } else if file_type.is_file() && path.extension().and_then(OsStr::to_str) == Some("json") {
            output.push(path);
            if output.len() > MAX_FIXTURES {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("corpus contains more than {MAX_FIXTURES} fixtures"),
                ));
            }
        }
    }
    Ok(())
}

fn read_fixture(corpus: &Path, path: PathBuf) -> Result<DynamicFunctionFixture, String> {
    let expectation = classify_expectation(corpus, &path)?;
    let bytes = read_bounded(&path)?;
    if contains_json_unicode_escape(&bytes) {
        return Err(format!(
            "dynamic-function fixture {} uses a JSON Unicode escape; use literal scalar-valid UTF-8 so lone surrogates cannot be decoded lossily",
            path.display()
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "dynamic-function fixture {} is invalid JSON: {error}",
            path.display()
        )
    })?;
    parse_fixture_value(path, expectation, value)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect fixture {}: {error}", path.display()))?;
    if metadata.len() > MAX_FIXTURE_BYTES as u64 {
        return Err(format!(
            "dynamic-function fixture {} contains {} bytes; the limit is {MAX_FIXTURE_BYTES}",
            path.display(),
            metadata.len()
        ));
    }
    let requested = MAX_FIXTURE_BYTES + 1;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(requested).map_err(|_| {
        format!(
            "cannot reserve {requested} bytes for dynamic-function fixture {}",
            path.display()
        )
    })?;
    File::open(path)
        .map_err(|error| format!("cannot open fixture {}: {error}", path.display()))?
        .take(requested as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read fixture {}: {error}", path.display()))?;
    if bytes.len() > MAX_FIXTURE_BYTES {
        return Err(format!(
            "dynamic-function fixture {} grew beyond the {MAX_FIXTURE_BYTES}-byte limit while reading",
            path.display()
        ));
    }
    Ok(bytes)
}

fn classify_expectation(corpus: &Path, path: &Path) -> Result<Expectation, String> {
    let relative = path.strip_prefix(corpus).map_err(|_| {
        format!(
            "dynamic-function fixture {} is outside corpus {}",
            path.display(),
            corpus.display()
        )
    })?;
    match relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
    {
        Some("accept") => Ok(Expectation::Accept),
        Some("reject") => Ok(Expectation::Reject),
        _ => Err(format!(
            "dynamic-function fixture {} must be under accept/ or reject/",
            path.display()
        )),
    }
}

fn parse_fixture_value(
    path: PathBuf,
    expectation: Expectation,
    value: Value,
) -> Result<DynamicFunctionFixture, String> {
    let Value::Object(mut object) = value else {
        return Err(format!(
            "dynamic-function fixture {} must be a JSON object",
            path.display()
        ));
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "kind" | "parameters" | "body"))
    {
        return Err(format!(
            "dynamic-function fixture {} has unknown field `{unknown}`",
            path.display()
        ));
    }
    let kind = take_string(&mut object, "kind", &path)?;
    let kind = parse_kind(&kind).ok_or_else(|| {
        format!(
            "dynamic-function fixture {} has unsupported kind `{kind}`",
            path.display()
        )
    })?;
    let parameters = match object.remove("parameters") {
        Some(Value::Array(parameters)) => parameters,
        Some(_) => {
            return Err(format!(
                "dynamic-function fixture {} field `parameters` must be an array of strings",
                path.display()
            ));
        }
        None => {
            return Err(format!(
                "dynamic-function fixture {} is missing field `parameters`",
                path.display()
            ));
        }
    };
    if parameters.len() > MAX_PARAMETER_FRAGMENTS {
        return Err(format!(
            "dynamic-function fixture {} has {} parameter fragments; the limit is {MAX_PARAMETER_FRAGMENTS}",
            path.display(),
            parameters.len()
        ));
    }
    let parameters = parameters
        .into_iter()
        .enumerate()
        .map(|(index, value)| match value {
            Value::String(value) => Ok(value),
            _ => Err(format!(
                "dynamic-function fixture {} parameter {index} must be a string",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = take_string(&mut object, "body", &path)?;
    let fragment_bytes = parameters
        .iter()
        .try_fold(body.len(), |total, parameter| {
            total.checked_add(parameter.len())
        })
        .ok_or_else(|| {
            format!(
                "dynamic-function fixture {} fragment byte count overflowed",
                path.display()
            )
        })?;
    if fragment_bytes > MAX_FRAGMENT_BYTES {
        return Err(format!(
            "dynamic-function fixture {} contains {fragment_bytes} fragment bytes; the limit is {MAX_FRAGMENT_BYTES}",
            path.display()
        ));
    }
    Ok(DynamicFunctionFixture {
        path,
        kind,
        parameters,
        body,
        expectation,
    })
}

fn take_string(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
    path: &Path,
) -> Result<String, String> {
    match object.remove(field) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(format!(
            "dynamic-function fixture {} field `{field}` must be a string",
            path.display()
        )),
        None => Err(format!(
            "dynamic-function fixture {} is missing field `{field}`",
            path.display()
        )),
    }
}

fn parse_kind(kind: &str) -> Option<DynamicFunctionKind> {
    match kind {
        "function" => Some(DynamicFunctionKind::Function),
        "generator" => Some(DynamicFunctionKind::GeneratorFunction),
        "async" => Some(DynamicFunctionKind::AsyncFunction),
        "async-generator" => Some(DynamicFunctionKind::AsyncGeneratorFunction),
        _ => None,
    }
}

fn contains_json_unicode_escape(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().any(|(index, byte)| {
        if *byte != b'u' || index == 0 {
            return false;
        }
        let slash_count = bytes[..index]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        slash_count % 2 == 1
    })
}

fn observe_candidate(fixture: &DynamicFunctionFixture) -> Result<CandidateObservation, String> {
    let parameters = fixture
        .parameters
        .iter()
        .map(|parameter| SourceFragment::new(parameter))
        .collect::<Vec<_>>();
    let source = DynamicFunctionSource::new(
        fixture.kind,
        &parameters,
        SourceFragment::new(&fixture.body),
    );
    let limits = FrontendLimits::new(MAX_GENERATED_WRAPPER_BYTES)
        .with_max_dynamic_function_fragments(MAX_PARAMETER_FRAGMENTS + 1);
    match with_dynamic_function_source(source, limits, |_, prepared| {
        prepared.generated_source().to_owned()
    }) {
        Ok(generated_source) => Ok(CandidateObservation {
            observation: Observation {
                accepted: true,
                detail: "accepted".to_owned(),
            },
            generated_source,
        }),
        Err(error)
            if matches!(
                error.stage(),
                DiagnosticStage::Parser | DiagnosticStage::Profile | DiagnosticStage::Semantic
            ) =>
        {
            let messages = error
                .diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            let generated_source = error
                .prepared_source()
                .ok_or_else(|| {
                    format!(
                        "Oxc candidate rejected {} without retaining its generated wrapper",
                        fixture.path.display()
                    )
                })?
                .generated_source()
                .to_owned();
            Ok(CandidateObservation {
                observation: Observation {
                    accepted: false,
                    detail: format!("{}: {messages}", error.stage()),
                },
                generated_source,
            })
        }
        Err(error) => Err(format!(
            "Oxc candidate could not classify {}: {error}",
            fixture.path.display()
        )),
    }
}

fn observe_oracle(
    executable: &Path,
    fixture: &DynamicFunctionFixture,
    generated_source: &str,
    timeout: Duration,
) -> Result<Observation, String> {
    let temporary = TempOracleCase::create()?;
    let result = (|| {
        temporary.write_source(generated_source)?;
        let output = run_program_with_arguments_bounded(
            executable,
            &[
                OsStr::new("-c"),
                OsStr::new("-o"),
                temporary.output_path().as_os_str(),
                temporary.input_path().as_os_str(),
            ],
            timeout,
            MAX_ORACLE_STREAM_BYTES,
        )?;
        let output_exists = temporary.inspect_artifacts()?;
        classify_compiler_output(&fixture.path, &output, output_exists)
    })();
    let cleanup = temporary.cleanup();
    match (result, cleanup) {
        (Ok(observation), Ok(())) => Ok(observation),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(format!(
            "dynamic-function compiler oracle cleanup failed for {}: {cleanup}",
            fixture.path.display()
        )),
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; compiler-oracle cleanup also failed: {cleanup}"
        )),
    }
}

fn validate_compiler_oracle_release(executable: &Path, timeout: Duration) -> Result<(), String> {
    let output = run_program_with_arguments_bounded(
        executable,
        &[OsStr::new("--help")],
        timeout,
        MAX_ORACLE_STREAM_BYTES,
    )?;
    if !matches!(output.status, Status::Exited(Some(0 | 1))) {
        return Err(format!(
            "dynamic-function compiler oracle {} could not report its version: status={:?}",
            executable.display(),
            output.status
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout
        .lines()
        .chain(stderr.lines())
        .any(|line| line.trim() == EXPECTED_COMPILER_ORACLE_BANNER)
    {
        Ok(())
    } else {
        Err(format!(
            "dynamic-function compiler oracle {} is not the pinned release; expected banner `{EXPECTED_COMPILER_ORACLE_BANNER}`",
            executable.display()
        ))
    }
}

fn classify_compiler_output(
    path: &Path,
    output: &ProgramOutput,
    output_exists: bool,
) -> Result<Observation, String> {
    match &output.status {
        Status::Exited(Some(0))
            if output.stdout.is_empty() && output.stderr.is_empty() && output_exists =>
        {
            Ok(Observation {
                accepted: true,
                detail: "accepted".to_owned(),
            })
        }
        Status::Exited(Some(_))
            if output.stdout.is_empty()
                && String::from_utf8_lossy(&output.stderr)
                    .trim_start()
                    .starts_with("SyntaxError:") =>
        {
            Ok(Observation {
                accepted: false,
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
        _ => Err(format!(
            "dynamic-function compiler oracle could not classify {}: status={:?}; output_exists={output_exists}; stderr={}; stdout={}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        )),
    }
}

struct TempOracleCase {
    directory: PathBuf,
    input: PathBuf,
    output: PathBuf,
    cleaned: bool,
}

impl TempOracleCase {
    fn create() -> Result<Self, String> {
        let root = env::temp_dir();
        for _ in 0..MAX_TEMP_DIRECTORY_ATTEMPTS {
            let counter = TEMP_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let directory = root.join(format!(
                "quickjs-dynamic-function-qjsc-{}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        input: directory.join("wrapper.js"),
                        output: directory.join("bytecode.c"),
                        directory,
                        cleaned: false,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "cannot create compiler-oracle temporary directory {}: {error}",
                        directory.display()
                    ));
                }
            }
        }
        Err(format!(
            "cannot create a unique compiler-oracle temporary directory after {MAX_TEMP_DIRECTORY_ATTEMPTS} attempts"
        ))
    }

    fn input_path(&self) -> &Path {
        &self.input
    }

    fn output_path(&self) -> &Path {
        &self.output
    }

    fn write_source(&self, source: &str) -> Result<(), String> {
        if source.len() > MAX_GENERATED_WRAPPER_BYTES {
            return Err(format!(
                "generated wrapper contains {} UTF-8 bytes; the compiler-oracle input limit is {MAX_GENERATED_WRAPPER_BYTES}",
                source.len()
            ));
        }
        let mut input = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.input)
            .map_err(|error| {
                format!(
                    "cannot create compiler-oracle input {}: {error}",
                    self.input.display()
                )
            })?;
        input.write_all(source.as_bytes()).map_err(|error| {
            format!(
                "cannot write compiler-oracle input {}: {error}",
                self.input.display()
            )
        })
    }

    fn inspect_artifacts(&self) -> Result<bool, String> {
        for entry in fs::read_dir(&self.directory).map_err(|error| {
            format!(
                "cannot inspect compiler-oracle directory {}: {error}",
                self.directory.display()
            )
        })? {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot inspect compiler-oracle directory {}: {error}",
                    self.directory.display()
                )
            })?;
            let path = entry.path();
            if path != self.input && path != self.output {
                return Err(format!(
                    "compiler oracle created unexpected artifact {}",
                    path.display()
                ));
            }
        }
        let input = fs::symlink_metadata(&self.input).map_err(|error| {
            format!(
                "cannot inspect compiler-oracle input {}: {error}",
                self.input.display()
            )
        })?;
        if !input.file_type().is_file() {
            return Err(format!(
                "compiler-oracle input {} is not a regular file",
                self.input.display()
            ));
        }
        match fs::symlink_metadata(&self.output) {
            Ok(output) => {
                if !output.file_type().is_file() {
                    return Err(format!(
                        "compiler-oracle output {} is not a regular file",
                        self.output.display()
                    ));
                }
                if output.len() > MAX_ORACLE_OUTPUT_BYTES {
                    return Err(format!(
                        "compiler-oracle output {} contains {} bytes; the limit is {MAX_ORACLE_OUTPUT_BYTES}",
                        self.output.display(),
                        output.len()
                    ));
                }
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!(
                "cannot inspect compiler-oracle output {}: {error}",
                self.output.display()
            )),
        }
    }

    fn cleanup(mut self) -> Result<(), String> {
        let result = cleanup_temp_paths(&self.directory, &self.input, &self.output);
        self.cleaned = result.is_ok();
        result
    }
}

impl Drop for TempOracleCase {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = cleanup_temp_paths(&self.directory, &self.input, &self.output);
        }
    }
}

fn cleanup_temp_paths(directory: &Path, input: &Path, output: &Path) -> Result<(), String> {
    for path in [output, input] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot delete temporary artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    match fs::remove_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot delete temporary directory {}: {error}",
            directory.display()
        )),
    }
}

fn format_failure(
    fixture: &DynamicFunctionFixture,
    oracle: &Observation,
    candidate: &Observation,
) -> String {
    format!(
        "--- {}\nkind: {:?}\nexpected: {}\nQuickJS: {} ({})\nOxc front end: {} ({})",
        fixture.path.display(),
        fixture.kind,
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
        DynamicFunctionFixture, Expectation, MAX_FIXTURE_BYTES, TempOracleCase,
        classify_compiler_output, classify_expectation, collect_fixtures,
        contains_json_unicode_escape, observe_candidate, parse_fixture_value,
    };
    use crate::{ProgramOutput, Status};
    use quickjs_frontend::DynamicFunctionKind;
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn classifies_expectations_and_strict_fixture_schema() {
        let corpus = Path::new("tests/dynamic-function");
        let path = PathBuf::from("tests/dynamic-function/accept/basic.json");
        assert_eq!(classify_expectation(corpus, &path), Ok(Expectation::Accept));
        assert_eq!(
            parse_fixture_value(
                path.clone(),
                Expectation::Accept,
                json!({
                    "kind": "async-generator",
                    "parameters": ["left", "right"],
                    "body": "return left + right;"
                }),
            ),
            Ok(DynamicFunctionFixture {
                path,
                kind: DynamicFunctionKind::AsyncGeneratorFunction,
                parameters: vec!["left".to_owned(), "right".to_owned()],
                body: "return left + right;".to_owned(),
                expectation: Expectation::Accept,
            })
        );
        assert!(
            parse_fixture_value(
                PathBuf::from("fixture.json"),
                Expectation::Accept,
                json!({
                    "kind": "function",
                    "parameters": [],
                    "body": "",
                    "typo": true
                }),
            )
            .unwrap_err()
            .contains("unknown field")
        );
    }

    #[test]
    fn rejects_json_unicode_escapes_but_allows_literal_unicode_and_escaped_backslashes() {
        assert!(contains_json_unicode_escape(br#"{"body":"\u03c0"}"#));
        assert!(!contains_json_unicode_escape("{\"body\":\"π\"}".as_bytes()));
        assert!(!contains_json_unicode_escape(br#"{"body":"\\u03c0"}"#));
    }

    #[test]
    fn candidate_retains_the_exact_wrapper_for_parse_only_compilation() {
        let fixture = DynamicFunctionFixture {
            path: PathBuf::from("quotes.json"),
            kind: DynamicFunctionKind::GeneratorFunction,
            parameters: vec!["value = \"quoted\"".to_owned()],
            body: "yield value;\n".to_owned(),
            expectation: Expectation::Accept,
        };
        let candidate = observe_candidate(&fixture).expect("bounded candidate");
        assert!(candidate.observation.accepted);
        assert_eq!(
            candidate.generated_source,
            "(function* anonymous(value = \"quoted\"\n) {\nyield value;\n\n})"
        );
    }

    #[test]
    fn candidate_matches_every_declared_expectation() {
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/dynamic-function");
        let fixtures = collect_fixtures(&corpus).expect("valid dynamic-function corpus");
        let mismatches = fixtures
            .iter()
            .filter_map(|fixture| {
                let candidate = observe_candidate(fixture).expect("bounded candidate");
                let observation = candidate.observation;
                (!fixture.expectation.matches(observation.accepted)).then(|| {
                    format!(
                        "{} expected {} but candidate {}: {}",
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
    fn compiler_observations_are_exact_and_timeouts_are_infrastructure_errors() {
        let path = Path::new("fixture.json");
        let accepted = ProgramOutput {
            status: Status::Exited(Some(0)),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(
            classify_compiler_output(path, &accepted, true)
                .expect("successful compile")
                .accepted
        );
        let rejected = ProgramOutput {
            status: Status::Exited(Some(1)),
            stdout: Vec::new(),
            stderr: b"SyntaxError: unexpected token\n".to_vec(),
        };
        assert!(
            !classify_compiler_output(path, &rejected, true)
                .expect("syntax rejection")
                .accepted
        );
        let timeout = ProgramOutput {
            status: Status::TimedOut,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(classify_compiler_output(path, &timeout, false).is_err());
    }

    #[test]
    fn temporary_compiler_artifacts_are_inspected_and_cleaned() {
        let temporary = TempOracleCase::create().expect("unique temporary directory");
        let directory = temporary.directory.clone();
        temporary
            .write_source("(function anonymous(\n) {\n\n})")
            .expect("bounded source");
        fs::write(temporary.output_path(), b"generated").expect("simulated compiler output");
        assert_eq!(temporary.inspect_artifacts(), Ok(true));
        temporary.cleanup().expect("exact artifact cleanup");
        assert!(!directory.exists());
    }

    #[test]
    fn fixture_byte_limit_is_small_enough_for_bounded_tooling() {
        assert_eq!(MAX_FIXTURE_BYTES, 64 * 1024);
    }
}
