//! Differential checks for the Oxc/QuickJS syntax boundary.

use crate::{ProgramOutput, Status};
use crate::{collect_javascript_files, run_program_with_arguments_bounded, validate_executable};
use quickjs_frontend::{
    Allocator, CompilationGoal, FrontendOptions, GlobalScriptGoal, ParseMode, parse,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const EXPECTED_ORACLE_BANNER: &str = "QuickJS version 2026-06-04";
const EXPECTED_MANIFEST_RELEASE: &str = "2026-06-04";
const EXPECTED_EVAL_POLICY: &str = "excluded-user-deferred";
const MANIFEST_FILE_NAME: &str = "manifest.json";
const MANIFEST_SCHEMA_VERSION: u64 = 1;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FIXTURE_BYTES: usize = 64 * 1024;
const MAX_ORACLE_FIXTURE_STREAM_BYTES: usize = 16 * 1024;
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        match value {
            "accept" => Ok(Self::Accept),
            "reject" => Ok(Self::Reject),
            _ => Err(format!(
                "{location} must be `accept` or `reject`, found `{value}`"
            )),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ParserFixture {
    path: PathBuf,
    goal: ParserGoal,
    candidate_expectation: Expectation,
    oracle_expectation: Expectation,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

    const fn manifest_name(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Module => "module",
            Self::StrictScript => "strict-script",
            Self::AsyncScript => "async-script",
            Self::StrictAsyncScript => "strict-async-script",
        }
    }

    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        match value {
            "script" => Ok(Self::Script),
            "module" => Ok(Self::Module),
            "strict-script" => Ok(Self::StrictScript),
            "async-script" => Ok(Self::AsyncScript),
            "strict-async-script" => Ok(Self::StrictAsyncScript),
            _ => Err(format!(
                "{location} contains unknown non-eval parser goal `{value}`"
            )),
        }
    }
}

const REQUIRED_GOALS: [ParserGoal; 5] = [
    ParserGoal::Script,
    ParserGoal::Module,
    ParserGoal::StrictScript,
    ParserGoal::AsyncScript,
    ParserGoal::StrictAsyncScript,
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ParserFamily {
    SourceLexical,
    Bindings,
    Functions,
    Expressions,
    ClassesObjects,
    Statements,
    AnnexB,
    Modules,
    TargetProfile,
}

impl ParserFamily {
    const ALL: [Self; 9] = [
        Self::SourceLexical,
        Self::Bindings,
        Self::Functions,
        Self::Expressions,
        Self::ClassesObjects,
        Self::Statements,
        Self::AnnexB,
        Self::Modules,
        Self::TargetProfile,
    ];

    const fn manifest_name(self) -> &'static str {
        match self {
            Self::SourceLexical => "source-lexical",
            Self::Bindings => "bindings",
            Self::Functions => "functions",
            Self::Expressions => "expressions",
            Self::ClassesObjects => "classes-objects",
            Self::Statements => "statements",
            Self::AnnexB => "annex-b",
            Self::Modules => "modules",
            Self::TargetProfile => "target-profile",
        }
    }

    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|family| family.manifest_name() == value)
            .ok_or_else(|| format!("{location} contains unknown parser family `{value}`"))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ParserClaim {
    LexicalCommentsHashbangHtml,
    LexicalIdentifiersKeywordsUnicode,
    LexicalLiteralsTokenization,
    LexicalAsiAmbiguity,
    LexicalMalformedTokenRejections,
    BindingDeclarationsPatterns,
    BindingCollisionEarlyErrors,
    BindingStrictModeEarlyErrors,
    FunctionForms,
    FunctionParameters,
    FunctionContextualEarlyErrors,
    FunctionCoverGrammarEarlyErrors,
    ExpressionOperatorsAssignment,
    ExpressionMemberCallNewOptional,
    ExpressionContextualMetaSuperNewTarget,
    ClassObjectLiterals,
    ClassSyntaxPrivateSuper,
    ClassDuplicateProtoPrivateEarlyErrors,
    StatementBasicControl,
    StatementIterationLabels,
    StatementAbruptHandlers,
    StatementLexicalPlacementCollisions,
    AnnexBHtmlComments,
    AnnexBBlockFunctions,
    ModuleImportExport,
    ModuleAttributes,
    ModuleTopLevelAwaitContext,
    ModuleEarlyErrors,
    ProfileAcceptedEs2025,
    ProfileRejectedOutsideTarget,
    ProfileRegexpPatternDelegation,
}

impl ParserClaim {
    const ALL: [Self; 31] = [
        Self::LexicalCommentsHashbangHtml,
        Self::LexicalIdentifiersKeywordsUnicode,
        Self::LexicalLiteralsTokenization,
        Self::LexicalAsiAmbiguity,
        Self::LexicalMalformedTokenRejections,
        Self::BindingDeclarationsPatterns,
        Self::BindingCollisionEarlyErrors,
        Self::BindingStrictModeEarlyErrors,
        Self::FunctionForms,
        Self::FunctionParameters,
        Self::FunctionContextualEarlyErrors,
        Self::FunctionCoverGrammarEarlyErrors,
        Self::ExpressionOperatorsAssignment,
        Self::ExpressionMemberCallNewOptional,
        Self::ExpressionContextualMetaSuperNewTarget,
        Self::ClassObjectLiterals,
        Self::ClassSyntaxPrivateSuper,
        Self::ClassDuplicateProtoPrivateEarlyErrors,
        Self::StatementBasicControl,
        Self::StatementIterationLabels,
        Self::StatementAbruptHandlers,
        Self::StatementLexicalPlacementCollisions,
        Self::AnnexBHtmlComments,
        Self::AnnexBBlockFunctions,
        Self::ModuleImportExport,
        Self::ModuleAttributes,
        Self::ModuleTopLevelAwaitContext,
        Self::ModuleEarlyErrors,
        Self::ProfileAcceptedEs2025,
        Self::ProfileRejectedOutsideTarget,
        Self::ProfileRegexpPatternDelegation,
    ];

