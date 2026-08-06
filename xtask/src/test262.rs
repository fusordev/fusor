//! Pinned Test262 inventory and Global Script execution.

use quickjs::{ScriptEvaluationError, ScriptLimits, evaluate_script};
use quickjs_frontend::DiagnosticStage;
use quickjs_runtime::{
    Context, ExceptionKind, ExecutionError, ExecutionLimits, GlobalScriptError, JsException,
    Runtime, RuntimeLimits,
};
use serde_json::{Value as JsonValue, json};
use serde_yaml_ng::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub const PINNED_TEST262_REVISION: &str = "5c8206929d81b2d3d727ca6aac56c18358c8d790";
const PINNED_QUICKJS_RELEASE: &str = "2026-06-04";
const DEFAULT_BASELINE: &str = "tests/test262/upstream";
const DEFAULT_INSTRUCTION_FUEL: u64 = 10_000_000;
const STRICT_PREFIX: &str = "\"use strict\";\n";
const BASELINE_CONFIG_FNV1A64: u64 = 0xcd1b_b878_684d_5709;
const BASELINE_PATCH_FNV1A64: u64 = 0x69b9_930a_3213_9bb8;
const BASELINE_ERRORS_FNV1A64: u64 = 0xdcb6_b795_f816_719f;
const BASELINE_EXPECTED_ERROR_LINES: usize = 58;

#[derive(Debug, Eq, PartialEq)]
pub struct Test262Options {
    pub suite: PathBuf,
    pub baseline: PathBuf,
    pub filter: Option<String>,
    pub admit_feature: Option<String>,
    pub limit: Option<usize>,
    pub report: Option<PathBuf>,
    pub inventory_only: bool,
    pub instruction_fuel: u64,
}

pub fn parse_options(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<Test262Options, String> {
    let mut suite = None;
    let mut baseline = PathBuf::from(DEFAULT_BASELINE);
    let mut filter = None;
    let mut admit_feature = None;
    let mut limit = None;
    let mut report = None;
    let mut inventory_only = false;
    let mut instruction_fuel = DEFAULT_INSTRUCTION_FUEL;

    while let Some(option) = arguments.next() {
        match option.to_string_lossy().as_ref() {
            "--suite" => suite = Some(required_path(&mut arguments, "--suite")?),
            "--baseline" => baseline = required_path(&mut arguments, "--baseline")?,
            "--filter" => {
                let value = required_value(&mut arguments, "--filter")?;
                let value = value
                    .into_string()
                    .map_err(|_| "--filter must be valid UTF-8".to_owned())?;
                filter = Some(normalize_filter(&value)?);
            }
            "--admit-feature" => {
                let value = required_value(&mut arguments, "--admit-feature")?;
                let value = value
                    .into_string()
                    .map_err(|_| "--admit-feature must be valid UTF-8".to_owned())?;
                if value.is_empty() {
                    return Err("--admit-feature must not be empty".to_owned());
                }
                admit_feature = Some(value);
            }
            "--limit" => {
                limit = Some(required_positive_usize(&mut arguments, "--limit")?);
            }
            "--report" => report = Some(required_path(&mut arguments, "--report")?),
            "--inventory-only" => inventory_only = true,
            "--instruction-fuel" => {
                instruction_fuel = required_positive_u64(&mut arguments, "--instruction-fuel")?;
            }
            unknown => return Err(format!("unknown test262 option `{unknown}`")),
        }
    }

    Ok(Test262Options {
        suite: suite.ok_or("missing required --suite TEST262_PATH")?,
        baseline,
        filter,
        admit_feature,
        limit,
        report,
        inventory_only,
        instruction_fuel,
    })
}

fn required_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<OsString, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn required_path(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    required_value(arguments, option).map(PathBuf::from)
}

fn required_positive_usize(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<usize, String> {
    let value = required_value(arguments, option)?;
    let parsed = value
        .to_string_lossy()
        .parse::<usize>()
        .map_err(|_| format!("{option} must be a positive integer"))?;
    if parsed == 0 {
        return Err(format!("{option} must be a positive integer"));
    }
    Ok(parsed)
}

fn required_positive_u64(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<u64, String> {
    let value = required_value(arguments, option)?;
    let parsed = value
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|_| format!("{option} must be a positive integer"))?;
    if parsed == 0 {
        return Err(format!("{option} must be a positive integer"));
    }
    Ok(parsed)
}

#[derive(Debug)]
struct Baseline {
    root: PathBuf,
    policy: BaselinePolicy,
    expected_error_lines: usize,
    expected_errors: BTreeSet<(String, bool)>,
    config_fingerprint: u64,
    patch_fingerprint: u64,
    errors_fingerprint: u64,
}

impl Baseline {
    fn load(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("could not resolve baseline {}: {error}", root.display()))?;
        let config = read_required(&root.join("test262.conf"), "QuickJS Test262 configuration")?;
        let patch = read_required(&root.join("test262.patch"), "QuickJS Test262 patch")?;
        let errors = read_required(
            &root.join("test262_errors.txt"),
            "QuickJS Test262 expected errors",
        )?;
        let policy = BaselinePolicy::parse(&config)?;
        let expected_errors = errors
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(validate_expected_error_line)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let expected_error_lines = expected_errors.len();
        let config_fingerprint = fnv1a64(config.as_bytes());
        let patch_fingerprint = fnv1a64(patch.as_bytes());
        let errors_fingerprint = fnv1a64(errors.as_bytes());
        validate_baseline_fingerprint("test262.conf", config_fingerprint, BASELINE_CONFIG_FNV1A64)?;
        validate_baseline_fingerprint("test262.patch", patch_fingerprint, BASELINE_PATCH_FNV1A64)?;
        validate_baseline_fingerprint(
            "test262_errors.txt",
            errors_fingerprint,
            BASELINE_ERRORS_FNV1A64,
        )?;
        if expected_error_lines != BASELINE_EXPECTED_ERROR_LINES {
            return Err(format!(
                "test262_errors.txt has {expected_error_lines} entries, expected {BASELINE_EXPECTED_ERROR_LINES}"
            ));
        }
        Ok(Self {
            root,
            policy,
            expected_error_lines,
            expected_errors,
            config_fingerprint,
            patch_fingerprint,
            errors_fingerprint,
        })
    }

    fn patch_path(&self) -> PathBuf {
        self.root.join("test262.patch")
    }

    fn expects_failure(&self, path: &str, mode: &str) -> bool {
        self.expected_errors
            .contains(&(path.to_owned(), mode == "strict"))
    }
}

fn validate_baseline_fingerprint(label: &str, actual: u64, expected: u64) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{label} fingerprint {actual:016x} does not match pinned QuickJS {PINNED_QUICKJS_RELEASE} fingerprint {expected:016x}"
        ))
    }
}

