//! Bounded differential gate for `Number.prototype.toString(radix)`.

use crate::{ProgramOutput, Status, run_program_with_arguments_bounded, validate_executable};
use quickjs::{DynamicFunctionLimits, construct_dynamic_function};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{ExecutionLimits, JsNumber, Runtime, RuntimeLimits};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const DEFAULT_NUMBER_RADIX_CORPUS: &str = "tests/number-radix/manifest.json";

const EXPECTED_ORACLE_BANNER: &str = "QuickJS version 2026-06-04";
const EXPECTED_MANIFEST_RELEASE: &str = "2026-06-04";
const MANIFEST_SCHEMA_VERSION: u64 = 1;
const MIN_RADIX: u8 = 2;
const MAX_RADIX: u8 = 36;
const RADIX_COUNT: usize = 35;
const RADIX_COUNT_U64: u64 = 35;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_BOUNDARY_VALUES: usize = 64;
const MAX_SAMPLE_CASES: usize = 512;
const MAX_CASES: usize = 1_024;
const MAX_GENERATED_ORACLE_SOURCE_BYTES: usize = 128 * 1024;
const MAX_ORACLE_VERSION_STREAM_BYTES: usize = 16 * 1024;
const MAX_ORACLE_CASE_STREAM_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESULT_BYTES: usize = 2 * 1024;
const MAX_REPORTED_MISMATCHES: usize = 32;
const MAX_CANDIDATE_PANICS: usize = 32;
const MAX_ERROR_PREVIEW_BYTES: usize = 512;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct NumberRadixDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NumberRadixCase {
    bits: u64,
    radix: u8,
}

#[derive(Debug, Eq, PartialEq)]
struct NumberRadixCorpus {
    sample_seed: u64,
    sample_count: usize,
    boundary_bits: Vec<u64>,
}

enum CandidateObservation {
    Value(String),
    Error(String),
}

pub(crate) fn run_number_radix_differential(
    options: &NumberRadixDifferentialOptions,
) -> Result<bool, String> {
    validate_executable(&options.oracle, "Number radix oracle")?;
    validate_oracle_release(&options.oracle, options.timeout)?;

    let corpus = load_corpus(&options.corpus)?;
    let cases = build_cases(&corpus)?;
    let expected = observe_oracle(&options.oracle, &cases, options.timeout)?;
    let actual = observe_candidate(&cases)?;

    if expected.len() != cases.len() || actual.len() != cases.len() {
        return Err("Number radix differential produced an inconsistent result count".to_owned());
    }

    let mut mismatch_count = 0_usize;
    let mut reported = Vec::new();
    for ((case, expected), actual) in cases.iter().zip(&expected).zip(actual) {
        let matches = matches!(&actual, CandidateObservation::Value(value) if value == expected);
        if matches {
            continue;
        }
        mismatch_count = mismatch_count
            .checked_add(1)
            .ok_or_else(|| "Number radix mismatch count overflowed".to_owned())?;
        if reported.len() < MAX_REPORTED_MISMATCHES {
            reported.push(format_mismatch(*case, expected, &actual));
        }
    }

    let passed = cases
        .len()
        .checked_sub(mismatch_count)
        .ok_or_else(|| "Number radix pass count underflowed".to_owned())?;
    if mismatch_count == 0 {
        println!(
            "number radix differential: {passed}/{} cases match ({} boundary values across radices {MIN_RADIX}..={MAX_RADIX}; {} fixed-seed samples)",
            cases.len(),
            corpus.boundary_bits.len(),
            corpus.sample_count
        );
        return Ok(true);
    }

    for failure in reported {
        eprintln!("{failure}");
    }
    if mismatch_count > MAX_REPORTED_MISMATCHES {
        eprintln!(
            "number radix differential: omitted {} additional mismatch(es)",
            mismatch_count - MAX_REPORTED_MISMATCHES
        );
    }
    eprintln!(
        "number radix differential: {passed}/{} cases match; {mismatch_count} mismatch(es)",
        cases.len()
    );
    Ok(false)
}