    const fn manifest_name(self) -> &'static str {
        match self {
            Self::LexicalCommentsHashbangHtml => "lexical.comments-hashbang-html",
            Self::LexicalIdentifiersKeywordsUnicode => "lexical.identifiers-keywords-unicode",
            Self::LexicalLiteralsTokenization => "lexical.literals-tokenization",
            Self::LexicalAsiAmbiguity => "lexical.asi-ambiguity",
            Self::LexicalMalformedTokenRejections => "lexical.malformed-token-rejections",
            Self::BindingDeclarationsPatterns => "binding.declarations-patterns",
            Self::BindingCollisionEarlyErrors => "binding.collision-early-errors",
            Self::BindingStrictModeEarlyErrors => "binding.strict-mode-early-errors",
            Self::FunctionForms => "function.forms",
            Self::FunctionParameters => "function.parameters",
            Self::FunctionContextualEarlyErrors => "function.contextual-early-errors",
            Self::FunctionCoverGrammarEarlyErrors => "function.cover-grammar-early-errors",
            Self::ExpressionOperatorsAssignment => "expression.operators-assignment",
            Self::ExpressionMemberCallNewOptional => "expression.member-call-new-optional",
            Self::ExpressionContextualMetaSuperNewTarget => {
                "expression.contextual-meta-super-new-target"
            }
            Self::ClassObjectLiterals => "class.object-literals",
            Self::ClassSyntaxPrivateSuper => "class.syntax-private-super",
            Self::ClassDuplicateProtoPrivateEarlyErrors => {
                "class.duplicate-proto-private-early-errors"
            }
            Self::StatementBasicControl => "statement.basic-control",
            Self::StatementIterationLabels => "statement.iteration-labels",
            Self::StatementAbruptHandlers => "statement.abrupt-handlers",
            Self::StatementLexicalPlacementCollisions => "statement.lexical-placement-collisions",
            Self::AnnexBHtmlComments => "annex-b.html-comments",
            Self::AnnexBBlockFunctions => "annex-b.block-functions",
            Self::ModuleImportExport => "module.import-export",
            Self::ModuleAttributes => "module.attributes",
            Self::ModuleTopLevelAwaitContext => "module.top-level-await-context",
            Self::ModuleEarlyErrors => "module.early-errors",
            Self::ProfileAcceptedEs2025 => "profile.accepted-es2025",
            Self::ProfileRejectedOutsideTarget => "profile.rejected-outside-target",
            Self::ProfileRegexpPatternDelegation => "profile.regexp-pattern-delegation",
        }
    }

    const fn family(self) -> ParserFamily {
        match self {
            Self::LexicalCommentsHashbangHtml
            | Self::LexicalIdentifiersKeywordsUnicode
            | Self::LexicalLiteralsTokenization
            | Self::LexicalAsiAmbiguity
            | Self::LexicalMalformedTokenRejections => ParserFamily::SourceLexical,
            Self::BindingDeclarationsPatterns
            | Self::BindingCollisionEarlyErrors
            | Self::BindingStrictModeEarlyErrors => ParserFamily::Bindings,
            Self::FunctionForms
            | Self::FunctionParameters
            | Self::FunctionContextualEarlyErrors
            | Self::FunctionCoverGrammarEarlyErrors => ParserFamily::Functions,
            Self::ExpressionOperatorsAssignment
            | Self::ExpressionMemberCallNewOptional
            | Self::ExpressionContextualMetaSuperNewTarget => ParserFamily::Expressions,
            Self::ClassObjectLiterals
            | Self::ClassSyntaxPrivateSuper
            | Self::ClassDuplicateProtoPrivateEarlyErrors => ParserFamily::ClassesObjects,
            Self::StatementBasicControl
            | Self::StatementIterationLabels
            | Self::StatementAbruptHandlers
            | Self::StatementLexicalPlacementCollisions => ParserFamily::Statements,
            Self::AnnexBHtmlComments | Self::AnnexBBlockFunctions => ParserFamily::AnnexB,
            Self::ModuleImportExport
            | Self::ModuleAttributes
            | Self::ModuleTopLevelAwaitContext
            | Self::ModuleEarlyErrors => ParserFamily::Modules,
            Self::ProfileAcceptedEs2025
            | Self::ProfileRejectedOutsideTarget
            | Self::ProfileRegexpPatternDelegation => ParserFamily::TargetProfile,
        }
    }

    const fn allows_quickjs_expectation(self, expectation: Expectation) -> bool {
        match self {
            Self::LexicalMalformedTokenRejections
            | Self::BindingCollisionEarlyErrors
            | Self::BindingStrictModeEarlyErrors
            | Self::ClassDuplicateProtoPrivateEarlyErrors
            | Self::StatementLexicalPlacementCollisions
            | Self::ModuleEarlyErrors
            | Self::ProfileRejectedOutsideTarget
            | Self::ProfileRegexpPatternDelegation => matches!(expectation, Expectation::Reject),
            Self::FunctionForms | Self::ClassObjectLiterals | Self::ProfileAcceptedEs2025 => {
                matches!(expectation, Expectation::Accept)
            }
            _ => true,
        }
    }

    const fn allows_goal(self, goal: ParserGoal) -> bool {
        match self {
            Self::BindingStrictModeEarlyErrors => matches!(
                goal,
                ParserGoal::Module | ParserGoal::StrictScript | ParserGoal::StrictAsyncScript
            ),
            Self::AnnexBBlockFunctions => !matches!(goal, ParserGoal::Module),
            Self::ModuleAttributes | Self::ModuleTopLevelAwaitContext | Self::ModuleEarlyErrors => {
                matches!(goal, ParserGoal::Module)
            }
            _ => true,
        }
    }

    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|claim| claim.manifest_name() == value)
            .ok_or_else(|| format!("{location} contains unknown parser claim `{value}`"))
    }
}

const REQUIRED_CLAIM_POLARITIES: usize = ParserClaim::ALL.len() * 2 - 11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DifferenceDirection {
    FrontendAccept,
    FrontendReject,
}