fn read_required(path: &Path, label: &str) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("could not read {label} {}: {error}", path.display()))
}

fn validate_expected_error_line(line: &str) -> Result<(String, bool), String> {
    let line = line
        .strip_prefix("test262/test/")
        .ok_or_else(|| format!("invalid QuickJS expected-error entry `{line}`"))?;
    let (path, detail) = line
        .split_once(':')
        .ok_or_else(|| format!("invalid QuickJS expected-error entry `{line}`"))?;
    if !Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
        || !detail.contains(": ")
    {
        return Err(format!("invalid QuickJS expected-error entry `{line}`"));
    }
    Ok((path.to_owned(), detail.contains("strict mode:")))
}

#[derive(Debug, Default)]
struct BaselinePolicy {
    skipped_features: BTreeSet<String>,
    exclusions: Vec<String>,
}

impl BaselinePolicy {
    fn parse(source: &str) -> Result<Self, String> {
        let mut section = "";
        let mut policy = Self::default();
        for raw_line in source.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = &line[1..line.len() - 1];
                continue;
            }
            match section {
                "features" => {
                    if let Some((feature, value)) = line.split_once('=')
                        && value.trim() == "skip"
                    {
                        policy.skipped_features.insert(feature.trim().to_owned());
                    }
                }
                "exclude" => policy.exclusions.push(normalize_baseline_path(line)?),
                _ => {}
            }
        }
        policy.exclusions.sort();
        policy.exclusions.dedup();
        if policy.skipped_features.is_empty() || policy.exclusions.is_empty() {
            return Err("QuickJS test262.conf is missing feature or exclusion policy".to_owned());
        }
        Ok(policy)
    }

    fn excludes(&self, relative: &str) -> bool {
        let candidate = format!("test/{relative}");
        self.exclusions.iter().any(|excluded| {
            candidate == *excluded
                || excluded
                    .strip_suffix('/')
                    .is_some_and(|directory| candidate.starts_with(&format!("{directory}/")))
        })
    }
}

fn normalize_baseline_path(path: &str) -> Result<String, String> {
    let path = path.strip_prefix("test262/").unwrap_or(path);
    let directory = path.ends_with('/');
    let normalized = normalize_relative_path(path.trim_end_matches('/'))?;
    Ok(if directory {
        format!("{normalized}/")
    } else {
        normalized
    })
}