fn load_corpus(path: &Path) -> Result<NumberRadixCorpus, String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "cannot inspect Number radix corpus {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "Number radix corpus {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "Number radix corpus {} contains {} bytes; the limit is {MAX_MANIFEST_BYTES}",
            path.display(),
            metadata.len()
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "cannot read Number radix corpus {}: {error}",
            path.display()
        )
    })?;
    parse_corpus(&bytes, &path.display().to_string())
}

fn parse_corpus(bytes: &[u8], location: &str) -> Result<NumberRadixCorpus, String> {
    let manifest: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid Number radix corpus {location}: {error}"))?;
    let root = manifest
        .as_object()
        .ok_or_else(|| format!("Number radix corpus {location} must be a JSON object"))?;
    require_exact_keys(
        root,
        &[
            "schema",
            "quickjs_release",
            "sample_seed",
            "sample_count",
            "boundary_bits",
        ],
        location,
    )?;

    let schema = required_u64(root, "schema", location)?;
    if schema != MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "Number radix corpus {location} has schema {schema}; expected {MANIFEST_SCHEMA_VERSION}"
        ));
    }
    let release = required_string(root, "quickjs_release", location)?;
    if release != EXPECTED_MANIFEST_RELEASE {
        return Err(format!(
            "Number radix corpus {location} targets QuickJS {release}; expected {EXPECTED_MANIFEST_RELEASE}"
        ));
    }
    let sample_seed = parse_hex_u64(
        required_string(root, "sample_seed", location)?,
        &format!("Number radix corpus {location} field `sample_seed`"),
    )?;
    let sample_count_u64 = required_u64(root, "sample_count", location)?;
    let sample_count = usize::try_from(sample_count_u64).map_err(|_| {
        format!("Number radix corpus {location} field `sample_count` does not fit usize")
    })?;
    if sample_count > MAX_SAMPLE_CASES {
        return Err(format!(
            "Number radix corpus {location} requests {sample_count} samples; the limit is {MAX_SAMPLE_CASES}"
        ));
    }

    let boundary_values = root
        .get("boundary_bits")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!("Number radix corpus {location} field `boundary_bits` must be an array")
        })?;
    if boundary_values.is_empty() {
        return Err(format!(
            "Number radix corpus {location} field `boundary_bits` must not be empty"
        ));
    }
    if boundary_values.len() > MAX_BOUNDARY_VALUES {
        return Err(format!(
            "Number radix corpus {location} contains {} boundary values; the limit is {MAX_BOUNDARY_VALUES}",
            boundary_values.len()
        ));
    }

    let mut boundary_bits = Vec::new();
    boundary_bits
        .try_reserve_exact(boundary_values.len())
        .map_err(|_| "cannot reserve Number radix boundary corpus".to_owned())?;
    let mut unique = BTreeSet::new();
    for (index, value) in boundary_values.iter().enumerate() {
        let value = value.as_str().ok_or_else(|| {
            format!(
                "Number radix corpus {location} field `boundary_bits[{index}]` must be a string"
            )
        })?;
        let bits = parse_hex_u64(
            value,
            &format!("Number radix corpus {location} field `boundary_bits[{index}]`"),
        )?;
        if !unique.insert(bits) {
            return Err(format!(
                "Number radix corpus {location} repeats boundary bits 0x{bits:016x}"
            ));
        }
        boundary_bits.push(bits);
    }

    validate_case_count(boundary_bits.len(), sample_count)?;
    Ok(NumberRadixCorpus {
        sample_seed,
        sample_count,
        boundary_bits,
    })
}

fn require_exact_keys(
    object: &Map<String, Value>,
    expected: &[&str],
    location: &str,
) -> Result<(), String> {
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "Number radix corpus {location} fields are {actual:?}; expected {expected:?}"
    ))
}

fn required_u64(object: &Map<String, Value>, field: &str, location: &str) -> Result<u64, String> {
    object.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("Number radix corpus {location} field `{field}` must be an unsigned integer")
    })
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    location: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Number radix corpus {location} field `{field}` must be a string"))
}