impl DifferenceDirection {
    const fn manifest_name(self) -> &'static str {
        match self {
            Self::FrontendAccept => "frontend-accept",
            Self::FrontendReject => "frontend-reject",
        }
    }

    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        match value {
            "frontend-accept" => Ok(Self::FrontendAccept),
            "frontend-reject" => Ok(Self::FrontendReject),
            _ => Err(format!(
                "{location} contains unknown difference direction `{value}`"
            )),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ParserCoverage {
    goals: usize,
    families: usize,
    claims: usize,
    claim_polarities: usize,
    differences: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct ParserCorpus {
    fixtures: Vec<ParserFixture>,
    coverage: ParserCoverage,
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
    let corpus = load_parser_corpus(&options.corpus)?;
    let fixtures = &corpus.fixtures;

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    for fixture in fixtures {
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
    println!(
        "parser coverage: {}/{} goals, {}/{} families, {}/{} claims, {}/{} required claim polarities, {} intentional difference(s)",
        corpus.coverage.goals,
        REQUIRED_GOALS.len(),
        corpus.coverage.families,
        ParserFamily::ALL.len(),
        corpus.coverage.claims,
        ParserClaim::ALL.len(),
        corpus.coverage.claim_polarities,
        REQUIRED_CLAIM_POLARITIES,
        corpus.coverage.differences,
    );

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

fn load_parser_corpus(corpus: &Path) -> Result<ParserCorpus, String> {
    let fixtures = collect_parser_fixtures(corpus)?;
    for fixture in &fixtures {
        validate_parser_fixture_source(fixture)?;
    }
    let manifest = read_parser_manifest(corpus)?;
    let cases = validate_manifest_header(&manifest)?;
    let mut validation = ManifestValidation::new(corpus, &fixtures)?;
    for (index, case) in cases.iter().enumerate() {
        validation.validate_case(index, case)?;
    }
    let coverage = validation.finish()?;
    Ok(ParserCorpus { fixtures, coverage })
}

fn validate_parser_fixture_source(fixture: &ParserFixture) -> Result<String, String> {
    let source = read_parser_fixture_source(&fixture.path)?;
    if contains_eval_identifier(&source, fixture.goal) {
        return Err(format!(
            "parser fixture {} contains excluded `eval` identifier",
            fixture.path.display()
        ));
    }
    Ok(source)
}

fn read_parser_fixture_source(path: &Path) -> Result<String, String> {
    let fixture_file = fs::File::open(path)
        .map_err(|error| format!("failed to open parser fixture {}: {error}", path.display()))?;
    let metadata = fixture_file.metadata().map_err(|error| {
        format!(
            "failed to inspect parser fixture {}: {error}",
            path.display()
        )
    })?;
    if metadata.len() > MAX_FIXTURE_BYTES as u64 {
        return Err(format!(
            "parser fixture {} contains {} bytes, exceeding the {MAX_FIXTURE_BYTES}-byte limit",
            path.display(),
            metadata.len()
        ));
    }

    let requested = MAX_FIXTURE_BYTES + 1;
    let mut bytes = Vec::new();
    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_FIXTURE_BYTES)
        .min(MAX_FIXTURE_BYTES);
    bytes.try_reserve_exact(initial_capacity).map_err(|_| {
        format!(
            "failed to reserve {initial_capacity} bytes for parser fixture {}",
            path.display()
        )
    })?;
    fixture_file
        .take(requested as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read parser fixture {}: {error}", path.display()))?;
    if bytes.len() > MAX_FIXTURE_BYTES {
        return Err(format!(
            "parser fixture {} grew beyond the {MAX_FIXTURE_BYTES}-byte limit while reading",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        format!(
            "parser fixture {} is not valid UTF-8: {error}",
            path.display()
        )
    })
}

fn contains_eval_identifier(source: &str, goal: ParserGoal) -> bool {
    let allocator = Allocator::new();
    match parse(&allocator, source, goal.candidate_options()) {
        Ok(parsed) => parsed.semantic().nodes().iter().any(|node| {
            node.kind()
                .identifier_name()
                .is_some_and(|name| name.as_str() == "eval")
        }),
        Err(_) => contains_raw_eval_identifier(source),
    }
}

fn contains_raw_eval_identifier(source: &str) -> bool {
    source.contains("eval") || contains_escaped_eval_spelling(source.as_bytes())
}

fn contains_escaped_eval_spelling(source: &[u8]) -> bool {
    (0..source.len()).any(|start| {
        b"eval"
            .iter()
            .try_fold(start, |offset, expected| {
                match_raw_identifier_character(source, offset, *expected)
            })
            .is_some()
    })
}

fn match_raw_identifier_character(source: &[u8], offset: usize, expected: u8) -> Option<usize> {
    if source.get(offset) == Some(&expected) {
        return Some(offset + 1);
    }
    if source.get(offset..offset + 2)? != b"\\u" {
        return None;
    }
    if source.get(offset + 2) == Some(&b'{') {
        let digits_start = offset + 3;
        let close = source
            .get(digits_start..)?
            .iter()
            .position(|byte| *byte == b'}')?
            + digits_start;
        let digits = source.get(digits_start..close)?;
        return decoded_ascii_escape(digits, expected).then_some(close + 1);
    }
    let digits = source.get(offset + 2..offset + 6)?;
    decoded_ascii_escape(digits, expected).then_some(offset + 6)
}

fn decoded_ascii_escape(digits: &[u8], expected: u8) -> bool {
    !digits.is_empty()
        && digits.len() <= 6
        && digits.iter().all(u8::is_ascii_hexdigit)
        && std::str::from_utf8(digits)
            .ok()
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            == Some(u32::from(expected))
}

fn read_parser_manifest(corpus: &Path) -> Result<Value, String> {
    let manifest_path = corpus.join(MANIFEST_FILE_NAME);
    let metadata = fs::metadata(&manifest_path).map_err(|error| {
        format!(
            "failed to inspect parser manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "parser manifest {} is {} bytes, exceeding the {MAX_MANIFEST_BYTES}-byte limit",
            manifest_path.display(),
            metadata.len()
        ));
    }
    let requested =
        usize::try_from(MAX_MANIFEST_BYTES + 1).expect("the parser manifest limit fits in usize");
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(requested).map_err(|_| {
        format!(
            "failed to reserve {requested} bytes for parser manifest {}",
            manifest_path.display()
        )
    })?;
    let manifest_file = fs::File::open(&manifest_path).map_err(|error| {
        format!(
            "failed to open parser manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    manifest_file
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "failed to read parser manifest {}: {error}",
                manifest_path.display()
            )
        })?;
    if bytes.len() >= requested {
        return Err(format!(
            "parser manifest {} exceeds the {MAX_MANIFEST_BYTES}-byte limit while being read",
            manifest_path.display()
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "failed to parse parser manifest {} as JSON: {error}",
            manifest_path.display()
        )
    })
}

fn validate_manifest_header(manifest: &Value) -> Result<&[Value], String> {
    let root = exact_object(
        manifest,
        "parser manifest",
        &["schema", "quickjs_release", "eval", "cases"],
    )?;
    let schema = root
        .get("schema")
        .and_then(Value::as_u64)
        .ok_or("parser manifest field `schema` must be an unsigned integer")?;
    if schema != MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "parser manifest schema must be {MANIFEST_SCHEMA_VERSION}, found {schema}"
        ));
    }
    let release = required_string(root, "quickjs_release", "parser manifest")?;
    if release != EXPECTED_MANIFEST_RELEASE {
        return Err(format!(
            "parser manifest release must match the pinned target; expected `{EXPECTED_MANIFEST_RELEASE}`, found `{release}`"
        ));
    }
    let eval_policy = required_string(root, "eval", "parser manifest")?;
    if eval_policy != EXPECTED_EVAL_POLICY {
        return Err(format!(
            "parser manifest eval policy must be `{EXPECTED_EVAL_POLICY}`, found `{eval_policy}`"
        ));
    }
    let cases = root
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("parser manifest field `cases` must be an array")?;
    if cases.is_empty() {
        return Err("parser manifest field `cases` must not be empty".to_owned());
    }
    Ok(cases)
}

struct ManifestValidation<'fixture> {
    fixtures: &'fixture [ParserFixture],
    actual: BTreeMap<PathBuf, usize>,
    declared_paths: BTreeSet<PathBuf>,
    difference_ids: BTreeSet<String>,
    covered_goals: BTreeSet<ParserGoal>,
    covered_families: BTreeSet<ParserFamily>,
    covered_claims: BTreeSet<ParserClaim>,
    covered_claim_polarities: BTreeSet<(ParserClaim, Expectation)>,
    differences: usize,
}

impl<'fixture> ManifestValidation<'fixture> {
    fn new(corpus: &Path, fixtures: &'fixture [ParserFixture]) -> Result<Self, String> {
        let mut actual = BTreeMap::new();
        for (index, fixture) in fixtures.iter().enumerate() {
            let relative = fixture.path.strip_prefix(corpus).map_err(|_| {
                format!(
                    "parser fixture {} is outside corpus {}",
                    fixture.path.display(),
                    corpus.display()
                )
            })?;
            if actual.insert(relative.to_path_buf(), index).is_some() {
                return Err(format!(
                    "parser corpus contains duplicate fixture path {}",
                    relative.display()
                ));
            }
        }
        Ok(Self {
            fixtures,
            actual,
            declared_paths: BTreeSet::new(),
            difference_ids: BTreeSet::new(),
            covered_goals: BTreeSet::new(),
            covered_families: BTreeSet::new(),
            covered_claims: BTreeSet::new(),
            covered_claim_polarities: BTreeSet::new(),
            differences: 0,
        })
    }

    fn validate_case(&mut self, index: usize, case: &Value) -> Result<(), String> {
        let location = format!("parser manifest case {index}");
        let case = exact_object(
            case,
            &location,
            &[
                "path",
                "goal",
                "quickjs",
                "frontend",
                "families",
                "claims",
                "evidence",
                "difference",
            ],
        )?;
        let path_text = required_string(case, "path", &location)?;
        let relative = validate_manifest_case_path(path_text, &location)?;
        if !self.declared_paths.insert(relative.clone()) {
            return Err(format!(
                "parser fixture {} is declared more than once in manifest.json",
                relative.display()
            ));
        }
        let Some(fixture_index) = self.actual.remove(&relative) else {
            return Err(format!(
                "parser fixture {} declared by manifest.json does not exist in the parser corpus",
                relative.display()
            ));
        };
        let fixture = &self.fixtures[fixture_index];
        let goal = ParserGoal::from_manifest(
            required_string(case, "goal", &location)?,
            &format!("{location} field `goal`"),
        )?;
        if goal != fixture.goal {
            return Err(format!(
                "parser manifest case {} declares goal `{}` but its directory selects `{}`",
                relative.display(),
                goal.manifest_name(),
                fixture.goal.manifest_name()
            ));
        }
        let quickjs = Expectation::from_manifest(
            required_string(case, "quickjs", &location)?,
            &format!("{location} field `quickjs`"),
        )?;
        let frontend = Expectation::from_manifest(
            required_string(case, "frontend", &location)?,
            &format!("{location} field `frontend`"),
        )?;
        if quickjs != fixture.oracle_expectation || frontend != fixture.candidate_expectation {
            return Err(format!(
                "parser manifest case {} expectations ({quickjs}/{frontend}) do not match its directory ({}/{})",
                relative.display(),
                fixture.oracle_expectation,
                fixture.candidate_expectation
            ));
        }

        let families = parse_families(case, &location)?;
        let claims = parse_claims(case, &location)?;
        validate_case_claims(&relative, &families, &claims, quickjs, goal)?;
        validate_evidence(case, &location)?;
        validate_difference(
            case.get("difference")
                .expect("exact_object checked the difference field"),
            quickjs,
            frontend,
            &relative,
            &location,
            &mut self.difference_ids,
        )?;
        if quickjs != frontend {
            self.differences += 1;
        }

        self.covered_goals.insert(goal);
        self.covered_families.extend(families);
        for claim in &claims {
            self.covered_claim_polarities.insert((*claim, quickjs));
        }
        self.covered_claims.extend(claims);
        Ok(())
    }

    fn finish(self) -> Result<ParserCoverage, String> {
        if let Some((relative, _)) = self.actual.first_key_value() {
            return Err(format!(
                "parser fixture {} is not declared in manifest.json",
                relative.display()
            ));
        }
        for goal in REQUIRED_GOALS {
            if !self.covered_goals.contains(&goal) {
                return Err(format!(
                    "parser manifest is missing required non-eval goal `{}`",
                    goal.manifest_name()
                ));
            }
        }
        for family in ParserFamily::ALL {
            if !self.covered_families.contains(&family) {
                return Err(format!(
                    "parser manifest is missing required family `{}`",
                    family.manifest_name()
                ));
            }
        }
        for claim in ParserClaim::ALL {
            if !self.covered_claims.contains(&claim) {
                return Err(format!(
                    "parser manifest is missing required claim `{}`",
                    claim.manifest_name()
                ));
            }
        }
        for claim in ParserClaim::ALL {
            for expectation in [Expectation::Accept, Expectation::Reject] {
                if claim.allows_quickjs_expectation(expectation)
                    && !self
                        .covered_claim_polarities
                        .contains(&(claim, expectation))
                {
                    return Err(format!(
                        "parser manifest claim `{}` is missing required QuickJS {expectation} coverage",
                        claim.manifest_name()
                    ));
                }
            }
        }
        let claim_polarities = self
            .covered_claim_polarities
            .iter()
            .filter(|(claim, expectation)| claim.allows_quickjs_expectation(*expectation))
            .count();
        Ok(ParserCoverage {
            goals: self.covered_goals.len(),
            families: self.covered_families.len(),
            claims: self.covered_claims.len(),
            claim_polarities,
            differences: self.differences,
        })
    }
}

fn validate_case_claims(
    relative: &Path,
    families: &BTreeSet<ParserFamily>,
    claims: &BTreeSet<ParserClaim>,
    quickjs: Expectation,
    goal: ParserGoal,
) -> Result<(), String> {
    let expected_families = claims
        .iter()
        .map(|claim| claim.family())
        .collect::<BTreeSet<_>>();
    if *families != expected_families {
        return Err(format!(
            "parser manifest case {} families must exactly match those derived from claims; declared [{}], derived [{}]",
            relative.display(),
            display_families(families),
            display_families(&expected_families)
        ));
    }
    for claim in claims {
        if !claim.allows_quickjs_expectation(quickjs) {
            return Err(format!(
                "parser manifest case {} claim `{}` does not allow QuickJS {quickjs} coverage",
                relative.display(),
                claim.manifest_name()
            ));
        }
        if !claim.allows_goal(goal) {
            return Err(format!(
                "parser manifest case {} claim `{}` does not allow parser goal `{}`",
                relative.display(),
                claim.manifest_name(),
                goal.manifest_name()
            ));
        }
    }
    Ok(())
}

fn display_families(families: &BTreeSet<ParserFamily>) -> String {
    families
        .iter()
        .map(|family| family.manifest_name())
        .collect::<Vec<_>>()
        .join(", ")
}

fn exact_object<'value>(
    value: &'value Value,
    location: &str,
    expected_fields: &[&str],
) -> Result<&'value Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{location} must be a JSON object"))?;
    for field in object.keys() {
        if !expected_fields.iter().any(|expected| *expected == field) {
            return Err(format!("{location} contains unknown field `{field}`"));
        }
    }
    for field in expected_fields {
        if !object.contains_key(*field) {
            return Err(format!("{location} is missing required field `{field}`"));
        }
    }
    Ok(object)
}