fn verify_checkout(suite: &Path, baseline: &Baseline) -> Result<PathBuf, String> {
    let suite = suite.canonicalize().map_err(|error| {
        format!(
            "could not resolve Test262 checkout {}: {error}",
            suite.display()
        )
    })?;
    for required in ["test", "harness", ".git"] {
        if !suite.join(required).exists() {
            return Err(format!(
                "{} is not a Test262 Git checkout: missing {required}",
                suite.display()
            ));
        }
    }
    let revision = git_output(&suite, &["rev-parse", "HEAD"])?;
    if revision.trim() != PINNED_TEST262_REVISION {
        return Err(format!(
            "Test262 checkout is at {}, expected {PINNED_TEST262_REVISION}",
            revision.trim()
        ));
    }
    let autocrlf = Command::new("git")
        .arg("-C")
        .arg(&suite)
        .args(["config", "--get", "core.autocrlf"])
        .output()
        .map_err(|error| format!("could not inspect Test262 line-ending policy: {error}"))?;
    if autocrlf.status.success() && String::from_utf8_lossy(&autocrlf.stdout).trim() == "true" {
        return Err(
            "Test262 checkout has core.autocrlf=true; recreate it with core.autocrlf=false"
                .to_owned(),
        );
    }
    let patch = baseline
        .patch_path()
        .canonicalize()
        .map_err(|error| format!("could not resolve Test262 patch: {error}"))?;
    let patched = Command::new("git")
        .arg("-C")
        .arg(&suite)
        .args(["apply", "--reverse", "--check", "--whitespace=nowarn"])
        .arg(&patch)
        .output()
        .map_err(|error| format!("could not validate the QuickJS Test262 patch: {error}"))?;
    if !patched.status.success() {
        return Err(format!(
            "Test262 checkout does not contain the pinned QuickJS {PINNED_QUICKJS_RELEASE} patch: {}",
            String::from_utf8_lossy(&patched.stderr).trim()
        ));
    }
    let harness_diff = Command::new("git")
        .arg("-C")
        .arg(&suite)
        .args(["diff", "--no-ext-diff", "--binary", "HEAD", "--", "harness"])
        .output()
        .map_err(|error| format!("could not compare the patched Test262 harness: {error}"))?;
    if !harness_diff.status.success() {
        return Err(format!(
            "could not compare the patched Test262 harness: {}",
            String::from_utf8_lossy(&harness_diff.stderr).trim()
        ));
    }
    let pinned_patch = fs::read(&patch)
        .map_err(|error| format!("could not read pinned Test262 patch: {error}"))?;
    if harness_diff.stdout != pinned_patch {
        return Err(
            "Test262 harness changes are not exactly the pinned QuickJS release patch".to_owned(),
        );
    }
    let test_diff = Command::new("git")
        .arg("-C")
        .arg(&suite)
        .args(["diff", "--quiet", "HEAD", "--", "test"])
        .status()
        .map_err(|error| format!("could not validate tracked Test262 sources: {error}"))?;
    if !test_diff.success() {
        return Err("Test262 test sources differ from the pinned revision".to_owned());
    }
    Ok(suite)
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not invoke git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| "git output was not UTF-8".to_owned())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Metadata {
    flags: BTreeSet<String>,
    features: BTreeSet<String>,
    includes: Vec<String>,
    negative: Option<NegativeExpectation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NegativeExpectation {
    phase: String,
    error_type: String,
}

fn parse_metadata(source: &str) -> Result<Metadata, String> {
    let start = source
        .find("/*---")
        .ok_or("source has no Test262 frontmatter opener")?;
    let body_start = start + "/*---".len();
    let relative_end = source[body_start..]
        .find("---*/")
        .ok_or("source has no Test262 frontmatter closer")?;
    let yaml = &source[body_start..body_start + relative_end];
    let value: YamlValue =
        serde_yaml_ng::from_str(yaml).map_err(|error| format!("invalid YAML: {error}"))?;
    let mapping = value
        .as_mapping()
        .ok_or("Test262 frontmatter is not a YAML mapping")?;
    let flags = string_set(mapping.get(YamlValue::from("flags")), "flags")?;
    let features = string_set(mapping.get(YamlValue::from("features")), "features")?;
    let includes = string_list(mapping.get(YamlValue::from("includes")), "includes")?;
    let negative = mapping
        .get(YamlValue::from("negative"))
        .map(parse_negative)
        .transpose()?;
    Ok(Metadata {
        flags,
        features,
        includes,
        negative,
    })
}

fn string_set(value: Option<&YamlValue>, label: &str) -> Result<BTreeSet<String>, String> {
    string_list(value, label)
        .map(IntoIterator::into_iter)
        .map(Iterator::collect)
}

fn string_list(value: Option<&YamlValue>, label: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_sequence()
        .ok_or_else(|| format!("{label} is not a YAML sequence"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{label} contains a non-string value"))
        })
        .collect()
}

fn parse_negative(value: &YamlValue) -> Result<NegativeExpectation, String> {
    let mapping = value.as_mapping().ok_or("negative is not a YAML mapping")?;
    let phase = mapping
        .get(YamlValue::from("phase"))
        .and_then(YamlValue::as_str)
        .ok_or("negative.phase is missing or not a string")?;
    let error_type = mapping
        .get(YamlValue::from("type"))
        .and_then(YamlValue::as_str)
        .ok_or("negative.type is missing or not a string")?;
    if !matches!(phase, "parse" | "resolution" | "runtime") {
        return Err(format!("unsupported negative phase `{phase}`"));
    }
    Ok(NegativeExpectation {
        phase: phase.to_owned(),
        error_type: error_type.to_owned(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestMode {
    NonStrict,
    Strict,
    Raw,
}

impl TestMode {
    const fn name(self) -> &'static str {
        match self {
            Self::NonStrict => "non-strict",
            Self::Strict => "strict",
            Self::Raw => "raw",
        }
    }
}

fn modes(metadata: &Metadata) -> Result<Vec<TestMode>, String> {
    let raw = metadata.flags.contains("raw");
    let strict = metadata.flags.contains("onlyStrict");
    let non_strict = metadata.flags.contains("noStrict");
    if usize::from(raw) + usize::from(strict) + usize::from(non_strict) > 1 {
        return Err("raw, onlyStrict, and noStrict flags are mutually exclusive".to_owned());
    }
    if raw {
        Ok(vec![TestMode::Raw])
    } else if metadata.flags.contains("module") || strict {
        Ok(vec![TestMode::Strict])
    } else if non_strict {
        Ok(vec![TestMode::NonStrict])
    } else {
        Ok(vec![TestMode::NonStrict, TestMode::Strict])
    }
}

#[derive(Debug)]
struct TestPlan {
    path: PathBuf,
    relative: String,
    metadata: Metadata,
    modes: Vec<TestMode>,
    skip_reason: Option<String>,
}

#[derive(Debug, Default)]
struct Inventory {
    plans: Vec<TestPlan>,
    fixtures: usize,
    skip_counts: BTreeMap<String, usize>,
}

impl Inventory {
    fn collect(
        suite: &Path,
        baseline: &Baseline,
        filter: Option<&str>,
        admitted_feature: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Self, String> {
        let test_root = suite.join("test");
        let harness_root = suite.join("harness");
        let paths = tracked_test_files(suite, filter)?;
        let mut inventory = Self::default();
        for path in paths {
            let relative = relative_slash_path(&test_root, &path)?;
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("_FIXTURE"))
            {
                inventory.fixtures += 1;
                continue;
            }
            if limit.is_some_and(|limit| inventory.plans.len() >= limit) {
                break;
            }
            let source = fs::read_to_string(&path)
                .map_err(|error| format!("could not read test/{relative}: {error}"))?;
            let metadata = parse_metadata(&source)
                .map_err(|error| format!("invalid metadata in test/{relative}: {error}"))?;
            let test_modes = modes(&metadata)
                .map_err(|error| format!("invalid flags in test/{relative}: {error}"))?;
            let skip_reason = classify_skip(
                &relative,
                &source,
                &metadata,
                &baseline.policy,
                admitted_feature,
                &harness_root,
            )?;
            if let Some(reason) = &skip_reason {
                *inventory.skip_counts.entry(reason.clone()).or_default() += test_modes.len();
            }
            inventory.plans.push(TestPlan {
                path,
                relative,
                metadata,
                modes: test_modes,
                skip_reason,
            });
        }
        Ok(inventory)
    }

    fn admitted_cases(&self) -> usize {
        self.plans
            .iter()
            .filter(|plan| plan.skip_reason.is_none())
            .map(|plan| plan.modes.len())
            .sum()
    }

    fn skipped_cases(&self) -> usize {
        self.skip_counts.values().sum()
    }
}

fn tracked_test_files(suite: &Path, filter: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let pathspec = filter.map_or_else(|| "test".to_owned(), |filter| format!("test/{filter}"));
    let output = Command::new("git")
        .arg("-C")
        .arg(suite)
        .args(["ls-tree", "-r", "--name-only", "HEAD", "--"])
        .arg(&pathspec)
        .output()
        .map_err(|error| format!("could not inventory tracked Test262 sources: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let output = String::from_utf8(output.stdout)
        .map_err(|_| "Test262 tracked path inventory was not UTF-8".to_owned())?;
    let mut files = Vec::new();
    for relative in output.lines().filter(|path| {
        Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
    }) {
        let path = suite.join(relative);
        if !path.is_file() {
            return Err(format!(
                "Test262 checkout is sparse or incomplete: missing tracked source {relative}"
            ));
        }
        files.push(path);
    }
    Ok(files)
}

fn classify_skip(
    relative: &str,
    source: &str,
    metadata: &Metadata,
    policy: &BaselinePolicy,
    admitted_feature: Option<&str>,
    harness_root: &Path,
) -> Result<Option<String>, String> {
    if relative.starts_with("intl402/") || relative.starts_with("staging/intl402/") {
        return Ok(Some("low-priority-intl402".to_owned()));
    }
    if policy.excludes(relative) {
        return Ok(Some("quickjs-baseline-exclude".to_owned()));
    }
    if let Some(feature) = metadata.features.iter().find(|feature| {
        policy.skipped_features.contains(*feature)
            && admitted_feature.is_none_or(|admitted| admitted != feature.as_str())
    }) {
        return Ok(Some(format!("quickjs-skipped-feature:{feature}")));
    }
    if metadata.flags.contains("module") {
        return Ok(Some("unsupported-module-goal".to_owned()));
    }
    let parse_negative = metadata
        .negative
        .as_ref()
        .is_some_and(|negative| negative.phase == "parse");
    if parse_negative {
        return Ok(None);
    }
    if metadata.flags.contains("async") {
        return Ok(Some("unsupported-async-host-print".to_owned()));
    }
    if metadata.flags.contains("CanBlockIsFalse") {
        return Ok(Some("unsupported-can-block-false".to_owned()));
    }
    if metadata
        .negative
        .as_ref()
        .is_some_and(|negative| negative.phase == "resolution")
    {
        return Ok(Some("unsupported-module-resolution".to_owned()));
    }
    if requires_host_api(source) {
        return Ok(Some("unsupported-test262-host-api".to_owned()));
    }
    for include in &metadata.includes {
        let include_path = safe_harness_include(harness_root, include)?;
        let include_source = fs::read_to_string(&include_path).map_err(|error| {
            format!(
                "could not read Test262 harness include {}: {error}",
                include_path.display()
            )
        })?;
        if requires_host_api(&include_source) {
            return Ok(Some("unsupported-test262-host-api".to_owned()));
        }
    }
    Ok(None)
}

fn requires_host_api(source: &str) -> bool {
    source.contains("$262") || source.contains("print(")
}

fn safe_harness_include(root: &Path, include: &str) -> Result<PathBuf, String> {
    let relative = Path::new(include);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("invalid Test262 harness include `{include}`"));
    }
    Ok(root.join(relative))
}

fn normalize_filter(filter: &str) -> Result<String, String> {
    let filter = filter.replace('\\', "/");
    let filter = filter
        .trim_start_matches("./")
        .strip_prefix("test/")
        .unwrap_or(filter.trim_start_matches("./"))
        .trim_end_matches('/');
    if filter.is_empty() {
        return Err("--filter must not be empty".to_owned());
    }
    normalize_relative_path(filter)
}

fn normalize_relative_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(format!("path is not a normalized relative path: `{path}`"));
    }
    Ok(normalized)
}

fn relative_slash_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "discovered path {} escaped Test262 test root {}",
            path.display(),
            root.display()
        )
    })?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Test262 path is not UTF-8: {}", path.display())),
            _ => Err(format!(
                "Test262 path is not normalized: {}",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(components.join("/"))
}

#[derive(Debug, Default)]
struct ExecutionSummary {
    passed: usize,
    failed: usize,
    failures: Vec<FailureRecord>,
}

#[derive(Debug)]
struct FailureRecord {
    path: String,
    mode: &'static str,
    expected: String,
    actual: String,
    detail: String,
}

impl FailureRecord {
    fn json(&self, baseline: &Baseline) -> JsonValue {
        json!({
            "path": self.path,
            "mode": self.mode,
            "expected": self.expected,
            "actual": self.actual,
            "detail": self.detail,
            "quickjs_baseline_known_failure": baseline.expects_failure(&self.path, self.mode),
        })
    }
}

struct HarnessSources {
    assert: String,
    sta: String,
    root: PathBuf,
}

impl HarnessSources {
    fn load(suite: &Path) -> Result<Self, String> {
        let root = suite.join("harness");
        Ok(Self {
            assert: read_required(&root.join("assert.js"), "Test262 assert.js")?,
            sta: read_required(&root.join("sta.js"), "Test262 sta.js")?,
            root,
        })
    }
}

pub fn run_test262(options: &Test262Options) -> Result<bool, String> {
    let baseline = Baseline::load(&options.baseline)?;
    if let Some(feature) = options.admit_feature.as_deref() {
        if options.filter.is_none() {
            return Err("--admit-feature requires an explicit --filter subtree".to_owned());
        }
        if !baseline.policy.skipped_features.contains(feature) {
            return Err(format!(
                "--admit-feature `{feature}` is not skipped by the pinned QuickJS baseline"
            ));
        }
    }
    let suite = verify_checkout(&options.suite, &baseline)?;
    let inventory = Inventory::collect(
        &suite,
        &baseline,
        options.filter.as_deref(),
        options.admit_feature.as_deref(),
        options.limit,
    )?;
    if inventory.plans.is_empty() {
        return Err("Test262 selection contains no runnable test source files".to_owned());
    }
    let execution = if options.inventory_only {
        ExecutionSummary::default()
    } else {
        execute_inventory(&suite, &inventory, options.instruction_fuel)?
    };
    let report = build_report(options, &baseline, &inventory, &execution);
    if let Some(path) = &options.report {
        write_report(path, &report)?;
    }
    println!(
        "test262: revision={} files={} admitted-cases={} skipped-cases={} passed={} failed={}{}",
        PINNED_TEST262_REVISION,
        inventory.plans.len(),
        inventory.admitted_cases(),
        inventory.skipped_cases(),
        execution.passed,
        execution.failed,
        if options.inventory_only {
            " inventory-only"
        } else {
            ""
        }
    );
    for failure in execution.failures.iter().take(20) {
        eprintln!(
            "test262 failure: test/{} [{}]: expected {}; got {}: {}",
            failure.path, failure.mode, failure.expected, failure.actual, failure.detail
        );
    }
    if execution.failures.len() > 20 {
        eprintln!(
            "test262: {} additional failures omitted from stderr; use --report for the full list",
            execution.failures.len() - 20
        );
    }
    Ok(options.inventory_only || execution.failed == 0)
}

fn execute_inventory(
    suite: &Path,
    inventory: &Inventory,
    instruction_fuel: u64,
) -> Result<ExecutionSummary, String> {
    let harness = HarnessSources::load(suite)?;
    let mut summary = ExecutionSummary::default();
    for plan in &inventory.plans {
        if plan.skip_reason.is_some() {
            continue;
        }
        let source = fs::read_to_string(&plan.path)
            .map_err(|error| format!("could not read test/{}: {error}", plan.relative))?;
        for &mode in &plan.modes {
            match execute_case(plan, mode, &source, &harness, instruction_fuel)? {
                None => summary.passed += 1,
                Some(failure) => {
                    summary.failed += 1;
                    summary.failures.push(failure);
                }
            }
        }
    }
    Ok(summary)
}

fn execute_case(
    plan: &TestPlan,
    mode: TestMode,
    source: &str,
    harness: &HarnessSources,
    instruction_fuel: u64,
) -> Result<Option<FailureRecord>, String> {
    let mut runtime = Runtime::try_new(RuntimeLimits::default())
        .map_err(|error| format!("could not create runtime: {error}"))?;
    let realm = runtime
        .create_realm()
        .map_err(|error| format!("could not create realm: {error}"))?;
    let mut context = runtime
        .context(&realm)
        .map_err(|error| format!("could not enter realm: {error}"))?;
    let limits = ScriptLimits::default()
        .with_execution(ExecutionLimits::default().with_instruction_fuel(instruction_fuel));
    let parse_negative = plan
        .metadata
        .negative
        .as_ref()
        .is_some_and(|negative| negative.phase == "parse");
    if mode != TestMode::Raw && !parse_negative {
        for (name, harness_source) in [
            ("harness/assert.js", harness.assert.as_str()),
            ("harness/sta.js", harness.sta.as_str()),
        ] {
            if let Err(error) = evaluate_script(&mut context, harness_source, name, limits) {
                return Ok(Some(harness_failure(plan, mode, name, &error)));
            }
        }
        for include in &plan.metadata.includes {
            let path = safe_harness_include(&harness.root, include)?;
            let include_source = fs::read_to_string(&path).map_err(|error| {
                format!("could not read harness include {}: {error}", path.display())
            })?;
            let name = format!("harness/{include}");
            if let Err(error) = evaluate_script(&mut context, &include_source, &name, limits) {
                return Ok(Some(harness_failure(plan, mode, &name, &error)));
            }
        }
    }
    let strict_source;
    let source = if mode == TestMode::Strict {
        strict_source = format!("{STRICT_PREFIX}{source}");
        &strict_source
    } else {
        source
    };
    let source_name = format!("test/{}", plan.relative);
    let result = evaluate_script(&mut context, source, &source_name, limits);
    Ok(compare_result(
        plan,
        mode,
        &context,
        result.as_ref().map(|_| ()),
    ))
}

fn harness_failure(
    plan: &TestPlan,
    mode: TestMode,
    name: &str,
    error: &ScriptEvaluationError,
) -> FailureRecord {
    FailureRecord {
        path: plan.relative.clone(),
        mode: mode.name(),
        expected: expectation_name(plan.metadata.negative.as_ref()),
        actual: "harness failure".to_owned(),
        detail: format!("{name}: {error}"),
    }
}

fn compare_result(
    plan: &TestPlan,
    mode: TestMode,
    context: &Context<'_>,
    result: Result<(), &ScriptEvaluationError>,
) -> Option<FailureRecord> {
    let expected = plan.metadata.negative.as_ref();
    match (expected, result) {
        (None, Ok(())) => None,
        (None, Err(error)) => {
            let actual = classify_error(context, error);
            Some(FailureRecord {
                path: plan.relative.clone(),
                mode: mode.name(),
                expected: "normal completion".to_owned(),
                actual: actual.summary(),
                detail: error.to_string(),
            })
        }
        (Some(expected), Ok(())) => Some(FailureRecord {
            path: plan.relative.clone(),
            mode: mode.name(),
            expected: expectation_name(Some(expected)),
            actual: "normal completion".to_owned(),
            detail: "negative test completed normally".to_owned(),
        }),
        (Some(expected), Err(error)) => {
            let actual = classify_error(context, error);
            if actual.phase == expected.phase
                && actual.error_type.as_deref() == Some(&expected.error_type)
            {
                None
            } else {
                Some(FailureRecord {
                    path: plan.relative.clone(),
                    mode: mode.name(),
                    expected: expectation_name(Some(expected)),
                    actual: actual.summary(),
                    detail: error.to_string(),
                })
            }
        }
    }
}

fn expectation_name(expected: Option<&NegativeExpectation>) -> String {
    expected.map_or_else(
        || "normal completion".to_owned(),
        |expected| format!("{} {}", expected.phase, expected.error_type),
    )
}

struct ActualError {
    phase: String,
    error_type: Option<String>,
}

impl ActualError {
    fn summary(&self) -> String {
        self.error_type.as_ref().map_or_else(
            || self.phase.clone(),
            |kind| format!("{} {kind}", self.phase),
        )
    }
}

fn classify_error(context: &Context<'_>, error: &ScriptEvaluationError) -> ActualError {
    match error {
        ScriptEvaluationError::Frontend(frontend)
            if matches!(
                frontend.stage(),
                DiagnosticStage::Parser | DiagnosticStage::Semantic
            ) =>
        {
            ActualError {
                phase: "parse".to_owned(),
                error_type: Some("SyntaxError".to_owned()),
            }
        }
        ScriptEvaluationError::Frontend(frontend) => ActualError {
            phase: format!("frontend-{}", frontend.stage()),
            error_type: None,
        },
        ScriptEvaluationError::Compiler(_) => ActualError {
            phase: "compiler".to_owned(),
            error_type: None,
        },
        ScriptEvaluationError::Runtime(GlobalScriptError::Execution(
            ExecutionError::Exception(exception),
        )) => ActualError {
            phase: "runtime".to_owned(),
            error_type: exception_type(context, exception),
        },
        ScriptEvaluationError::Runtime(GlobalScriptError::Install(_)) => ActualError {
            phase: "installation".to_owned(),
            error_type: None,
        },
        ScriptEvaluationError::Runtime(GlobalScriptError::Execution(_)) => ActualError {
            phase: "host-execution".to_owned(),
            error_type: None,
        },
    }
}

fn exception_type(context: &Context<'_>, exception: &JsException) -> Option<String> {
    if let Some(kind) = exception.kind() {
        return Some(exception_kind_name(kind).to_owned());
    }
    let value = exception.thrown_value()?;
    context
        .error_object_kind(value)
        .ok()
        .flatten()
        .map(|kind| kind.constructor_name().to_owned())
}

const fn exception_kind_name(kind: ExceptionKind) -> &'static str {
    match kind {
        ExceptionKind::InternalError => "InternalError",
        ExceptionKind::RangeError => "RangeError",
        ExceptionKind::ReferenceError => "ReferenceError",
        ExceptionKind::SyntaxError => "SyntaxError",
        ExceptionKind::TypeError => "TypeError",
        ExceptionKind::UriError => "URIError",
    }
}