fn parse_hex_u64(value: &str, location: &str) -> Result<u64, String> {
    let Some(digits) = value.strip_prefix("0x") else {
        return Err(format!(
            "{location} must use exactly 16 lowercase hexadecimal digits after `0x`"
        ));
    };
    if digits.len() != 16
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{location} must use exactly 16 lowercase hexadecimal digits after `0x`"
        ));
    }
    u64::from_str_radix(digits, 16)
        .map_err(|error| format!("{location} is not a binary64 bit pattern: {error}"))
}

fn validate_case_count(boundary_count: usize, sample_count: usize) -> Result<usize, String> {
    let boundary_cases = boundary_count
        .checked_mul(RADIX_COUNT)
        .ok_or_else(|| "Number radix boundary case count overflowed".to_owned())?;
    let total = boundary_cases
        .checked_add(sample_count)
        .ok_or_else(|| "Number radix total case count overflowed".to_owned())?;
    if total > MAX_CASES {
        return Err(format!(
            "Number radix corpus expands to {total} cases; the limit is {MAX_CASES}"
        ));
    }
    Ok(total)
}

fn build_cases(corpus: &NumberRadixCorpus) -> Result<Vec<NumberRadixCase>, String> {
    let total = validate_case_count(corpus.boundary_bits.len(), corpus.sample_count)?;
    let mut cases = Vec::new();
    cases
        .try_reserve_exact(total)
        .map_err(|_| format!("cannot reserve {total} Number radix cases"))?;
    let mut unique = BTreeSet::new();

    for bits in &corpus.boundary_bits {
        for radix in MIN_RADIX..=MAX_RADIX {
            let case = NumberRadixCase { bits: *bits, radix };
            if !unique.insert(case) {
                return Err(format!(
                    "Number radix corpus generated duplicate boundary case bits=0x{bits:016x} radix={radix}"
                ));
            }
            cases.push(case);
        }
    }

    let mut random = SplitMix64::new(corpus.sample_seed);
    let maximum_attempts = corpus
        .sample_count
        .checked_mul(4)
        .ok_or_else(|| "Number radix sample attempt count overflowed".to_owned())?;
    let mut attempts = 0_usize;
    while cases.len() < total {
        attempts = attempts
            .checked_add(1)
            .ok_or_else(|| "Number radix sample attempt count overflowed".to_owned())?;
        if attempts > maximum_attempts {
            return Err(format!(
                "could not generate {} unique fixed-seed Number radix samples in {maximum_attempts} attempts",
                corpus.sample_count
            ));
        }
        let bits = random.next();
        let radix_offset = u8::try_from(random.next() % RADIX_COUNT_U64)
            .map_err(|_| "Number radix random offset does not fit u8".to_owned())?;
        let radix = MIN_RADIX
            .checked_add(radix_offset)
            .ok_or_else(|| "Number radix random radix overflowed".to_owned())?;
        let case = NumberRadixCase { bits, radix };
        if unique.insert(case) {
            cases.push(case);
        }
    }
    Ok(cases)
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

fn validate_oracle_release(executable: &Path, timeout: Duration) -> Result<(), String> {
    let output = run_program_with_arguments_bounded(
        executable,
        &[OsStr::new("--help")],
        timeout,
        MAX_ORACLE_VERSION_STREAM_BYTES,
    )?;
    if !matches!(output.status, Status::Exited(Some(0 | 1))) {
        return Err(format!(
            "Number radix oracle {} could not report its version: status={:?}",
            executable.display(),
            output.status
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout
        .lines()
        .chain(stderr.lines())
        .any(|line| line.trim() == EXPECTED_ORACLE_BANNER)
    {
        return Ok(());
    }
    Err(format!(
        "Number radix oracle {} is not the pinned release; expected banner `{EXPECTED_ORACLE_BANNER}`",
        executable.display()
    ))
}

fn observe_oracle(
    executable: &Path,
    cases: &[NumberRadixCase],
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let source = build_oracle_source(cases)?;
    let output = run_program_with_arguments_bounded(
        executable,
        &[OsStr::new("-e"), OsStr::new(&source)],
        timeout,
        MAX_ORACLE_CASE_STREAM_BYTES,
    )?;
    classify_oracle_output(executable, cases, &output)
}

fn build_oracle_source(cases: &[NumberRadixCase]) -> Result<String, String> {
    if cases.len() > MAX_CASES {
        return Err(format!(
            "Number radix oracle received {} cases; the limit is {MAX_CASES}",
            cases.len()
        ));
    }
    let mut source = String::from(
        "const __b=new ArrayBuffer(8),__v=new DataView(__b);\
         function __emit(i,h,l,r){\
         __v.setUint32(0,h);__v.setUint32(4,l);\
         print(i+\"\\t\"+Number.prototype.toString.call(__v.getFloat64(0),r));}\n",
    );
    for (index, case) in cases.iter().enumerate() {
        let high = u32::try_from(case.bits >> 32)
            .map_err(|_| "Number radix high bits do not fit u32".to_owned())?;
        let low = u32::try_from(case.bits & u64::from(u32::MAX))
            .map_err(|_| "Number radix low bits do not fit u32".to_owned())?;
        writeln!(
            source,
            "__emit({index},0x{high:08x},0x{low:08x},{});",
            case.radix
        )
        .map_err(|_| "cannot write generated Number radix oracle source".to_owned())?;
        if source.len() > MAX_GENERATED_ORACLE_SOURCE_BYTES {
            return Err(format!(
                "generated Number radix oracle source contains {} bytes; the limit is {MAX_GENERATED_ORACLE_SOURCE_BYTES}",
                source.len()
            ));
        }
    }
    Ok(source)
}

fn classify_oracle_output(
    executable: &Path,
    cases: &[NumberRadixCase],
    output: &ProgramOutput,
) -> Result<Vec<String>, String> {
    if output.status != Status::Exited(Some(0)) {
        return Err(format!(
            "Number radix oracle {} failed: status={:?}; stdout={}; stderr={}",
            executable.display(),
            output.status,
            stream_preview(&output.stdout),
            stream_preview(&output.stderr)
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "Number radix oracle {} wrote unexpected stderr: {}",
            executable.display(),
            stream_preview(&output.stderr)
        ));
    }
    parse_oracle_stdout(&output.stdout, cases.len())
}

fn parse_oracle_stdout(stdout: &[u8], expected_count: usize) -> Result<Vec<String>, String> {
    if expected_count == 0 {
        return if stdout.is_empty() {
            Ok(Vec::new())
        } else {
            Err("Number radix oracle emitted output for an empty corpus".to_owned())
        };
    }
    let text = std::str::from_utf8(stdout)
        .map_err(|error| format!("Number radix oracle stdout is not UTF-8: {error}"))?;
    let text = text.strip_suffix('\n').ok_or_else(|| {
        "Number radix oracle stdout must end with exactly one complete result line".to_owned()
    })?;
    if text.ends_with('\n') || text.contains('\r') {
        return Err(
            "Number radix oracle stdout contains an unexpected blank or CR line".to_owned(),
        );
    }

    let lines = text.split('\n').collect::<Vec<_>>();
    if lines.len() != expected_count {
        return Err(format!(
            "Number radix oracle emitted {} result lines; expected {expected_count}",
            lines.len()
        ));
    }
    let mut results = Vec::new();
    results
        .try_reserve_exact(expected_count)
        .map_err(|_| format!("cannot reserve {expected_count} Number radix oracle results"))?;
    for (expected_index, line) in lines.into_iter().enumerate() {
        let (index, result) = line.split_once('\t').ok_or_else(|| {
            format!("Number radix oracle line {expected_index} has no tab separator")
        })?;
        if index != expected_index.to_string() {
            return Err(format!(
                "Number radix oracle line {expected_index} reports non-canonical index `{index}`"
            ));
        }
        validate_result(result, "oracle", expected_index)?;
        results.push(result.to_owned());
    }
    Ok(results)
}

fn observe_candidate(cases: &[NumberRadixCase]) -> Result<Vec<CandidateObservation>, String> {
    if cases.len() > MAX_CASES {
        return Err(format!(
            "Number radix candidate received {} cases; the limit is {MAX_CASES}",
            cases.len()
        ));
    }

    #[cfg(not(test))]
    let _panic_hook = QuietPanicHook::install();
    let mut observations = Vec::new();
    observations
        .try_reserve_exact(cases.len())
        .map_err(|_| format!("cannot reserve {} candidate results", cases.len()))?;
    let mut next_index = 0_usize;
    let mut panic_count = 0_usize;
    while next_index < cases.len() {
        let mut active_index = next_index;
        let attempt = catch_unwind(AssertUnwindSafe(|| {
            observe_candidate_attempt(cases, next_index, &mut active_index, &mut observations)
        }));
        match attempt {
            Ok(Ok(())) => next_index = cases.len(),
            Ok(Err(error)) => return Err(error),
            Err(payload) => {
                panic_count = panic_count
                    .checked_add(1)
                    .ok_or_else(|| "Number radix candidate panic count overflowed".to_owned())?;
                observations.push(CandidateObservation::Error(format!(
                    "candidate panicked: {}",
                    panic_payload(&payload)
                )));
                next_index = active_index
                    .checked_add(1)
                    .ok_or_else(|| "Number radix candidate case index overflowed".to_owned())?;
                if panic_count >= MAX_CANDIDATE_PANICS && next_index < cases.len() {
                    let skipped = cases.len() - next_index;
                    observations.extend((0..skipped).map(|_| {
                        CandidateObservation::Error(format!(
                            "candidate was not run after the {MAX_CANDIDATE_PANICS}-panic safety limit"
                        ))
                    }));
                    next_index = cases.len();
                }
            }
        }
    }
    Ok(observations)
}

#[cfg(not(test))]
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

#[cfg(not(test))]
struct QuietPanicHook(Option<PanicHook>);

#[cfg(not(test))]
impl QuietPanicHook {
    fn install() -> Self {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        Self(Some(previous))
    }
}

#[cfg(not(test))]
impl Drop for QuietPanicHook {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            std::panic::set_hook(previous);
        }
    }
}

fn observe_candidate_attempt(
    cases: &[NumberRadixCase],
    start_index: usize,
    active_index: &mut usize,
    observations: &mut Vec<CandidateObservation>,
) -> Result<(), String> {
    let mut runtime = Runtime::try_new(RuntimeLimits::default())
        .map_err(|error| format!("cannot create Number radix candidate runtime: {error}"))?;
    let realm = runtime
        .create_realm()
        .map_err(|error| format!("cannot create Number radix candidate realm: {error}"))?;
    let mut context = runtime
        .context(&realm)
        .map_err(|error| format!("cannot create Number radix candidate context: {error}"))?;
    let parameters = [SourceFragment::new("value"), SourceFragment::new("radix")];
    let completion = construct_dynamic_function(
        &mut context,
        DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new("return Number.prototype.toString.call(value,radix);"),
        ),
        DynamicFunctionLimits::default(),
    )
    .map_err(|error| format!("cannot compile Number radix candidate through facade: {error}"))?;
    let formatter = completion.into_value().into_function().map_err(|error| {
        format!("Number radix candidate facade returned a non-function: {error}")
    })?;

    for (index, case) in cases.iter().enumerate().skip(start_index) {
        *active_index = index;
        let value = context.number(JsNumber::from_f64(f64::from_bits(case.bits)));
        let radix = context.number(JsNumber::from_i32(i32::from(case.radix)));
        let observation =
            match context.call(&formatter, &[value, radix], ExecutionLimits::default()) {
                Ok(result) => match result.as_string() {
                    Ok(Some(result)) => match result.to_utf8_lossy() {
                        Ok(result) => match validate_result(&result, "candidate", index) {
                            Ok(()) => CandidateObservation::Value(result),
                            Err(error) => CandidateObservation::Error(error),
                        },
                        Err(error) => CandidateObservation::Error(format!(
                            "cannot decode candidate String as UTF-8: {error}"
                        )),
                    },
                    Ok(None) => CandidateObservation::Error(
                        "facade formatter returned a non-String value".to_owned(),
                    ),
                    Err(error) => CandidateObservation::Error(format!(
                        "cannot inspect facade formatter result: {error}"
                    )),
                },
                Err(error) => CandidateObservation::Error(format!("execution failed: {error}")),
            };
        observations.push(observation);
        *active_index = index
            .checked_add(1)
            .ok_or_else(|| "Number radix candidate case index overflowed".to_owned())?;
    }
    Ok(())
}

fn panic_payload(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return truncate(message);
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return truncate(message);
    }
    "non-string panic payload".to_owned()
}