fn required_string<'value>(
    object: &'value Map<String, Value>,
    field: &str,
    location: &str,
) -> Result<&'value str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{location} field `{field}` must be a string"))
}

fn validate_manifest_case_path(path: &str, location: &str) -> Result<PathBuf, String> {
    let relative = PathBuf::from(path);
    if path.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "{location} field `path` must be a clean relative path, found `{path}`"
        ));
    }
    if !matches!(
        relative.extension().and_then(OsStr::to_str),
        Some("js" | "mjs")
    ) {
        return Err(format!(
            "{location} field `path` must name a .js or .mjs fixture, found `{path}`"
        ));
    }
    Ok(relative)
}

fn parse_families(
    object: &Map<String, Value>,
    location: &str,
) -> Result<BTreeSet<ParserFamily>, String> {
    let values = object
        .get("families")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{location} field `families` must be an array"))?;
    if values.is_empty() {
        return Err(format!("{location} field `families` must not be empty"));
    }
    let mut families = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let value = value
            .as_str()
            .ok_or_else(|| format!("{location} field `families` item {index} must be a string"))?;
        let family = ParserFamily::from_manifest(value, &format!("{location} field `families`"))?;
        if !families.insert(family) {
            return Err(format!(
                "{location} field `families` contains duplicate `{value}`"
            ));
        }
    }
    Ok(families)
}

fn parse_claims(
    object: &Map<String, Value>,
    location: &str,
) -> Result<BTreeSet<ParserClaim>, String> {
    let values = object
        .get("claims")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{location} field `claims` must be an array"))?;
    if values.is_empty() {
        return Err(format!("{location} field `claims` must not be empty"));
    }
    let mut claims = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let value = value
            .as_str()
            .ok_or_else(|| format!("{location} field `claims` item {index} must be a string"))?;
        let claim = ParserClaim::from_manifest(value, &format!("{location} field `claims`"))?;
        if !claims.insert(claim) {
            return Err(format!(
                "{location} field `claims` contains duplicate `{value}`"
            ));
        }
    }
    Ok(claims)
}

