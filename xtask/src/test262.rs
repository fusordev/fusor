//! Pinned Test262 inventory and Global Script execution.

use crate::DEFAULT_TIMEOUT_MS;
use quickjs::{ScriptEvaluationError, ScriptLimits, evaluate_script};
use quickjs_frontend::DiagnosticStage;
use quickjs_runtime::{
    Context, ExceptionKind, ExecutionError, ExecutionLimits, GlobalScriptError, JsException,
    Runtime, RuntimeLimits,
};
use rayon::ThreadPoolBuilder;
use serde_json::{Value as JsonValue, json};
use serde_yaml_ng::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::{runtime::Builder as TokioRuntimeBuilder, sync::mpsc};

const DEFAULT_BASELINE: &str = "tests/test262/upstream";
const DEFAULT_INSTRUCTION_FUEL: u64 = 10_000_000;
const STRICT_PREFIX: &str = "\"use strict\";\n";
const TEST262_WORKER_STACK_SIZE: usize = 64 * 1024 * 1024;
const TEST262_PROGRESS_CHANNEL_PER_WORKER: usize = 2;

#[derive(Debug, Eq, PartialEq)]
pub struct Test262Options {
    pub suite: PathBuf,
    pub baseline: PathBuf,
    pub filter: Option<String>,
    pub admit_feature: Option<String>,
    pub admit_intl402: bool,
    pub limit: Option<usize>,
    pub report: Option<PathBuf>,
    pub inventory_only: bool,
    pub instruction_fuel: u64,
    pub timeout_ms: u64,
    pub jobs: usize,
    pub progress_every: Option<usize>,
    pub verbose: bool,
}

pub fn parse_options(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<Test262Options, String> {
    let mut suite = None;
    let mut baseline = PathBuf::from(DEFAULT_BASELINE);
    let mut filter = None;
    let mut admit_feature = None;
    let mut admit_intl402 = false;
    let mut limit = None;
    let mut report = None;
    let mut inventory_only = false;
    let mut instruction_fuel = DEFAULT_INSTRUCTION_FUEL;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut jobs = default_jobs();
    let mut progress_every = None;
    let mut verbose = false;

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
            "--admit-intl402" => admit_intl402 = true,
            "--limit" => {
                limit = Some(required_positive_usize(&mut arguments, "--limit")?);
            }
            "--report" => report = Some(required_path(&mut arguments, "--report")?),
            "--inventory-only" => inventory_only = true,
            "--instruction-fuel" => {
                instruction_fuel = required_positive_u64(&mut arguments, "--instruction-fuel")?;
            }
            "--timeout-ms" => {
                timeout_ms = required_positive_u64(&mut arguments, "--timeout-ms")?;
            }
            "--jobs" => jobs = required_positive_usize(&mut arguments, "--jobs")?,
            "--progress-every" => {
                progress_every = Some(required_positive_usize(&mut arguments, "--progress-every")?);
            }
            "--verbose" | "-v" => verbose = true,
            unknown => return Err(format!("unknown test262 option `{unknown}`")),
        }
    }

    Ok(Test262Options {
        suite: suite.ok_or("missing required --suite TEST262_PATH")?,
        baseline,
        filter,
        admit_feature,
        admit_intl402,
        limit,
        report,
        inventory_only,
        instruction_fuel,
        timeout_ms,
        jobs,
        progress_every,
        verbose,
    })
}

fn default_jobs() -> usize {
    thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
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
    policy: BaselinePolicy,
    config_fingerprint: u64,
}

impl Baseline {
    fn load(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("could not resolve baseline {}: {error}", root.display()))?;
        let config = read_required(&root.join("test262.conf"), "Test262 filter policy")?;
        let policy = BaselinePolicy::parse(&config)?;
        let config_fingerprint = fnv1a64(config.as_bytes());
        Ok(Self {
            policy,
            config_fingerprint,
        })
    }
}

fn read_required(path: &Path, label: &str) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("could not read {label} {}: {error}", path.display()))
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
            return Err("Test262 filter policy is missing feature or exclusion rules".to_owned());
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

#[derive(Debug)]
struct VerifiedCheckout {
    root: PathBuf,
    revision: String,
}