fn validate_result(result: &str, role: &str, index: usize) -> Result<(), String> {
    if result.is_empty() {
        return Err(format!("{role} result {index} is empty"));
    }
    if result.len() > MAX_RESULT_BYTES {
        return Err(format!(
            "{role} result {index} contains {} bytes; the per-result limit is {MAX_RESULT_BYTES}",
            result.len()
        ));
    }
    if !result.is_ascii()
        || result
            .bytes()
            .any(|byte| matches!(byte, b'\n' | b'\r' | b'\t'))
    {
        return Err(format!(
            "{role} result {index} is not one line of ASCII text"
        ));
    }
    Ok(())
}

fn format_mismatch(case: NumberRadixCase, expected: &str, actual: &CandidateObservation) -> String {
    let expected =
        serde_json::to_string(expected).unwrap_or_else(|_| "\"<unprintable>\"".to_owned());
    let actual = match actual {
        CandidateObservation::Value(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"<unprintable>\"".to_owned())
        }
        CandidateObservation::Error(error) => format!("<error: {}>", truncate(error)),
    };
    format!(
        "number radix mismatch: bits=0x{:016x} radix={}\n  expected={expected}\n  actual={actual}",
        case.bits, case.radix
    )
}

fn stream_preview(bytes: &[u8]) -> String {
    truncate(&String::from_utf8_lossy(bytes))
}