fn validate_evidence(object: &Map<String, Value>, location: &str) -> Result<(), String> {
    let evidence = object
        .get("evidence")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{location} field `evidence` must be an array"))?;
    if evidence.is_empty() {
        return Err(format!("{location} field `evidence` must not be empty"));
    }
    let mut unique = BTreeSet::new();
    for (index, value) in evidence.iter().enumerate() {
        let value = value
            .as_str()
            .ok_or_else(|| format!("{location} field `evidence` item {index} must be a string"))?;
        if value.trim().is_empty() {
            return Err(format!(
                "{location} field `evidence` item {index} must not be empty"
            ));
        }
        let Some(canonical) = parse_pinned_quickjs_evidence(value) else {
            return Err(format!(
                "{location} field `evidence` item {index} must identify pinned QuickJS source or tests, found `{value}`"
            ));
        };
        if !unique.insert(canonical) {
            return Err(format!(
                "{location} field `evidence` contains duplicate `{value}`"
            ));
        }
    }
    Ok(())
}

fn parse_pinned_quickjs_evidence(value: &str) -> Option<(&str, Option<(u64, u64)>)> {
    let value = value.strip_prefix("quickjs/").unwrap_or(value);
    let (path, anchor) = match value.split_once(':') {
        Some((path, anchor)) => (path, Some(anchor)),
        None => (value, None),
    };
    if !is_clean_quickjs_evidence_path(path)
        || !matches!(path, "quickjs.c" | "test262.conf" | "test262_errors.txt")
            && !path.starts_with("tests/")
    {
        return None;
    }
    let anchor = match anchor {
        Some(anchor) => Some(parse_positive_line_anchor(anchor)?),
        None => None,
    };
    Some((path, anchor))
}

fn is_clean_quickjs_evidence_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        && path
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn parse_positive_line_anchor(anchor: &str) -> Option<(u64, u64)> {
    let Some((start, end)) = anchor.split_once('-') else {
        let line = parse_positive_line(anchor)?;
        return Some((line, line));
    };
    if end.contains('-') {
        return None;
    }
    let start = parse_positive_line(start)?;
    let end = parse_positive_line(end)?;
    (start <= end).then_some((start, end))
}

fn parse_positive_line(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse::<u64>().ok().filter(|line| *line > 0)
}