fn build_report(
    options: &Test262Options,
    baseline: &Baseline,
    inventory: &Inventory,
    execution: &ExecutionSummary,
) -> JsonValue {
    let skip_counts = inventory
        .skip_counts
        .iter()
        .map(|(reason, count)| (reason.clone(), json!(count)))
        .collect::<serde_json::Map<_, _>>();
    let observed_quickjs_baseline_failures = execution
        .failures
        .iter()
        .filter(|failure| baseline.expects_failure(&failure.path, failure.mode))
        .count();
    json!({
        "schema": 1,
        "test262": {
            "revision": PINNED_TEST262_REVISION,
            "package_version": "5.0.0",
        },
        "quickjs_baseline": {
            "release": PINNED_QUICKJS_RELEASE,
            "config_fnv1a64": format!("{:016x}", baseline.config_fingerprint),
            "patch_fnv1a64": format!("{:016x}", baseline.patch_fingerprint),
            "expected_errors_fnv1a64": format!("{:016x}", baseline.errors_fingerprint),
            "expected_error_lines": baseline.expected_error_lines,
            "observed_known_failures": observed_quickjs_baseline_failures,
        },
        "selection": {
            "filter": options.filter,
            "admitted_feature": options.admit_feature,
            "limit": options.limit,
            "instruction_fuel": options.instruction_fuel,
        },
        "inventory": {
            "test_files": inventory.plans.len(),
            "fixture_files": inventory.fixtures,
            "admitted_cases": inventory.admitted_cases(),
            "skipped_cases": inventory.skipped_cases(),
            "skip_counts": skip_counts,
        },
        "execution": {
            "enabled": !options.inventory_only,
            "passed": execution.passed,
            "failed": execution.failed,
            "failures": execution.failures.iter().map(|failure| failure.json(baseline)).collect::<Vec<_>>(),
        },
    })
}