fn truncate(text: &str) -> String {
    if text.len() <= MAX_ERROR_PREVIEW_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_ERROR_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::{
        CandidateObservation, EXPECTED_MANIFEST_RELEASE, NumberRadixCase, NumberRadixCorpus,
        build_cases, build_oracle_source, format_mismatch, observe_candidate, parse_corpus,
        parse_oracle_stdout,
    };
    use serde_json::json;

    fn corpus_json(release: &str, seed: &str, sample_count: u64, boundaries: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": 1,
            "quickjs_release": release,
            "sample_seed": seed,
            "sample_count": sample_count,
            "boundary_bits": boundaries,
        }))
        .expect("serialize test corpus")
    }

    #[test]
    fn corpus_expands_all_radices_then_a_reproducible_unique_sample() {
        let corpus = parse_corpus(
            &corpus_json(
                EXPECTED_MANIFEST_RELEASE,
                "0x6a09e667f3bcc909",
                3,
                &["0x0000000000000000", "0x8000000000000000"],
            ),
            "test.json",
        )
        .expect("valid corpus");
        let cases = build_cases(&corpus).expect("cases");
        assert_eq!(cases.len(), 2 * 35 + 3);
        assert_eq!(cases[0], NumberRadixCase { bits: 0, radix: 2 });
        assert_eq!(cases[34], NumberRadixCase { bits: 0, radix: 36 });
        assert_eq!(
            cases[35],
            NumberRadixCase {
                bits: 0x8000_0000_0000_0000,
                radix: 2
            }
        );
        assert_eq!(cases, build_cases(&corpus).expect("reproducible cases"));
        assert_eq!(
            cases
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            cases.len()
        );
    }

    #[test]
    fn corpus_rejects_wrong_release_duplicate_bits_and_case_overflow() {
        let wrong_release = corpus_json(
            "2025-09-13",
            "0x0000000000000001",
            0,
            &["0x0000000000000000"],
        );
        assert!(
            parse_corpus(&wrong_release, "wrong.json")
                .expect_err("wrong release")
                .contains("expected 2026-06-04")
        );

        let duplicate = corpus_json(
            EXPECTED_MANIFEST_RELEASE,
            "0x0000000000000001",
            0,
            &["0x0000000000000000", "0x0000000000000000"],
        );
        assert!(
            parse_corpus(&duplicate, "duplicate.json")
                .expect_err("duplicate bits")
                .contains("repeats boundary bits")
        );

        let boundaries = (0_u64..23)
            .map(|bits| format!("0x{bits:016x}"))
            .collect::<Vec<_>>();
        let boundary_refs = boundaries.iter().map(String::as_str).collect::<Vec<_>>();
        let overflow = corpus_json(
            EXPECTED_MANIFEST_RELEASE,
            "0x0000000000000001",
            512,
            &boundary_refs,
        );
        assert!(
            parse_corpus(&overflow, "overflow.json")
                .expect_err("case overflow")
                .contains("the limit is 1024")
        );
    }

    #[test]
    fn oracle_source_reconstructs_binary64_from_two_exact_u32_words() {
        let source = build_oracle_source(&[NumberRadixCase {
            bits: 0x3fb9_9999_9999_999a,
            radix: 16,
        }])
        .expect("oracle source");
        assert!(source.contains("new DataView"));
        assert!(source.contains("setUint32(0,h)"));
        assert!(source.contains("setUint32(4,l)"));
        assert!(source.contains("getFloat64(0)"));
        assert!(source.contains("__emit(0,0x3fb99999,0x9999999a,16);"));
        assert!(!source.contains("0.1"));
    }

    #[test]
    fn oracle_protocol_requires_canonical_indices_exact_count_and_bounded_ascii() {
        assert_eq!(
            parse_oracle_stdout(b"0\t0\n1\tff\n", 2).expect("valid protocol"),
            ["0", "ff"]
        );
        assert!(
            parse_oracle_stdout(b"00\t0\n", 1)
                .expect_err("non-canonical index")
                .contains("non-canonical index")
        );
        assert!(
            parse_oracle_stdout(b"0\t0\n", 2)
                .expect_err("missing result")
                .contains("expected 2")
        );
        assert!(
            parse_oracle_stdout(b"0\t0", 1)
                .expect_err("incomplete result")
                .contains("must end")
        );
    }

    #[test]
    fn candidate_uses_the_facade_and_runtime_number_intrinsic() {
        let observations = observe_candidate(&[
            NumberRadixCase { bits: 0, radix: 2 },
            NumberRadixCase {
                bits: 0x8000_0000_0000_0000,
                radix: 36,
            },
            NumberRadixCase {
                bits: 255.0_f64.to_bits(),
                radix: 16,
            },
        ])
        .expect("candidate observations");
        let values = observations
            .into_iter()
            .map(|observation| match observation {
                CandidateObservation::Value(value) => value,
                CandidateObservation::Error(error) => panic!("candidate error: {error}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(values, ["0", "0", "ff"]);
    }

    #[test]
    fn mismatch_reports_exact_bits_radix_expected_and_actual() {
        let mismatch = format_mismatch(
            NumberRadixCase {
                bits: 0x3fb9_9999_9999_999a,
                radix: 2,
            },
            "expected",
            &CandidateObservation::Value("actual".to_owned()),
        );
        assert!(mismatch.contains("bits=0x3fb999999999999a radix=2"));
        assert!(mismatch.contains("expected=\"expected\""));
        assert!(mismatch.contains("actual=\"actual\""));
    }

    #[test]
    fn cases_reject_an_impossible_oversized_in_memory_corpus() {
        let corpus = NumberRadixCorpus {
            sample_seed: 1,
            sample_count: 512,
            boundary_bits: (0_u64..23).collect(),
        };
        assert!(
            build_cases(&corpus)
                .expect_err("case overflow")
                .contains("the limit is 1024")
        );
    }
}