fn validate_difference(
    value: &Value,
    quickjs: Expectation,
    frontend: Expectation,
    path: &Path,
    location: &str,
    ids: &mut BTreeSet<String>,
) -> Result<(), String> {
    let expected_direction = match (quickjs, frontend) {
        (Expectation::Reject, Expectation::Accept) => Some(DifferenceDirection::FrontendAccept),
        (Expectation::Accept, Expectation::Reject) => Some(DifferenceDirection::FrontendReject),
        _ => None,
    };
    if value.is_null() {
        return if expected_direction.is_some() {
            Err(format!(
                "parser manifest case {} requires a difference record because its expectations differ",
                path.display()
            ))
        } else {
            Ok(())
        };
    }
    let Some(expected_direction) = expected_direction else {
        return Err(format!(
            "parser manifest case {} must not retain a difference record when both expectations match",
            path.display()
        ));
    };
    let difference = exact_object(
        value,
        &format!("{location} field `difference`"),
        &["id", "direction", "rationale", "regression"],
    )?;
    let id = required_string(difference, "id", &format!("{location} difference"))?;
    if id.trim().is_empty() {
        return Err(format!("{location} difference ID must not be empty"));
    }
    if !ids.insert(id.to_owned()) {
        return Err(format!(
            "parser difference ID `{id}` is declared more than once"
        ));
    }
    let direction = DifferenceDirection::from_manifest(
        required_string(difference, "direction", &format!("{location} difference"))?,
        &format!("{location} difference"),
    )?;
    if direction != expected_direction {
        return Err(format!(
            "parser manifest case {} difference direction must be `{}`, found `{}`",
            path.display(),
            expected_direction.manifest_name(),
            direction.manifest_name()
        ));
    }
    let rationale = required_string(difference, "rationale", &format!("{location} difference"))?;
    if rationale.trim().is_empty() {
        return Err(format!(
            "{location} difference field `rationale` must not be empty"
        ));
    }
    let regression = required_string(difference, "regression", &format!("{location} difference"))?;
    if regression != path.to_string_lossy() {
        return Err(format!(
            "{location} difference field `regression` must name its fixture `{}`, found `{regression}`",
            path.display()
        ));
    }
    Ok(())
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
    let source = validate_parser_fixture_source(fixture)?;
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
    let source = validate_parser_fixture_source(fixture)?;
    let output = match fixture.goal {
        ParserGoal::Script => run_program_with_arguments_bounded(
            executable,
            &[OsStr::new("--script"), fixture.path.as_os_str()],
            timeout,
            MAX_ORACLE_FIXTURE_STREAM_BYTES,
        )?,
        ParserGoal::Module => run_program_with_arguments_bounded(
            executable,
            &[OsStr::new("--module"), fixture.path.as_os_str()],
            timeout,
            MAX_ORACLE_FIXTURE_STREAM_BYTES,
        )?,
        ParserGoal::StrictScript => run_program_with_arguments_bounded(
            executable,
            &[
                OsStr::new("--script"),
                OsStr::new("--strict"),
                fixture.path.as_os_str(),
            ],
            timeout,
            MAX_ORACLE_FIXTURE_STREAM_BYTES,
        )?,
        ParserGoal::AsyncScript => run_program_with_arguments_bounded(
            executable,
            &[
                OsStr::new("--std"),
                OsStr::new("--script"),
                OsStr::new("-e"),
                OsStr::new(ASYNC_SCRIPT_ORACLE),
                fixture.path.as_os_str(),
            ],
            timeout,
            MAX_ORACLE_FIXTURE_STREAM_BYTES,
        )?,
        ParserGoal::StrictAsyncScript => {
            let (insertion_index, needs_separator) = strict_async_oracle_insertion(&source);
            let insertion_index = insertion_index.to_string();
            run_program_with_arguments_bounded(
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
                MAX_ORACLE_FIXTURE_STREAM_BYTES,
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
        Expectation, MAX_FIXTURE_BYTES, REQUIRED_CLAIM_POLARITIES, classify_fixture,
        classify_oracle_output, load_parser_corpus, observe_candidate,
        strict_async_oracle_insertion, validate_difference,
    };
    use crate::{ProgramOutput, Status};
    use serde_json::{Value, json};
    use std::{
        collections::BTreeSet,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

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
        let fixtures = load_parser_corpus(&corpus)
            .expect("valid parser corpus and manifest")
            .fixtures;

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

    #[test]
    fn manifest_rejects_a_release_mismatch() {
        let corpus = TestCorpus::new(&valid_manifest());
        let mut manifest = corpus.manifest();
        manifest["quickjs_release"] = json!("2025-09-13");
        corpus.write_manifest(&manifest);

        let error = load_parser_corpus(corpus.path()).expect_err("release must be exact");
        assert!(error.contains("expected `2026-06-04`"), "{error}");
    }

    #[test]
    fn manifest_rejects_non_quickjs_evidence() {
        let corpus = TestCorpus::new(&valid_manifest());
        let mut manifest = corpus.manifest();
        manifest["cases"][0]["evidence"] = json!(["outside-reference/parser.rs:1"]);
        corpus.write_manifest(&manifest);

        let error = load_parser_corpus(corpus.path()).expect_err("unpinned evidence");
        assert!(error.contains("pinned QuickJS source or tests"), "{error}");
    }

    #[test]
    fn manifest_rejects_malformed_quickjs_evidence_anchors_and_paths() {
        for evidence in [
            "quickjs.c:0",
            "quickjs.c:01",
            "quickjs.c:not-a-line",
            "quickjs.c:9-2",
            "quickjs.c:1-2-3",
            "quickjs.c:1:2",
            "quickjs.c:18446744073709551616",
            "tests/../../outside.js:1",
            "tests//test_language.js:1",
            "tests\\test_language.js:1",
            "not-allowlisted.js:1",
            "quickjs/tests/../quickjs.c:1",
        ] {
            let corpus = TestCorpus::new(&valid_manifest());
            let mut manifest = corpus.manifest();
            manifest["cases"][0]["evidence"] = json!([evidence]);
            corpus.write_manifest(&manifest);

            let error =
                load_parser_corpus(corpus.path()).expect_err("malformed evidence must fail closed");
            assert!(
                error.contains("pinned QuickJS source or tests"),
                "{evidence}: {error}"
            );
        }

        let aliases = TestCorpus::new(&valid_manifest());
        let mut manifest = aliases.manifest();
        manifest["cases"][0]["evidence"] = json!(["quickjs.c:1", "quickjs/quickjs.c:1-1"]);
        aliases.write_manifest(&manifest);
        let error =
            load_parser_corpus(aliases.path()).expect_err("canonical evidence aliases duplicate");
        assert!(error.contains("contains duplicate"), "{error}");
    }

    #[test]
    fn manifest_rejects_missing_orphan_and_duplicate_cases() {
        let orphan = TestCorpus::new(&valid_manifest());
        orphan.write_fixture("accept/script/orphan.js");
        let error = load_parser_corpus(orphan.path()).expect_err("orphan fixture");
        assert!(
            error.contains("is not declared in manifest.json"),
            "{error}"
        );

        let missing = TestCorpus::new(&valid_manifest());
        fs::remove_file(missing.path().join("accept/script/source.js"))
            .expect("remove declared fixture");
        let error = load_parser_corpus(missing.path()).expect_err("missing fixture");
        assert!(
            error.contains("does not exist in the parser corpus"),
            "{error}"
        );

        let duplicate = TestCorpus::new(&valid_manifest());
        let mut manifest = duplicate.manifest();
        let repeated = manifest["cases"][0].clone();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .push(repeated);
        duplicate.write_manifest(&manifest);
        let error = load_parser_corpus(duplicate.path()).expect_err("duplicate case");
        assert!(error.contains("declared more than once"), "{error}");
    }

    #[test]
    fn manifest_requires_differences_exactly_for_mismatched_expectations() {
        let missing = TestCorpus::new(&valid_manifest());
        let mut manifest = missing.manifest();
        let case = manifest["cases"][0].as_object_mut().expect("case object");
        case.insert(
            "path".to_owned(),
            json!("candidate-accept/script/source.js"),
        );
        case.insert("quickjs".to_owned(), json!("reject"));
        case.get_mut("claims")
            .and_then(Value::as_array_mut)
            .expect("claims array")
            .retain(|claim| {
                !matches!(
                    claim.as_str(),
                    Some("function.forms" | "class.object-literals")
                )
            });
        fs::create_dir_all(missing.path().join("candidate-accept/script"))
            .expect("create difference fixture directory");
        fs::rename(
            missing.path().join("accept/script/source.js"),
            missing.path().join("candidate-accept/script/source.js"),
        )
        .expect("move fixture into difference directory");
        missing.write_manifest(&manifest);
        let error = load_parser_corpus(missing.path()).expect_err("missing difference");
        assert!(error.contains("requires a difference record"), "{error}");

        let stale = TestCorpus::new(&valid_manifest());
        let mut manifest = stale.manifest();
        manifest["cases"][0]["difference"] = difference("QJS-OXC-STALE", "frontend-accept");
        stale.write_manifest(&manifest);
        let error = load_parser_corpus(stale.path()).expect_err("stale difference");
        assert!(
            error.contains("must not retain a difference record"),
            "{error}"
        );
    }

    #[test]
    fn differences_require_unique_ids_exact_direction_and_fixture_regressions() {
        let path = Path::new("candidate-accept/script/source.js");
        let mut ids = BTreeSet::new();
        validate_difference(
            &difference("QJS-OXC-ONE", "frontend-accept"),
            Expectation::Reject,
            Expectation::Accept,
            path,
            "case",
            &mut ids,
        )
        .expect("valid difference");

        let duplicate = validate_difference(
            &difference("QJS-OXC-ONE", "frontend-accept"),
            Expectation::Reject,
            Expectation::Accept,
            path,
            "case",
            &mut ids,
        )
        .expect_err("difference IDs must be unique");
        assert!(duplicate.contains("declared more than once"), "{duplicate}");

        let wrong_direction = validate_difference(
            &difference("QJS-OXC-TWO", "frontend-reject"),
            Expectation::Reject,
            Expectation::Accept,
            path,
            "case",
            &mut ids,
        )
        .expect_err("direction must match expectations");
        assert!(
            wrong_direction.contains("must be `frontend-accept`"),
            "{wrong_direction}"
        );

        let wrong_regression = validate_difference(
            &json!({
                "id": "QJS-OXC-THREE",
                "direction": "frontend-accept",
                "rationale": "intentional",
                "regression": "candidate-accept/script/other.js"
            }),
            Expectation::Reject,
            Expectation::Accept,
            path,
            "case",
            &mut ids,
        )
        .expect_err("regression must name the fixture");
        assert!(
            wrong_regression.contains("must name its fixture"),
            "{wrong_regression}"
        );
    }

    #[test]
    fn manifest_rejects_goal_mismatch_and_missing_coverage() {
        let mismatch = TestCorpus::new(&valid_manifest());
        let mut manifest = mismatch.manifest();
        manifest["cases"][0]["goal"] = json!("module");
        mismatch.write_manifest(&manifest);
        let error = load_parser_corpus(mismatch.path()).expect_err("goal mismatch");
        assert!(error.contains("declares goal `module`"), "{error}");

        let missing_goal = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_goal.manifest();
        let cases = manifest["cases"].as_array_mut().expect("cases array");
        let strict_async = cases
            .iter()
            .position(|case| case["goal"] == "strict-async-script")
            .expect("strict async case");
        let removed = cases.remove(strict_async);
        fs::remove_file(
            missing_goal
                .path()
                .join(removed["path"].as_str().expect("case path")),
        )
        .expect("remove strict async fixture");
        missing_goal.write_manifest(&manifest);
        let error = load_parser_corpus(missing_goal.path()).expect_err("goal coverage");
        assert!(
            error.contains("missing required non-eval goal `strict-async-script`"),
            "{error}"
        );

        let missing_claim = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_claim.manifest();
        let claims = manifest["cases"][0]["claims"]
            .as_array_mut()
            .expect("claims array");
        claims.retain(|claim| claim != "lexical.asi-ambiguity");
        missing_claim.write_manifest(&manifest);
        let error = load_parser_corpus(missing_claim.path()).expect_err("claim coverage");
        assert!(
            error.contains(
                "claim `lexical.asi-ambiguity` is missing required QuickJS accept coverage"
            ),
            "{error}"
        );

        let missing_family = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_family.manifest();
        for case in manifest["cases"].as_array_mut().expect("cases array") {
            case["families"]
                .as_array_mut()
                .expect("families array")
                .retain(|family| family != "target-profile");
            case["claims"]
                .as_array_mut()
                .expect("claims array")
                .retain(|claim| {
                    !matches!(
                        claim.as_str(),
                        Some(
                            "profile.accepted-es2025"
                                | "profile.rejected-outside-target"
                                | "profile.regexp-pattern-delegation"
                        )
                    )
                });
        }
        missing_family.write_manifest(&manifest);
        let error = load_parser_corpus(missing_family.path()).expect_err("family coverage");
        assert!(
            error.contains("missing required family `target-profile`"),
            "{error}"
        );
    }

    #[test]
    fn manifest_requires_each_declared_quickjs_claim_polarity() {
        let complete = TestCorpus::new(&valid_manifest());
        let coverage = load_parser_corpus(complete.path())
            .expect("single-polarity exceptions and all required pairs are valid")
            .coverage;
        assert_eq!(coverage.claims, 31);
        assert_eq!(coverage.claim_polarities, REQUIRED_CLAIM_POLARITIES);

        let missing_accept = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_accept.manifest();
        manifest["cases"][0]["claims"]
            .as_array_mut()
            .expect("claims array")
            .retain(|claim| claim != "expression.operators-assignment");
        missing_accept.write_manifest(&manifest);
        let error =
            load_parser_corpus(missing_accept.path()).expect_err("accept polarity is required");
        assert!(
            error.contains(
                "claim `expression.operators-assignment` is missing required QuickJS accept coverage"
            ),
            "{error}"
        );

        let missing_reject = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_reject.manifest();
        let reject_case = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["quickjs"] == "reject")
            .expect("reject case");
        reject_case["claims"]
            .as_array_mut()
            .expect("claims array")
            .retain(|claim| claim != "expression.operators-assignment");
        missing_reject.write_manifest(&manifest);
        let error =
            load_parser_corpus(missing_reject.path()).expect_err("reject polarity is required");
        assert!(
            error.contains(
                "claim `expression.operators-assignment` is missing required QuickJS reject coverage"
            ),
            "{error}"
        );
    }

    #[test]
    fn manifest_rejects_extra_families_for_claims() {
        let corpus = TestCorpus::new(&valid_manifest());
        let mut manifest = corpus.manifest();
        let strict_script = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["goal"] == "strict-script")
            .expect("strict Script case");
        strict_script["families"]
            .as_array_mut()
            .expect("families array")
            .push(json!("target-profile"));
        corpus.write_manifest(&manifest);

        let error = load_parser_corpus(corpus.path()).expect_err("extra family must fail closed");
        assert!(error.contains("families must exactly match"), "{error}");

        let missing = TestCorpus::new(&valid_manifest());
        let mut manifest = missing.manifest();
        manifest["cases"][0]["families"]
            .as_array_mut()
            .expect("families array")
            .retain(|family| family != "expressions");
        missing.write_manifest(&manifest);
        let error = load_parser_corpus(missing.path())
            .expect_err("missing derived family must fail closed");
        assert!(error.contains("families must exactly match"), "{error}");
    }

    #[test]
    fn manifest_rejects_disallowed_claim_polarities_and_goals() {
        let disallowed_polarity = TestCorpus::new(&valid_manifest());
        let mut manifest = disallowed_polarity.manifest();
        let reject_case = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["quickjs"] == "reject")
            .expect("reject case");
        reject_case["claims"]
            .as_array_mut()
            .expect("claims array")
            .push(json!("function.forms"));
        disallowed_polarity.write_manifest(&manifest);
        let error = load_parser_corpus(disallowed_polarity.path())
            .expect_err("accept-only claim must reject a reject mapping");
        assert!(
            error.contains("does not allow QuickJS reject coverage"),
            "{error}"
        );

        let reject_only_in_accept = TestCorpus::new(&valid_manifest());
        let mut manifest = reject_only_in_accept.manifest();
        let module_case = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["goal"] == "module" && case["quickjs"] == "accept")
            .expect("accepted Module case");
        module_case["claims"]
            .as_array_mut()
            .expect("claims array")
            .push(json!("module.early-errors"));
        reject_only_in_accept.write_manifest(&manifest);
        let error = load_parser_corpus(reject_only_in_accept.path())
            .expect_err("reject-only claim must reject an accept mapping");
        assert!(
            error.contains("does not allow QuickJS accept coverage"),
            "{error}"
        );

        let disallowed_goal = TestCorpus::new(&valid_manifest());
        let mut manifest = disallowed_goal.manifest();
        let script_case = &mut manifest["cases"][0];
        script_case["claims"]
            .as_array_mut()
            .expect("claims array")
            .push(json!("module.attributes"));
        script_case["families"]
            .as_array_mut()
            .expect("families array")
            .push(json!("modules"));
        disallowed_goal.write_manifest(&manifest);
        let error = load_parser_corpus(disallowed_goal.path())
            .expect_err("Module-only claim must reject a Script mapping");
        assert!(
            error.contains("does not allow parser goal `script`"),
            "{error}"
        );

        let strict_claim_in_script = TestCorpus::new(&valid_manifest());
        let mut manifest = strict_claim_in_script.manifest();
        let reject_script = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["goal"] == "script" && case["quickjs"] == "reject")
            .expect("rejected Script case");
        reject_script["claims"]
            .as_array_mut()
            .expect("claims array")
            .push(json!("binding.strict-mode-early-errors"));
        strict_claim_in_script.write_manifest(&manifest);
        let error = load_parser_corpus(strict_claim_in_script.path())
            .expect_err("strict-only claim must reject a Script mapping");
        assert!(
            error.contains("does not allow parser goal `script`"),
            "{error}"
        );

        let annex_module = TestCorpus::new(&valid_manifest());
        let mut manifest = annex_module.manifest();
        let reject_module = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["goal"] == "module" && case["quickjs"] == "reject")
            .expect("rejected Module case");
        reject_module["families"]
            .as_array_mut()
            .expect("families array")
            .push(json!("annex-b"));
        reject_module["claims"]
            .as_array_mut()
            .expect("claims array")
            .push(json!("annex-b.block-functions"));
        annex_module.write_manifest(&manifest);
        let error = load_parser_corpus(annex_module.path())
            .expect_err("Annex B block functions must reject a Module mapping");
        assert!(
            error.contains("does not allow parser goal `module`"),
            "{error}"
        );
    }

    #[test]
    fn corpus_rejects_eval_identifiers_and_unbounded_or_non_utf8_sources() {
        for source in [
            "eval('source');\n",
            "const eval = 1;\n",
            "object.eval;\n",
            "e\\u0076al;\n",
            "eval +\n",
            "e\\u0076al +\n",
        ] {
            let corpus = TestCorpus::new(&valid_manifest());
            fs::write(corpus.path().join("accept/script/source.js"), source)
                .expect("write eval fixture");
            let error =
                load_parser_corpus(corpus.path()).expect_err("eval identifier must fail closed");
            assert!(
                error.contains("contains excluded `eval` identifier"),
                "{source:?}: {error}"
            );
        }

        let allowed = TestCorpus::new(&valid_manifest());
        fs::write(
            allowed.path().join("accept/script/source.js"),
            "const evaluation = 'eval'; // eval\n/eval/.test('eval'); class C { #eval; }\n",
        )
        .expect("write non-identifier eval spellings");
        load_parser_corpus(allowed.path()).expect("non-identifier eval spellings remain allowed");

        let exact_limit = TestCorpus::new(&valid_manifest());
        fs::write(
            exact_limit.path().join("accept/script/source.js"),
            vec![b' '; MAX_FIXTURE_BYTES],
        )
        .expect("write exact-limit fixture");
        load_parser_corpus(exact_limit.path()).expect("exact fixture limit remains accepted");

        let oversized = TestCorpus::new(&valid_manifest());
        fs::write(
            oversized.path().join("accept/script/source.js"),
            vec![b' '; MAX_FIXTURE_BYTES + 1],
        )
        .expect("write oversized fixture");
        let error =
            load_parser_corpus(oversized.path()).expect_err("oversized fixture must fail closed");
        assert!(error.contains("65536-byte limit"), "{error}");

        let non_utf8 = TestCorpus::new(&valid_manifest());
        fs::write(
            non_utf8.path().join("accept/script/source.js"),
            [0xff, 0xfe],
        )
        .expect("write non-UTF-8 fixture");
        let error =
            load_parser_corpus(non_utf8.path()).expect_err("non-UTF-8 fixture must fail closed");
        assert!(error.contains("is not valid UTF-8"), "{error}");
    }

    fn difference(id: &str, direction: &str) -> Value {
        json!({
            "id": id,
            "direction": direction,
            "rationale": "pinned QuickJS and the Oxc boundary intentionally differ",
            "regression": "candidate-accept/script/source.js"
        })
    }

    #[allow(clippy::too_many_lines)]
    fn valid_manifest() -> Value {
        json!({
            "schema": 1,
            "quickjs_release": "2026-06-04",
            "eval": "excluded-user-deferred",
            "cases": [
                {
                    "path": "accept/script/source.js",
                    "goal": "script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": [
                        "source-lexical",
                        "bindings",
                        "functions",
                        "expressions",
                        "classes-objects",
                        "statements",
                        "annex-b"
                    ],
                    "claims": [
                        "lexical.comments-hashbang-html",
                        "lexical.identifiers-keywords-unicode",
                        "lexical.literals-tokenization",
                        "lexical.asi-ambiguity",
                        "binding.declarations-patterns",
                        "function.forms",
                        "function.parameters",
                        "function.contextual-early-errors",
                        "function.cover-grammar-early-errors",
                        "expression.operators-assignment",
                        "expression.member-call-new-optional",
                        "expression.contextual-meta-super-new-target",
                        "class.object-literals",
                        "class.syntax-private-super",
                        "statement.basic-control",
                        "statement.iteration-labels",
                        "statement.abrupt-handlers",
                        "annex-b.html-comments",
                        "annex-b.block-functions"
                    ],
                    "evidence": ["quickjs/tests/test_language.js:39-675"],
                    "difference": null
                },
                {
                    "path": "accept/module/source.mjs",
                    "goal": "module",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["modules", "target-profile"],
                    "claims": [
                        "module.import-export",
                        "module.attributes",
                        "module.top-level-await-context",
                        "profile.accepted-es2025"
                    ],
                    "evidence": ["quickjs/quickjs.c:31477-31916"],
                    "difference": null
                },
                {
                    "path": "accept/strict-script/source.js",
                    "goal": "strict-script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["source-lexical"],
                    "claims": ["lexical.comments-hashbang-html"],
                    "evidence": ["quickjs/quickjs.c:36210-36299"],
                    "difference": null
                },
                {
                    "path": "accept/async-script/source.js",
                    "goal": "async-script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["functions"],
                    "claims": ["function.contextual-early-errors"],
                    "evidence": ["quickjs/quickjs.c:36543-36546"],
                    "difference": null
                },
                {
                    "path": "accept/strict-async-script/source.js",
                    "goal": "strict-async-script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["functions"],
                    "claims": ["function.contextual-early-errors"],
                    "evidence": ["quickjs/quickjs.c:36543-36546"],
                    "difference": null
                },
                {
                    "path": "reject/script/rejections.js",
                    "goal": "script",
                    "quickjs": "reject",
                    "frontend": "reject",
                    "families": [
                        "source-lexical",
                        "bindings",
                        "functions",
                        "expressions",
                        "classes-objects",
                        "statements",
                        "annex-b",
                        "target-profile"
                    ],
                    "claims": [
                        "lexical.comments-hashbang-html",
                        "lexical.identifiers-keywords-unicode",
                        "lexical.literals-tokenization",
                        "lexical.asi-ambiguity",
                        "lexical.malformed-token-rejections",
                        "binding.declarations-patterns",
                        "binding.collision-early-errors",
                        "function.parameters",
                        "function.contextual-early-errors",
                        "function.cover-grammar-early-errors",
                        "expression.operators-assignment",
                        "expression.member-call-new-optional",
                        "expression.contextual-meta-super-new-target",
                        "class.syntax-private-super",
                        "class.duplicate-proto-private-early-errors",
                        "statement.basic-control",
                        "statement.iteration-labels",
                        "statement.abrupt-handlers",
                        "statement.lexical-placement-collisions",
                        "annex-b.html-comments",
                        "annex-b.block-functions",
                        "profile.rejected-outside-target",
                        "profile.regexp-pattern-delegation"
                    ],
                    "evidence": ["quickjs/test262_errors.txt:1-58"],
                    "difference": null
                },
                {
                    "path": "reject/module/rejections.mjs",
                    "goal": "module",
                    "quickjs": "reject",
                    "frontend": "reject",
                    "families": ["modules"],
                    "claims": [
                        "module.import-export",
                        "module.attributes",
                        "module.top-level-await-context",
                        "module.early-errors"
                    ],
                    "evidence": ["quickjs/test262.conf:53"],
                    "difference": null
                },
                {
                    "path": "reject/strict-script/rejections.js",
                    "goal": "strict-script",
                    "quickjs": "reject",
                    "frontend": "reject",
                    "families": ["bindings"],
                    "claims": ["binding.strict-mode-early-errors"],
                    "evidence": ["quickjs/quickjs.c:36210"],
                    "difference": null
                }
            ]
        })
    }

    struct TestCorpus {
        root: PathBuf,
    }

    impl TestCorpus {
        fn new(manifest: &Value) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "quickjs-parser-manifest-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("create unique parser corpus");
            let corpus = Self { root };
            for case in manifest["cases"].as_array().expect("cases array") {
                corpus.write_fixture(case["path"].as_str().expect("case path"));
            }
            corpus.write_manifest(manifest);
            corpus
        }

        fn path(&self) -> &Path {
            &self.root
        }

        fn manifest(&self) -> Value {
            serde_json::from_slice(
                &fs::read(self.root.join("manifest.json")).expect("read manifest"),
            )
            .expect("parse manifest")
        }

        fn write_manifest(&self, manifest: &Value) {
            fs::write(
                self.root.join("manifest.json"),
                serde_json::to_vec_pretty(manifest).expect("serialize manifest"),
            )
            .expect("write manifest");
        }

        fn write_fixture(&self, relative: &str) {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("create fixture parent");
            fs::write(path, "0;\n").expect("write fixture");
        }
    }

    impl Drop for TestCorpus {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove parser corpus");
        }
    }
}