fn verify_checkout(suite: &Path) -> Result<VerifiedCheckout, String> {
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
    let revision = revision.trim();
    if revision.is_empty() {
        return Err(format!(
            "could not determine the Test262 checkout revision at {}",
            suite.display()
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
    let test_and_harness_diff = Command::new("git")
        .arg("-C")
        .arg(&suite)
        .args(["diff", "--quiet", "HEAD", "--", "test", "harness"])
        .status()
        .map_err(|error| format!("could not validate Test262 sources: {error}"))?;
    if !test_and_harness_diff.success() {
        return Err(format!(
            "Test262 test or harness sources differ from upstream revision {revision}"
        ));
    }
    let changes = Command::new("git")
        .arg("-C")
        .arg(&suite)
        .args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            "test",
            "harness",
        ])
        .output()
        .map_err(|error| format!("could not inspect Test262 source status: {error}"))?;
    if !changes.status.success() {
        return Err(format!(
            "could not inspect Test262 source status: {}",
            String::from_utf8_lossy(&changes.stderr).trim()
        ));
    }
    if !changes.stdout.is_empty() {
        return Err("Test262 test or harness sources contain local changes".to_owned());
    }
    Ok(VerifiedCheckout {
        root: suite,
        revision: revision.to_owned(),
    })
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

#[derive(Clone, Debug)]
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
        admit_intl402: bool,
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
                admit_intl402,
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
    admit_intl402: bool,
    harness_root: &Path,
) -> Result<Option<String>, String> {
    let is_intl402 = is_intl402_path(relative);
    if is_intl402 && !admit_intl402 {
        return Ok(Some("low-priority-intl402".to_owned()));
    }
    if policy.excludes(relative) && !(is_intl402 && admit_intl402) {
        return Ok(Some("quickjs-baseline-exclude".to_owned()));
    }
    if let Some(feature) = metadata.features.iter().find(|feature| {
        policy.skipped_features.contains(*feature)
            && !(is_intl402 && admit_intl402)
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

fn is_intl402_path(relative: &str) -> bool {
    relative.starts_with("intl402/") || relative.starts_with("staging/intl402/")
}

fn validate_intl402_admission_scope(options: &Test262Options) -> Result<(), String> {
    if !options.admit_intl402 {
        return Ok(());
    }
    let Some(filter) = options.filter.as_deref() else {
        return Err("--admit-intl402 requires an explicit Intl subtree --filter".to_owned());
    };
    if filter == "intl402"
        || filter.starts_with("intl402/")
        || filter == "staging/intl402"
        || filter.starts_with("staging/intl402/")
    {
        Ok(())
    } else {
        Err("--admit-intl402 requires --filter intl402[/...] or staging/intl402[/...]".to_owned())
    }
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
    fn json(&self) -> JsonValue {
        json!({
            "path": self.path,
            "mode": self.mode,
            "expected": self.expected,
            "actual": self.actual,
            "detail": self.detail,
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
    validate_execution_profile(options, cfg!(debug_assertions))?;
    validate_intl402_admission_scope(options)?;
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
    let suite = verify_checkout(&options.suite)?;
    let inventory = Inventory::collect(
        &suite.root,
        &baseline,
        options.filter.as_deref(),
        options.admit_feature.as_deref(),
        options.admit_intl402,
        options.limit,
    )?;
    if inventory.plans.is_empty() {
        return Err("Test262 selection contains no runnable test source files".to_owned());
    }
    if options.verbose || options.progress_every.is_some() {
        println!(
            "test262: selection filter={} files={} admitted-cases={} skipped-cases={} workers={} fuel={} timeout-ms={}",
            options.filter.as_deref().unwrap_or("test"),
            inventory.plans.len(),
            inventory.admitted_cases(),
            inventory.skipped_cases(),
            if options.inventory_only {
                0
            } else {
                options.jobs.min(inventory.admitted_cases())
            },
            options.instruction_fuel,
            options.timeout_ms,
        );
        for (reason, count) in &inventory.skip_counts {
            println!("test262: skipped {count} cases ({reason})");
        }
        flush_progress_output();
    }
    let execution = if options.inventory_only {
        ExecutionSummary::default()
    } else {
        execute_inventory(
            &suite.root,
            &inventory,
            options.instruction_fuel,
            options.timeout_ms,
            options.jobs,
            options.progress_every,
            options.verbose,
        )?
    };
    let report = build_report(options, &suite, &baseline, &inventory, &execution);
    if let Some(path) = &options.report {
        write_report(path, &report)?;
    }
    println!(
        "test262: revision={} files={} admitted-cases={} skipped-cases={} passed={} failed={} pass-rate={}{} workers={}",
        suite.revision,
        inventory.plans.len(),
        inventory.admitted_cases(),
        inventory.skipped_cases(),
        execution.passed,
        execution.failed,
        test262_pass_rate(execution.passed, execution.failed),
        if options.inventory_only {
            " inventory-only"
        } else {
            ""
        },
        if options.inventory_only {
            0
        } else {
            options.jobs.min(inventory.admitted_cases())
        },
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

fn validate_execution_profile(
    options: &Test262Options,
    debug_assertions_enabled: bool,
) -> Result<(), String> {
    if debug_assertions_enabled
        && !options.inventory_only
        && options.filter.is_none()
        && options.limit.is_none()
    {
        return Err(
            "full Test262 execution requires an optimized xtask binary; run `cargo run --release --quiet -p xtask -- test262 ...`"
                .to_owned(),
        );
    }
    Ok(())
}

fn test262_pass_rate(passed: usize, failed: usize) -> String {
    let executed = passed.saturating_add(failed);
    if executed == 0 {
        return "n/a".to_owned();
    }
    let hundredths = ((passed as u128) * 10_000) / (executed as u128);
    format!(
        "{passed}/{executed} ({}.{:02}%)",
        hundredths / 100,
        hundredths % 100
    )
}

fn execute_inventory(
    suite: &Path,
    inventory: &Inventory,
    instruction_fuel: u64,
    timeout_ms: u64,
    jobs: usize,
    progress_every: Option<usize>,
    verbose: bool,
) -> Result<ExecutionSummary, String> {
    let harness = HarnessSources::load(suite)?;
    let mut cases = Vec::new();
    for plan in &inventory.plans {
        if plan.skip_reason.is_some() {
            continue;
        }
        let source = Arc::<str>::from(
            fs::read_to_string(&plan.path)
                .map_err(|error| format!("could not read test/{}: {error}", plan.relative))?,
        );
        for &mode in &plan.modes {
            cases.push(ExecutionCase {
                plan: plan.clone(),
                mode,
                source: Arc::clone(&source),
            });
        }
    }
    let mut completed = 0_usize;
    let mut passed = 0_usize;
    let mut failed = 0_usize;
    let mut runner_errors = 0_usize;
    let results = execute_cases_parallel(
        &cases,
        &harness,
        instruction_fuel,
        timeout_ms,
        jobs,
        |index, result| {
            completed += 1;
            match result {
                Ok(None) => passed += 1,
                Ok(Some(_)) => failed += 1,
                Err(_) => runner_errors += 1,
            }
            if verbose {
                print_verbose_completion(completed, cases.len(), &cases[index], result);
            } else if progress_every
                .is_some_and(|interval| should_print_progress(completed, cases.len(), interval))
            {
                print_progress_completion(completed, cases.len(), passed, failed, runner_errors);
            }
        },
    )?;
    let mut summary = ExecutionSummary::default();
    for result in results {
        match result? {
            None => summary.passed += 1,
            Some(failure) => {
                summary.failed += 1;
                summary.failures.push(failure);
            }
        }
    }
    Ok(summary)
}

#[derive(Clone)]
struct ExecutionCase {
    plan: TestPlan,
    mode: TestMode,
    source: Arc<str>,
}

fn print_verbose_completion(
    completed: usize,
    total: usize,
    case: &ExecutionCase,
    result: &Result<Option<FailureRecord>, String>,
) {
    match result {
        Ok(None) => println!(
            "test262: complete {completed}/{total} pass test/{} [{}]",
            case.plan.relative,
            case.mode.name(),
        ),
        Ok(Some(failure)) => println!(
            "test262: complete {completed}/{total} fail test/{} [{}]: expected {}; got {}: {}",
            failure.path, failure.mode, failure.expected, failure.actual, failure.detail,
        ),
        Err(error) => println!(
            "test262: complete {completed}/{total} runner-error test/{} [{}]: {error}",
            case.plan.relative,
            case.mode.name(),
        ),
    }
    flush_progress_output();
}

fn should_print_progress(completed: usize, total: usize, interval: usize) -> bool {
    completed == total || completed.is_multiple_of(interval)
}

fn print_progress_completion(
    completed: usize,
    total: usize,
    passed: usize,
    failed: usize,
    runner_errors: usize,
) {
    let line = format_progress_completion(completed, total, passed, failed, runner_errors);
    println!("{line}");
    flush_progress_output();
}

fn format_progress_completion(
    completed: usize,
    total: usize,
    passed: usize,
    failed: usize,
    runner_errors: usize,
) -> String {
    let pass_rate = test262_pass_rate(passed, failed);
    format!(
        "test262: progress completed={completed}/{total} passed={passed} failed={failed} pass-rate={pass_rate} runner-errors={runner_errors}"
    )
}

fn flush_progress_output() {
    let _ = std::io::stdout().flush();
}

fn execute_cases_parallel(
    cases: &[ExecutionCase],
    harness: &HarnessSources,
    instruction_fuel: u64,
    timeout_ms: u64,
    jobs: usize,
    mut on_completion: impl FnMut(usize, &Result<Option<FailureRecord>, String>) + Send,
) -> Result<Vec<Result<Option<FailureRecord>, String>>, String> {
    if cases.is_empty() {
        return Ok(Vec::new());
    }
    let worker_count = jobs.min(cases.len()).max(1);
    // `ThreadPool::scope` runs the progress receiver on a Rayon worker while
    // the submitted test workers block on Tokio's channel. Reserve one Rayon
    // thread for that coordinator so `--jobs 1` still has one executing test
    // worker rather than deadlocking before its first completion.
    let pool_thread_count = worker_count.saturating_add(1);
    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_thread_count)
        .stack_size(TEST262_WORKER_STACK_SIZE)
        .thread_name(|index| format!("test262-worker-{index}"))
        .build()
        .map_err(|error| format!("could not build Test262 Rayon worker pool: {error}"))?;
    let progress_runtime = TokioRuntimeBuilder::new_current_thread()
        .build()
        .map_err(|error| format!("could not build Test262 progress runtime: {error}"))?;
    let channel_capacity = worker_count.saturating_mul(TEST262_PROGRESS_CHANNEL_PER_WORKER);
    let (sender, mut receiver) = mpsc::channel(channel_capacity);
    let mut ordered = pool.scope(move |scope| {
        for worker in 0..worker_count {
            let sender = sender.clone();
            scope.spawn(move |_| {
                for index in (worker..cases.len()).step_by(worker_count) {
                    let case = &cases[index];
                    let result = execute_case(
                        &case.plan,
                        case.mode,
                        &case.source,
                        harness,
                        instruction_fuel,
                        timeout_ms,
                    );
                    if sender.blocking_send((index, result)).is_err() {
                        return;
                    }
                }
            });
        }
        let mut results = Vec::with_capacity(cases.len());
        drop(sender);
        for _ in 0..cases.len() {
            let (index, result) = progress_runtime
                .block_on(receiver.recv())
                .ok_or_else(|| "a Test262 worker stopped before reporting every case".to_owned())?;
            on_completion(index, &result);
            results.push((index, result));
        }
        Ok::<_, String>(results)
    })?;
    ordered.sort_unstable_by_key(|(index, _)| *index);
    Ok(ordered.into_iter().map(|(_, result)| result).collect())
}

fn execute_case(
    plan: &TestPlan,
    mode: TestMode,
    source: &str,
    harness: &HarnessSources,
    instruction_fuel: u64,
    timeout_ms: u64,
) -> Result<Option<FailureRecord>, String> {
    let mut runtime = Runtime::try_new(RuntimeLimits::default())
        .map_err(|error| format!("could not create runtime: {error}"))?;
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    runtime.set_interrupt_handler(Arc::new(move || started.elapsed() >= timeout));
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
    suite: &VerifiedCheckout,
    baseline: &Baseline,
    inventory: &Inventory,
    execution: &ExecutionSummary,
) -> JsonValue {
    let skip_counts = inventory
        .skip_counts
        .iter()
        .map(|(reason, count)| (reason.clone(), json!(count)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "schema": 1,
        "test262": {
            "revision": suite.revision,
        },
        "filter_policy": {
            "path": "tests/test262/upstream/test262.conf",
            "config_fnv1a64": format!("{:016x}", baseline.config_fingerprint),
        },
        "selection": {
            "filter": options.filter,
            "admitted_feature": options.admit_feature,
            "admitted_intl402": options.admit_intl402,
            "limit": options.limit,
            "instruction_fuel": options.instruction_fuel,
            "timeout_ms": options.timeout_ms,
            "jobs": options.jobs,
            "progress_every": options.progress_every,
            "verbose": options.verbose,
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
            "failures": execution.failures.iter().map(FailureRecord::json).collect::<Vec<_>>(),
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
            "test/intl402/Locale",
            "--admit-feature",
            "Temporal",
            "--admit-intl402",
            "--limit",
            "12",
            "--report",
            "/tmp/report.json",
            "--instruction-fuel",
            "5000",
            "--timeout-ms",
            "2500",
            "--jobs",
            "3",
            "--progress-every",
            "1000",
            "--verbose",
            "--inventory-only",
        ]
        .into_iter()
        .map(OsString::from);
        assert_eq!(
            parse_options(arguments),
            Ok(Test262Options {
                suite: PathBuf::from("/tmp/test262"),
                baseline: PathBuf::from("/tmp/baseline"),
                filter: Some("intl402/Locale".to_owned()),
                admit_feature: Some("Temporal".to_owned()),
                admit_intl402: true,
                limit: Some(12),
                report: Some(PathBuf::from("/tmp/report.json")),
                inventory_only: true,
                instruction_fuel: 5_000,
                timeout_ms: 2_500,
                jobs: 3,
                progress_every: Some(1_000),
                verbose: true,
            })
        );
    }

    #[test]
    fn progress_interval_prints_milestones_and_final_completion() {
        assert!(!should_print_progress(999, 2_500, 1_000));
        assert!(should_print_progress(1_000, 2_500, 1_000));
        assert!(should_print_progress(2_000, 2_500, 1_000));
        assert!(should_print_progress(2_500, 2_500, 1_000));
        assert_eq!(
            format_progress_completion(1_000, 2_500, 750, 250, 0),
            "test262: progress completed=1000/2500 passed=750 failed=250 pass-rate=750/1000 (75.00%) runner-errors=0"
        );
    }

    #[test]
    fn test262_pass_rate_is_stable_for_empty_partial_and_complete_runs() {
        assert_eq!(test262_pass_rate(0, 0), "n/a");
        assert_eq!(test262_pass_rate(1, 2), "1/3 (33.33%)");
        assert_eq!(test262_pass_rate(100, 0), "100/100 (100.00%)");
    }

    #[test]
    fn full_test262_requires_an_optimized_runner() {
        let mut options = Test262Options {
            suite: PathBuf::from("/tmp/test262"),
            baseline: PathBuf::from("/tmp/baseline"),
            filter: None,
            admit_feature: None,
            admit_intl402: false,
            limit: None,
            report: None,
            inventory_only: false,
            instruction_fuel: DEFAULT_INSTRUCTION_FUEL,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            jobs: 1,
            progress_every: None,
            verbose: false,
        };

        let error = validate_execution_profile(&options, true)
            .expect_err("an unbounded debug-profile run must be rejected");
        assert!(error.contains("--release"));

        options.filter = Some("built-ins/Array".to_owned());
        assert_eq!(validate_execution_profile(&options, true), Ok(()));
        options.filter = None;
        options.limit = Some(10);
        assert_eq!(validate_execution_profile(&options, true), Ok(()));
        options.limit = None;
        options.inventory_only = true;
        assert_eq!(validate_execution_profile(&options, true), Ok(()));
        options.inventory_only = false;
        assert_eq!(validate_execution_profile(&options, false), Ok(()));
    }

    #[test]
    fn parallel_execution_preserves_case_order_and_isolates_runtimes() {
        let plan = TestPlan {
            path: PathBuf::from("parallel.js"),
            relative: "parallel.js".to_owned(),
            metadata: Metadata::default(),
            modes: vec![TestMode::Raw],
            skip_reason: None,
        };
        let cases = (0..4)
            .map(|_| ExecutionCase {
                plan: plan.clone(),
                mode: TestMode::Raw,
                source: Arc::<str>::from("var state = 1;"),
            })
            .collect::<Vec<_>>();
        let harness = HarnessSources {
            assert: String::new(),
            sta: String::new(),
            root: PathBuf::from("unused-harness"),
        };
        for jobs in [1, 2] {
            let mut completed = Vec::new();
            let outcomes = execute_cases_parallel(
                &cases,
                &harness,
                DEFAULT_INSTRUCTION_FUEL,
                DEFAULT_TIMEOUT_MS,
                jobs,
                |index, _| {
                    completed.push(index);
                },
            )
            .expect("parallel execution");
            assert_eq!(outcomes.len(), cases.len());
            completed.sort_unstable();
            assert_eq!(completed, vec![0, 1, 2, 3]);
            assert!(
                outcomes
                    .into_iter()
                    .all(|outcome| matches!(outcome, Ok(None)))
            );
        }
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
                false,
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
                false,
                harness,
            ),
            Ok(Some("quickjs-skipped-feature:ShadowRealm".to_owned()))
        );
    }

    #[test]
    fn focused_intl402_admission_bypasses_only_intl_policy_skips() {
        let policy = BaselinePolicy {
            skipped_features: BTreeSet::from(["Intl.Locale".to_owned(), "ShadowRealm".to_owned()]),
            exclusions: vec!["test/intl402/".to_owned()],
        };
        let intl = Metadata {
            features: BTreeSet::from(["Intl.Locale".to_owned()]),
            ..Metadata::default()
        };
        let shadow_realm = Metadata {
            features: BTreeSet::from(["ShadowRealm".to_owned()]),
            ..Metadata::default()
        };
        let harness = Path::new("harness");

        assert_eq!(
            classify_skip(
                "intl402/Locale/basic.js",
                "",
                &intl,
                &policy,
                None,
                false,
                harness,
            ),
            Ok(Some("low-priority-intl402".to_owned()))
        );
        assert_eq!(
            classify_skip(
                "intl402/Locale/basic.js",
                "",
                &intl,
                &policy,
                None,
                true,
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
                None,
                true,
                harness,
            ),
            Ok(Some("quickjs-skipped-feature:ShadowRealm".to_owned()))
        );
    }

    #[test]
    fn intl402_admission_requires_an_intl_filter() {
        let mut options = Test262Options {
            suite: PathBuf::from("/tmp/test262"),
            baseline: PathBuf::from("/tmp/baseline"),
            filter: None,
            admit_feature: None,
            admit_intl402: true,
            limit: None,
            report: None,
            inventory_only: true,
            instruction_fuel: DEFAULT_INSTRUCTION_FUEL,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            jobs: 1,
            progress_every: None,
            verbose: false,
        };

        assert!(validate_intl402_admission_scope(&options).is_err());
        options.filter = Some("built-ins/Intl".to_owned());
        assert!(validate_intl402_admission_scope(&options).is_err());
        options.filter = Some("intl402/Locale".to_owned());
        assert_eq!(validate_intl402_admission_scope(&options), Ok(()));
    }

    #[test]
    fn checked_in_test262_filter_policy_is_parseable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/test262/upstream");
        let baseline = Baseline::load(&root).expect("filter policy");
        assert!(baseline.config_fingerprint != 0);
        assert!(baseline.policy.skipped_features.contains("Intl.Locale"));
        assert!(baseline.policy.excludes("annexB/language/basic.js"));
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
                DEFAULT_TIMEOUT_MS,
            )
            .expect("execute")
            .is_none()
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn case_timeout_interrupts_an_infinite_script() {
        let plan = TestPlan {
            path: PathBuf::from("timeout.js"),
            relative: "timeout.js".to_owned(),
            metadata: Metadata::default(),
            modes: vec![TestMode::Raw],
            skip_reason: None,
        };
        let harness = HarnessSources {
            assert: String::new(),
            sta: String::new(),
            root: PathBuf::from("unused-harness"),
        };
        let failure = execute_case(
            &plan,
            TestMode::Raw,
            "while (true) {}",
            &harness,
            u64::MAX,
            1,
        )
        .expect("runner result")
        .expect("timeout failure");
        assert_eq!(failure.actual, "host-execution");
        assert!(failure.detail.contains("interrupted by the host"));
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "quickjs-test262-{label}-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ))
    }
}