fn write_report(path: &Path, report: &JsonValue) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create report directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("could not serialize Test262 report: {error}"))?;
    bytes.push(b'\n');
    fs::write(path, bytes)
        .map_err(|error| format!("could not write Test262 report {}: {error}", path.display()))
}

const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_test262_options_with_bounded_selection() {
        let arguments = [
            "--suite",
            "/tmp/test262",
            "--baseline",
            "/tmp/baseline",
            "--filter",
            "test/language/expressions",
            "--admit-feature",
            "Temporal",
            "--limit",
            "12",
            "--report",
            "/tmp/report.json",
            "--instruction-fuel",
            "5000",
            "--inventory-only",
        ]
        .into_iter()
        .map(OsString::from);
        assert_eq!(
            parse_options(arguments),
            Ok(Test262Options {
                suite: PathBuf::from("/tmp/test262"),
                baseline: PathBuf::from("/tmp/baseline"),
                filter: Some("language/expressions".to_owned()),
                admit_feature: Some("Temporal".to_owned()),
                limit: Some(12),
                report: Some(PathBuf::from("/tmp/report.json")),
                inventory_only: true,
                instruction_fuel: 5_000,
            })
        );
    }

    #[test]
    fn parses_multiline_metadata_and_expands_default_modes() {
        let metadata = parse_metadata(
            r"/*---
flags:
  - generated
features: [Proxy, Reflect]
includes:
  - propertyHelper.js
negative:
  phase: runtime
  type: TypeError
---*/
throw new TypeError();",
        )
        .expect("metadata");
        assert_eq!(metadata.flags, BTreeSet::from(["generated".to_owned()]));
        assert_eq!(
            metadata.features,
            BTreeSet::from(["Proxy".to_owned(), "Reflect".to_owned()])
        );
        assert_eq!(metadata.includes, ["propertyHelper.js"]);
        assert_eq!(
            metadata.negative,
            Some(NegativeExpectation {
                phase: "runtime".to_owned(),
                error_type: "TypeError".to_owned(),
            })
        );
        assert_eq!(
            modes(&metadata),
            Ok(vec![TestMode::NonStrict, TestMode::Strict])
        );
    }

    #[test]
    fn parses_quickjs_skip_and_exclusion_policy() {
        let policy = BaselinePolicy::parse(
            "[features]\nProxy\nTemporal=skip\n\n[exclude]\ntest262/test/intl402/\n",
        )
        .expect("policy");
        assert_eq!(
            policy.skipped_features,
            BTreeSet::from(["Temporal".to_owned()])
        );
        assert!(policy.excludes("intl402/DateTimeFormat/basic.js"));
        assert!(!policy.excludes("built-ins/Date/basic.js"));
    }

    #[test]
    fn focused_feature_admission_only_removes_the_named_skip() {
        let policy = BaselinePolicy {
            skipped_features: BTreeSet::from(["Temporal".to_owned(), "ShadowRealm".to_owned()]),
            exclusions: Vec::new(),
        };
        let temporal = Metadata {
            features: BTreeSet::from(["Temporal".to_owned()]),
            ..Metadata::default()
        };
        let shadow_realm = Metadata {
            features: BTreeSet::from(["ShadowRealm".to_owned()]),
            ..Metadata::default()
        };
        let harness = Path::new("harness");

        assert_eq!(
            classify_skip(
                "built-ins/Temporal/basic.js",
                "",
                &temporal,
                &policy,
                Some("Temporal"),
                harness,
            ),
            Ok(None)
        );
        assert_eq!(
            classify_skip(
                "built-ins/ShadowRealm/basic.js",
                "",
                &shadow_realm,
                &policy,
                Some("Temporal"),
                harness,
            ),
            Ok(Some("quickjs-skipped-feature:ShadowRealm".to_owned()))
        );
    }

    #[test]
    fn checked_in_quickjs_baseline_is_exact_and_complete() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/test262/upstream");
        let baseline = Baseline::load(&root).expect("pinned baseline");
        assert_eq!(baseline.expected_error_lines, BASELINE_EXPECTED_ERROR_LINES);
        assert!(baseline.expects_failure(
            "language/identifier-resolution/assign-to-global-undefined.js",
            "strict"
        ));
        assert!(!baseline.expects_failure(
            "language/identifier-resolution/assign-to-global-undefined.js",
            "non-strict"
        ));
    }

    #[test]
    fn typed_negative_comparison_accepts_explicit_error_objects() {
        let root = unique_temp_dir("typed-negative");
        let harness_root = root.join("harness");
        fs::create_dir_all(&harness_root).expect("harness directory");
        let source = "/*---\nflags: [raw]\nnegative:\n  phase: runtime\n  type: TypeError\n---*/\nthrow new TypeError('expected');";
        let path = root.join("typed-negative.js");
        fs::write(&path, source).expect("test source");
        let metadata = parse_metadata(source).expect("metadata");
        let plan = TestPlan {
            path,
            relative: "typed-negative.js".to_owned(),
            modes: modes(&metadata).expect("modes"),
            metadata,
            skip_reason: None,
        };
        let harness = HarnessSources {
            assert: String::new(),
            sta: String::new(),
            root: harness_root,
        };
        assert!(
            execute_case(
                &plan,
                TestMode::Raw,
                source,
                &harness,
                DEFAULT_INSTRUCTION_FUEL,
            )
            .expect("execute")
            .is_none()
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "quickjs-test262-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }
}
