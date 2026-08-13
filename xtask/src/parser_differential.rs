//! Differential checks for the Oxc/QuickJS syntax boundary.

use crate::parser_diagnostics::{DiagnosticReach, PINNED_DIAGNOSTICS, PinnedDiagnostic};
use crate::parser_productions::{PINNED_PRODUCTIONS, PinnedProduction, ProductionGoals};
use crate::{ProgramOutput, Status};
use crate::{collect_javascript_files, run_program_with_arguments_bounded, validate_executable};
use fusor_frontend::{
    CompilationGoal, FrontendOptions, GlobalScriptGoal, ParseMode, with_parsed_program,
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
/// The largest fixture the harness reads.
///
/// The bound is generous enough for the pinned parser's own resource limits:
/// provoking `Too many call arguments` requires more than 65535 arguments
/// (`quickjs.c:27143`), and each argument needs at least a digit and a comma.
const MAX_FIXTURE_BYTES: usize = 256 * 1024;
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
    diagnostic: Option<DeclaredDiagnostic>,
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
    ProfileParserResourceLimits,
}

impl ParserClaim {
    const ALL: [Self; 32] = [
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
        Self::ProfileParserResourceLimits,
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
            Self::ProfileParserResourceLimits => "profile.parser-resource-limits",
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
            | Self::ProfileRegexpPatternDelegation
            | Self::ProfileParserResourceLimits => ParserFamily::TargetProfile,
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
            | Self::ProfileRegexpPatternDelegation
            | Self::ProfileParserResourceLimits => matches!(expectation, Expectation::Reject),
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

/// Every claim/polarity pair the corpus must cover.
///
/// Derived from the claim table so adding a claim cannot silently weaken the
/// required coverage.
const fn required_claim_polarities() -> usize {
    let mut required = 0;
    let mut index = 0;
    while index < ParserClaim::ALL.len() {
        let claim = ParserClaim::ALL[index];
        if claim.allows_quickjs_expectation(Expectation::Accept) {
            required += 1;
        }
        if claim.allows_quickjs_expectation(Expectation::Reject) {
            required += 1;
        }
        index += 1;
    }
    required
}

const REQUIRED_CLAIM_POLARITIES: usize = required_claim_polarities();

/// Every pinned diagnostic the corpus is required to provoke.
fn reachable_pinned_diagnostics() -> usize {
    PINNED_DIAGNOSTICS
        .iter()
        .filter(|diagnostic| matches!(diagnostic.reach, DiagnosticReach::Reachable))
        .count()
}

/// A pinned diagnostic declared by a rejecting corpus fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeclaredDiagnostic {
    entry: &'static PinnedDiagnostic,
}

impl DeclaredDiagnostic {
    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        PINNED_DIAGNOSTICS
            .iter()
            .find(|entry| entry.id == value)
            .map(|entry| Self { entry })
            .ok_or_else(|| {
                format!("{location} contains unknown pinned QuickJS diagnostic `{value}`")
            })
    }

    /// Reports whether an observed oracle message matches the pinned text.
    ///
    /// `%c`, `%s`, and `%.*s` are the pinned format's runtime substitutions, so
    /// they match any run of characters (`%c` matches exactly one). Every other
    /// character must match literally, which keeps a fixture from claiming a
    /// diagnostic the oracle did not actually report.
    fn matches_observed(self, observed: &str) -> bool {
        matches_pinned_message(self.entry.message, observed)
    }
}

/// Matches an observed oracle message against a pinned format string.
fn matches_pinned_message(pinned: &str, observed: &str) -> bool {
    let (literal, rest) = next_pinned_segment(pinned);
    let Some(remaining) = observed.strip_prefix(literal) else {
        return false;
    };
    match rest {
        PinnedTail::End => remaining.is_empty(),
        PinnedTail::SingleCharacter(tail) => {
            let mut characters = remaining.chars();
            characters
                .next()
                .is_some_and(|_| matches_pinned_message(tail, characters.as_str()))
        }
        PinnedTail::AnyCharacters(tail) => (0..=remaining.len())
            .filter(|end| remaining.is_char_boundary(*end))
            .any(|end| matches_pinned_message(tail, &remaining[end..])),
    }
}

/// What follows the literal prefix of a pinned format string.
enum PinnedTail<'pinned> {
    End,
    SingleCharacter(&'pinned str),
    AnyCharacters(&'pinned str),
}

/// Splits a pinned format string into its literal prefix and next substitution.
fn next_pinned_segment(pinned: &str) -> (&str, PinnedTail<'_>) {
    let mut search = 0;
    while let Some(offset) = pinned[search..].find('%') {
        let start = search + offset;
        let tail = &pinned[start..];
        if let Some(rest) = tail.strip_prefix("%.*s") {
            return (&pinned[..start], PinnedTail::AnyCharacters(rest));
        }
        if let Some(rest) = tail.strip_prefix("%s") {
            return (&pinned[..start], PinnedTail::AnyCharacters(rest));
        }
        if let Some(rest) = tail.strip_prefix("%c") {
            return (&pinned[..start], PinnedTail::SingleCharacter(rest));
        }
        search = start + '%'.len_utf8();
    }
    (pinned, PinnedTail::End)
}

/// A pinned grammar production declared by a corpus fixture.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeclaredProduction {
    id: &'static str,
}

impl DeclaredProduction {
    fn from_manifest(value: &str, location: &str) -> Result<Self, String> {
        PINNED_PRODUCTIONS
            .iter()
            .find(|production| production.id == value)
            .map(|production| Self { id: production.id })
            .ok_or_else(|| {
                format!("{location} contains unknown pinned QuickJS grammar production `{value}`")
            })
    }

    fn entry(self) -> &'static PinnedProduction {
        PINNED_PRODUCTIONS
            .iter()
            .find(|production| production.id == self.id)
            .expect("a declared production comes from the pinned table")
    }
}

impl ProductionGoals {
    /// Reports whether a parse goal admits the production.
    const fn admits(self, goal: ParserGoal) -> bool {
        match self {
            Self::Any => true,
            Self::ModuleOnly => matches!(goal, ParserGoal::Module),
            Self::SloppyOnly => matches!(goal, ParserGoal::Script | ParserGoal::AsyncScript),
            Self::AwaitCapable => matches!(
                goal,
                ParserGoal::Module | ParserGoal::AsyncScript | ParserGoal::StrictAsyncScript
            ),
        }
    }
}

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
    diagnostics: usize,
    productions: usize,
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

        if !fixture.candidate_expectation.matches(candidate.accepted)
            || !fixture.oracle_expectation.matches(oracle.accepted)
        {
            failures.push(format_failure(fixture, &oracle, &candidate));
            continue;
        }
        if let Some(diagnostic) = fixture.diagnostic {
            let observed = oracle
                .detail
                .strip_prefix("SyntaxError:")
                .map_or(oracle.detail.as_str(), str::trim);
            let observed = observed.lines().next().unwrap_or_default().trim();
            if !diagnostic.matches_observed(observed) {
                failures.push(format!(
                    "--- {}\ndeclared pinned diagnostic: {} ({})\npinned message: {}\nQuickJS reported: {observed}",
                    fixture.path.display(),
                    diagnostic.entry.id,
                    diagnostic.entry.sites.join(", "),
                    diagnostic.entry.message
                ));
                continue;
            }
        }
        passed += 1;
    }
    println!(
        "parser coverage: {}/{} goals, {}/{} families, {}/{} claims, {}/{} required claim polarities, {}/{} grammar productions, {}/{} pinned diagnostics, {} intentional difference(s)",
        corpus.coverage.goals,
        REQUIRED_GOALS.len(),
        corpus.coverage.families,
        ParserFamily::ALL.len(),
        corpus.coverage.claims,
        ParserClaim::ALL.len(),
        corpus.coverage.claim_polarities,
        REQUIRED_CLAIM_POLARITIES,
        corpus.coverage.productions,
        PINNED_PRODUCTIONS.len(),
        corpus.coverage.diagnostics,
        reachable_pinned_diagnostics(),
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
    let mut fixtures = collect_parser_fixtures(corpus)?;
    for fixture in &fixtures {
        validate_parser_fixture_source(fixture)?;
    }
    let manifest = read_parser_manifest(corpus)?;
    let cases = validate_manifest_header(&manifest)?;
    let mut validation = ManifestValidation::new(corpus, &fixtures)?;
    for (index, case) in cases.iter().enumerate() {
        validation.validate_case(index, case)?;
    }
    let (coverage, diagnostics) = validation.finish()?;
    for (index, diagnostic) in diagnostics {
        fixtures[index].diagnostic = Some(diagnostic);
    }
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

/// Reports whether a fixture uses the excluded `eval` identifier.
///
/// The scan parses through the isolated frontend context so a deeply nested
/// fixture cannot exhaust the caller's stack; a fixture the front end rejects
/// falls back to a raw spelling scan.
fn contains_eval_identifier(source: &str, goal: ParserGoal) -> bool {
    let parsed = with_parsed_program(source, goal.candidate_options(), |unit| {
        unit.semantic().nodes().iter().any(|node| {
            node.kind()
                .identifier_name()
                .is_some_and(|name| name.as_str() == "eval")
        })
    });
    match parsed {
        Ok(found) => found,
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
    covered_diagnostics: BTreeSet<&'static str>,
    covered_productions: BTreeSet<&'static str>,
    fixture_diagnostics: Vec<(usize, DeclaredDiagnostic)>,
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
            covered_diagnostics: BTreeSet::new(),
            covered_productions: BTreeSet::new(),
            fixture_diagnostics: Vec::new(),
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
                "fusor",
                "frontend",
                "families",
                "claims",
                "productions",
                "diagnostic",
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
        let fusor = Expectation::from_manifest(
            required_string(case, "fusor", &location)?,
            &format!("{location} field `fusor`"),
        )?;
        let frontend = Expectation::from_manifest(
            required_string(case, "frontend", &location)?,
            &format!("{location} field `frontend`"),
        )?;
        if fusor != fixture.oracle_expectation || frontend != fixture.candidate_expectation {
            return Err(format!(
                "parser manifest case {} expectations ({fusor}/{frontend}) do not match its directory ({}/{})",
                relative.display(),
                fixture.oracle_expectation,
                fixture.candidate_expectation
            ));
        }

        let families = parse_families(case, &location)?;
        let claims = parse_claims(case, &location)?;
        validate_case_claims(&relative, &families, &claims, fusor, goal)?;
        let productions = parse_productions(case, &location, goal, fusor, &relative)?;
        let diagnostic = Self::validate_case_diagnostic(
            case.get("diagnostic")
                .expect("exact_object checked the diagnostic field"),
            fusor,
            &claims,
            &relative,
            &location,
        )?;
        validate_evidence(case, &location)?;
        validate_difference(
            case.get("difference")
                .expect("exact_object checked the difference field"),
            fusor,
            frontend,
            &relative,
            &location,
            &mut self.difference_ids,
        )?;
        if fusor != frontend {
            self.differences += 1;
        }

        self.covered_goals.insert(goal);
        self.covered_families.extend(families);
        for claim in &claims {
            self.covered_claim_polarities.insert((*claim, fusor));
        }
        self.covered_claims.extend(claims);
        for production in productions {
            self.covered_productions.insert(production.id);
        }
        if let Some(diagnostic) = diagnostic {
            self.covered_diagnostics.insert(diagnostic.entry.id);
            self.fixture_diagnostics.push((fixture_index, diagnostic));
        }
        Ok(())
    }

    /// Validates the pinned diagnostic a rejecting fixture declares.
    ///
    /// Every fixture the oracle rejects must name exactly the diagnostic it
    /// provokes, and accepted fixtures must not name one. The declared
    /// diagnostic must also agree with the fixture's claims, so a fixture cannot
    /// claim one early-error surface while provoking another.
    fn validate_case_diagnostic(
        value: &Value,
        fusor: Expectation,
        claims: &BTreeSet<ParserClaim>,
        relative: &Path,
        location: &str,
    ) -> Result<Option<DeclaredDiagnostic>, String> {
        if value.is_null() {
            return if matches!(fusor, Expectation::Reject) {
                Err(format!(
                    "parser manifest case {} must declare the pinned QuickJS diagnostic it provokes",
                    relative.display()
                ))
            } else {
                Ok(None)
            };
        }
        if matches!(fusor, Expectation::Accept) {
            return Err(format!(
                "parser manifest case {} must not declare a diagnostic because QuickJS accepts it",
                relative.display()
            ));
        }
        let declared = value
            .as_str()
            .ok_or_else(|| format!("{location} field `diagnostic` must be a string or null"))?;
        let diagnostic =
            DeclaredDiagnostic::from_manifest(declared, &format!("{location} field `diagnostic`"))?;
        if let DiagnosticReach::Unreachable(reason) = diagnostic.entry.reach {
            return Err(format!(
                "parser manifest case {} declares diagnostic `{declared}`, which the ledger records as unreachable: {reason}",
                relative.display()
            ));
        }
        let expected = diagnostic
            .entry
            .claims
            .iter()
            .map(|claim| {
                ParserClaim::from_manifest(
                    claim,
                    &format!("pinned diagnostic `{declared}` claim table"),
                )
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if !expected.is_subset(claims) {
            let missing = expected
                .difference(claims)
                .map(|claim| claim.manifest_name())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "parser manifest case {} declares diagnostic `{declared}` but omits its required claim(s) [{missing}]",
                relative.display()
            ));
        }
        Ok(Some(diagnostic))
    }

    fn finish(self) -> Result<(ParserCoverage, Vec<(usize, DeclaredDiagnostic)>), String> {
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
        for diagnostic in &PINNED_DIAGNOSTICS {
            let covered = self.covered_diagnostics.contains(diagnostic.id);
            match diagnostic.reach {
                DiagnosticReach::Reachable if !covered => {
                    return Err(format!(
                        "parser manifest has no fixture for reachable pinned diagnostic `{}` ({})",
                        diagnostic.id,
                        diagnostic.sites.join(", ")
                    ));
                }
                DiagnosticReach::Unreachable(reason) if covered => {
                    return Err(format!(
                        "parser manifest declares unreachable pinned diagnostic `{}`: {reason}",
                        diagnostic.id
                    ));
                }
                _ => {}
            }
        }
        for production in &PINNED_PRODUCTIONS {
            if !self.covered_productions.contains(production.id) {
                return Err(format!(
                    "parser manifest has no accepted fixture for pinned grammar production `{}` ({})",
                    production.id,
                    production.sites.join(", ")
                ));
            }
        }
        Ok((
            ParserCoverage {
                goals: self.covered_goals.len(),
                families: self.covered_families.len(),
                claims: self.covered_claims.len(),
                claim_polarities,
                diagnostics: self.covered_diagnostics.len(),
                productions: self.covered_productions.len(),
                differences: self.differences,
            },
            self.fixture_diagnostics,
        ))
    }
}

fn validate_case_claims(
    relative: &Path,
    families: &BTreeSet<ParserFamily>,
    claims: &BTreeSet<ParserClaim>,
    fusor: Expectation,
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
        if !claim.allows_quickjs_expectation(fusor) {
            return Err(format!(
                "parser manifest case {} claim `{}` does not allow QuickJS {fusor} coverage",
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

/// Parses the grammar productions a fixture declares.
///
/// Only fixtures the pinned oracle accepts may declare productions: rejection
/// does not prove the grammar is parsed. A declared production must also be
/// legal under the fixture's parse goal, so `import` forms cannot be claimed by
/// a Script fixture and `with` cannot be claimed by a strict one.
fn parse_productions(
    object: &Map<String, Value>,
    location: &str,
    goal: ParserGoal,
    fusor: Expectation,
    relative: &Path,
) -> Result<BTreeSet<DeclaredProduction>, String> {
    let values = object
        .get("productions")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{location} field `productions` must be an array"))?;
    if matches!(fusor, Expectation::Reject) {
        return if values.is_empty() {
            Ok(BTreeSet::new())
        } else {
            Err(format!(
                "parser manifest case {} must not declare grammar productions because QuickJS rejects it",
                relative.display()
            ))
        };
    }
    if values.is_empty() {
        return Err(format!(
            "parser manifest case {} must declare the grammar productions it exercises",
            relative.display()
        ));
    }
    let mut productions = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let value = value.as_str().ok_or_else(|| {
            format!("{location} field `productions` item {index} must be a string")
        })?;
        let production =
            DeclaredProduction::from_manifest(value, &format!("{location} field `productions`"))?;
        if !production.entry().goals.admits(goal) {
            return Err(format!(
                "parser manifest case {} declares production `{value}`, which parser goal `{}` does not admit",
                relative.display(),
                goal.manifest_name()
            ));
        }
        if !productions.insert(production) {
            return Err(format!(
                "{location} field `productions` contains duplicate `{value}`"
            ));
        }
    }
    Ok(productions)
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
    let value = value.strip_prefix("fusor/").unwrap_or(value);
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
    fusor: Expectation,
    frontend: Expectation,
    path: &Path,
    location: &str,
    ids: &mut BTreeSet<String>,
) -> Result<(), String> {
    let expected_direction = match (fusor, frontend) {
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
        diagnostic: None,
    })
}

#[derive(Debug)]
struct Observation {
    accepted: bool,
    detail: String,
}

/// Observes the Oxc front end on a fixture.
///
/// Parsing runs through the crate's isolated frontend context, which is the
/// path embedders use and the only one with a bounded, documented stack. A
/// deeply nested fixture would otherwise exhaust the harness thread's stack
/// before the front end could report a verdict.
fn observe_candidate(fixture: &ParserFixture) -> Result<Observation, String> {
    let source = validate_parser_fixture_source(fixture)?;
    match with_parsed_program(&source, fixture.goal.candidate_options(), |_| ()) {
        Ok(()) => Ok(Observation {
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
        DiagnosticReach, Expectation, MAX_FIXTURE_BYTES, PINNED_DIAGNOSTICS, PINNED_PRODUCTIONS,
        PinnedDiagnostic, REQUIRED_CLAIM_POLARITIES, classify_fixture, classify_oracle_output,
        load_parser_corpus, matches_pinned_message, observe_candidate,
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
                diagnostic: None,
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
                diagnostic: None,
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
            "fusor/tests/../quickjs.c:1",
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
        manifest["cases"][0]["evidence"] = json!(["quickjs.c:1", "fusor/quickjs.c:1-1"]);
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
        case.insert("fusor".to_owned(), json!("reject"));
        case.insert(
            "diagnostic".to_owned(),
            json!("unexpected-token-in-expression"),
        );
        let claims = case
            .get_mut("claims")
            .and_then(Value::as_array_mut)
            .expect("claims array");
        claims.retain(|claim| {
            !matches!(
                claim.as_str(),
                Some("function.forms" | "class.object-literals")
            )
        });
        claims.push(json!("lexical.malformed-token-rejections"));
        // The case flips to a QuickJS rejection, which may not declare grammar
        // productions; they move to another accepting case of the same goal.
        let moved = case
            .insert("productions".to_owned(), json!([]))
            .expect("productions array");
        let cases = manifest["cases"].as_array_mut().expect("cases array");
        for production in moved.as_array().expect("productions array") {
            let id = production.as_str().expect("production id");
            let entry = PINNED_PRODUCTIONS
                .iter()
                .find(|candidate| candidate.id == id)
                .expect("moved productions come from the pinned table");
            let recipient = cases
                .iter_mut()
                .find(|case| {
                    case["fusor"] == "accept"
                        && case["path"] != "candidate-accept/script/source.js"
                        && super::ParserGoal::from_manifest(
                            case["goal"].as_str().expect("case goal"),
                            "synthetic case goal",
                        )
                        .is_ok_and(|goal| entry.goals.admits(goal))
                })
                .expect("another accepting case admits the moved production");
            recipient["productions"]
                .as_array_mut()
                .expect("productions array")
                .push(production.clone());
        }
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
        manifest["cases"][0]["difference"] = difference("FUS-OXC-STALE", "frontend-accept");
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
            &difference("FUS-OXC-ONE", "frontend-accept"),
            Expectation::Reject,
            Expectation::Accept,
            path,
            "case",
            &mut ids,
        )
        .expect("valid difference");

        let duplicate = validate_difference(
            &difference("FUS-OXC-ONE", "frontend-accept"),
            Expectation::Reject,
            Expectation::Accept,
            path,
            "case",
            &mut ids,
        )
        .expect_err("difference IDs must be unique");
        assert!(duplicate.contains("declared more than once"), "{duplicate}");

        let wrong_direction = validate_difference(
            &difference("FUS-OXC-TWO", "frontend-reject"),
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
                "id": "FUS-OXC-THREE",
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
        let cases = manifest["cases"].as_array_mut().expect("cases array");
        for case in cases.iter_mut() {
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
                                | "profile.parser-resource-limits"
                        )
                    )
                });
        }
        // Cases whose only claims were target-profile claims no longer describe
        // anything, so they are dropped rather than left with empty lists.
        let removed = cases
            .iter()
            .filter(|case| case["claims"].as_array().is_some_and(Vec::is_empty))
            .map(|case| case["path"].as_str().expect("case path").to_owned())
            .collect::<Vec<_>>();
        cases.retain(|case| !case["claims"].as_array().is_some_and(Vec::is_empty));
        for path in removed {
            fs::remove_file(missing_family.path().join(path)).expect("remove fixture");
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
        assert_eq!(coverage.claims, super::ParserClaim::ALL.len());
        assert_eq!(coverage.claim_polarities, REQUIRED_CLAIM_POLARITIES);
        assert_eq!(coverage.diagnostics, super::reachable_pinned_diagnostics());

        // `lexical.asi-ambiguity` is not required by any pinned diagnostic, so
        // removing it isolates the claim-polarity rule from the diagnostic rule.
        let missing_accept = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_accept.manifest();
        manifest["cases"][0]["claims"]
            .as_array_mut()
            .expect("claims array")
            .retain(|claim| claim != "lexical.asi-ambiguity");
        missing_accept.write_manifest(&manifest);
        let error =
            load_parser_corpus(missing_accept.path()).expect_err("accept polarity is required");
        assert!(
            error.contains(
                "claim `lexical.asi-ambiguity` is missing required QuickJS accept coverage"
            ),
            "{error}"
        );

        let missing_reject = TestCorpus::new(&valid_manifest());
        let mut manifest = missing_reject.manifest();
        for case in manifest["cases"].as_array_mut().expect("cases array") {
            if case["fusor"] == "reject" {
                case["claims"]
                    .as_array_mut()
                    .expect("claims array")
                    .retain(|claim| claim != "lexical.asi-ambiguity");
            }
        }
        missing_reject.write_manifest(&manifest);
        let error =
            load_parser_corpus(missing_reject.path()).expect_err("reject polarity is required");
        assert!(
            error.contains(
                "claim `lexical.asi-ambiguity` is missing required QuickJS reject coverage"
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
            .find(|case| case["fusor"] == "reject")
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
            .find(|case| case["goal"] == "module" && case["fusor"] == "accept")
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
            .find(|case| case["goal"] == "script" && case["fusor"] == "reject")
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
            .find(|case| case["goal"] == "module" && case["fusor"] == "reject")
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
        assert!(error.contains("262144-byte limit"), "{error}");

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

    #[test]
    fn pinned_message_wildcards_match_only_the_substituted_text() {
        assert!(matches_pinned_message("expecting '%c'", "expecting ';'"));
        assert!(matches_pinned_message("expecting '%c'", "expecting ')'"));
        assert!(!matches_pinned_message("expecting '%c'", "expecting ';;'"));
        assert!(!matches_pinned_message("expecting '%c'", "expecting ''"));
        assert!(matches_pinned_message(
            "'%s' is a reserved identifier",
            "'enum' is a reserved identifier"
        ));
        assert!(!matches_pinned_message(
            "'%s' is a reserved identifier",
            "'enum' is a reserved word"
        ));
        assert!(matches_pinned_message(
            "unexpected token in expression: '%.*s'",
            "unexpected token in expression: '@'"
        ));
        assert!(matches_pinned_message(
            "a declaration in the head of a for-%s loop can't have an initializer",
            "a declaration in the head of a for-in loop can't have an initializer"
        ));
        assert!(matches_pinned_message("stack overflow", "stack overflow"));
        assert!(!matches_pinned_message(
            "stack overflow",
            "stack overflowed"
        ));
        assert!(matches_pinned_message(
            "\"use strict\" not allowed in function with default or destructuring parameter",
            "\"use strict\" not allowed in function with default or destructuring parameter"
        ));
    }

    #[test]
    fn pinned_diagnostic_table_is_a_closed_well_formed_vocabulary() {
        let mut ids = BTreeSet::new();
        let mut sites = BTreeSet::new();
        for diagnostic in &PINNED_DIAGNOSTICS {
            assert!(
                ids.insert(diagnostic.id),
                "duplicate diagnostic id `{}`",
                diagnostic.id
            );
            assert!(
                !diagnostic.message.is_empty(),
                "diagnostic `{}` has no pinned message",
                diagnostic.id
            );
            assert!(
                !diagnostic.sites.is_empty(),
                "diagnostic `{}` records no call site",
                diagnostic.id
            );
            assert!(
                !diagnostic.claims.is_empty(),
                "diagnostic `{}` records no claim",
                diagnostic.id
            );
            for claim in diagnostic.claims {
                super::ParserClaim::from_manifest(claim, "diagnostic claim table").unwrap_or_else(
                    |error| {
                        panic!(
                            "diagnostic `{}` claim is not a ledger claim: {error}",
                            diagnostic.id
                        )
                    },
                );
            }
            for site in diagnostic.sites {
                assert!(
                    site.starts_with("quickjs.c:"),
                    "diagnostic `{}` site `{site}` is not a pinned anchor",
                    diagnostic.id
                );
                assert!(
                    sites.insert(*site),
                    "call site `{site}` is claimed by more than one diagnostic"
                );
            }
            if let DiagnosticReach::Unreachable(reason) = diagnostic.reach {
                assert!(
                    !reason.trim().is_empty(),
                    "unreachable diagnostic `{}` records no reason",
                    diagnostic.id
                );
            }
        }
        assert!(
            super::reachable_pinned_diagnostics() < PINNED_DIAGNOSTICS.len(),
            "the ledger must record which pinned diagnostics are unreachable"
        );
    }

    #[test]
    fn manifest_requires_a_matching_diagnostic_on_every_rejecting_case() {
        let missing = TestCorpus::new(&valid_manifest());
        let mut manifest = missing.manifest();
        let reject = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "reject")
            .expect("reject case");
        reject["diagnostic"] = Value::Null;
        missing.write_manifest(&manifest);
        let error = load_parser_corpus(missing.path()).expect_err("diagnostic is required");
        assert!(
            error.contains("must declare the pinned QuickJS diagnostic it provokes"),
            "{error}"
        );

        let on_accept = TestCorpus::new(&valid_manifest());
        let mut manifest = on_accept.manifest();
        let accept = manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "accept")
            .expect("accept case");
        accept["diagnostic"] = json!("unexpected-character");
        on_accept.write_manifest(&manifest);
        let error = load_parser_corpus(on_accept.path())
            .expect_err("accepted fixtures must not declare a diagnostic");
        assert!(
            error.contains("must not declare a diagnostic because QuickJS accepts it"),
            "{error}"
        );

        let unknown = TestCorpus::new(&valid_manifest());
        let mut manifest = unknown.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "reject")
            .expect("reject case")["diagnostic"] = json!("not-a-pinned-diagnostic");
        unknown.write_manifest(&manifest);
        let error =
            load_parser_corpus(unknown.path()).expect_err("unknown diagnostics must fail closed");
        assert!(
            error.contains("unknown pinned QuickJS diagnostic `not-a-pinned-diagnostic`"),
            "{error}"
        );
    }

    #[test]
    fn manifest_rejects_unreachable_diagnostics_and_missing_reachable_ones() {
        let unreachable = PINNED_DIAGNOSTICS
            .iter()
            .find(|diagnostic| matches!(diagnostic.reach, DiagnosticReach::Unreachable(_)))
            .expect("the ledger records unreachable diagnostics");
        let declared = TestCorpus::new(&valid_manifest());
        let mut manifest = declared.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "reject")
            .expect("reject case")["diagnostic"] = json!(unreachable.id);
        declared.write_manifest(&manifest);
        let error = load_parser_corpus(declared.path())
            .expect_err("unreachable diagnostics must not be declared");
        assert!(
            error.contains("the ledger records as unreachable"),
            "{error}"
        );

        let dropped = TestCorpus::new(&valid_manifest());
        let mut manifest = dropped.manifest();
        let cases = manifest["cases"].as_array_mut().expect("cases array");
        let index = cases
            .iter()
            .position(|case| case["diagnostic"] == "unexpected-character")
            .expect("generated diagnostic case");
        let removed = cases.remove(index);
        fs::remove_file(
            dropped
                .path()
                .join(removed["path"].as_str().expect("case path")),
        )
        .expect("remove diagnostic fixture");
        dropped.write_manifest(&manifest);
        let error = load_parser_corpus(dropped.path())
            .expect_err("reachable diagnostics require a fixture");
        assert!(
            error.contains("no fixture for reachable pinned diagnostic `unexpected-character`"),
            "{error}"
        );
    }

    #[test]
    fn manifest_requires_a_diagnostic_to_carry_its_ledger_claims() {
        let corpus = TestCorpus::new(&valid_manifest());
        let mut manifest = corpus.manifest();
        let cases = manifest["cases"].as_array_mut().expect("cases array");
        let case = cases
            .iter_mut()
            .find(|case| case["diagnostic"] == "unexpected-character")
            .expect("generated diagnostic case");
        case["claims"] = json!(["lexical.literals-tokenization"]);
        case["families"] = json!(["source-lexical"]);
        corpus.write_manifest(&manifest);
        let error = load_parser_corpus(corpus.path())
            .expect_err("a declared diagnostic pins its required claims");
        assert!(
            error.contains("omits its required claim(s) [lexical.malformed-token-rejections]"),
            "{error}"
        );
    }

    #[test]
    fn pinned_production_table_is_a_closed_well_formed_vocabulary() {
        let mut ids = BTreeSet::new();
        for production in &PINNED_PRODUCTIONS {
            assert!(
                ids.insert(production.id),
                "duplicate production id `{}`",
                production.id
            );
            assert!(
                !production.grammar.is_empty(),
                "production `{}` names no grammar",
                production.id
            );
            assert!(
                !production.sites.is_empty(),
                "production `{}` records no parser anchor",
                production.id
            );
            for site in production.sites {
                assert!(
                    site.starts_with("quickjs.c:"),
                    "production `{}` site `{site}` is not a pinned anchor",
                    production.id
                );
            }
            assert!(
                super::REQUIRED_GOALS
                    .into_iter()
                    .any(|goal| production.goals.admits(goal)),
                "production `{}` is admitted by no parser goal",
                production.id
            );
        }
    }

    #[test]
    fn manifest_requires_accepted_coverage_for_every_grammar_production() {
        let dropped = TestCorpus::new(&valid_manifest());
        let mut manifest = dropped.manifest();
        for case in manifest["cases"].as_array_mut().expect("cases array") {
            case["productions"]
                .as_array_mut()
                .expect("productions array")
                .retain(|production| production != "statement.switch");
        }
        dropped.write_manifest(&manifest);
        let error = load_parser_corpus(dropped.path())
            .expect_err("every production needs accepted coverage");
        assert!(
            error.contains("no accepted fixture for pinned grammar production `statement.switch`"),
            "{error}"
        );

        let unknown = TestCorpus::new(&valid_manifest());
        let mut manifest = unknown.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "accept")
            .expect("accept case")["productions"]
            .as_array_mut()
            .expect("productions array")
            .push(json!("not-a-pinned-production"));
        unknown.write_manifest(&manifest);
        let error =
            load_parser_corpus(unknown.path()).expect_err("unknown productions must fail closed");
        assert!(
            error.contains("unknown pinned QuickJS grammar production `not-a-pinned-production`"),
            "{error}"
        );

        let empty = TestCorpus::new(&valid_manifest());
        let mut manifest = empty.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "accept")
            .expect("accept case")["productions"] = json!([]);
        empty.write_manifest(&manifest);
        let error = load_parser_corpus(empty.path())
            .expect_err("accepted fixtures must declare productions");
        assert!(
            error.contains("must declare the grammar productions it exercises"),
            "{error}"
        );
    }

    #[test]
    fn manifest_rejects_productions_a_goal_cannot_admit() {
        let module_in_script = TestCorpus::new(&valid_manifest());
        let mut manifest = module_in_script.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "accept" && case["goal"] == "script")
            .expect("accepting Script case")["productions"]
            .as_array_mut()
            .expect("productions array")
            .push(json!("module.import-declaration"));
        module_in_script.write_manifest(&manifest);
        let error = load_parser_corpus(module_in_script.path())
            .expect_err("Module productions require the Module goal");
        assert!(
            error.contains("which parser goal `script` does not admit"),
            "{error}"
        );

        let sloppy_in_strict = TestCorpus::new(&valid_manifest());
        let mut manifest = sloppy_in_strict.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "accept" && case["goal"] == "strict-script")
            .expect("accepting strict Script case")["productions"]
            .as_array_mut()
            .expect("productions array")
            .push(json!("statement.with"));
        sloppy_in_strict.write_manifest(&manifest);
        let error = load_parser_corpus(sloppy_in_strict.path())
            .expect_err("sloppy-only productions require a sloppy goal");
        assert!(
            error.contains("which parser goal `strict-script` does not admit"),
            "{error}"
        );

        let on_rejection = TestCorpus::new(&valid_manifest());
        let mut manifest = on_rejection.manifest();
        manifest["cases"]
            .as_array_mut()
            .expect("cases array")
            .iter_mut()
            .find(|case| case["fusor"] == "reject")
            .expect("reject case")["productions"] = json!(["statement.block"]);
        on_rejection.write_manifest(&manifest);
        let error = load_parser_corpus(on_rejection.path())
            .expect_err("rejections do not prove grammar coverage");
        assert!(
            error.contains("must not declare grammar productions because QuickJS rejects it"),
            "{error}"
        );
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
        let mut manifest = json!({
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
                    "evidence": ["fusor/tests/test_language.js:39-675"],
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
                    "evidence": ["fusor/quickjs.c:31477-31916"],
                    "difference": null
                },
                {
                    "path": "accept/strict-script/source.js",
                    "goal": "strict-script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["source-lexical"],
                    "claims": ["lexical.comments-hashbang-html"],
                    "evidence": ["fusor/quickjs.c:36210-36299"],
                    "difference": null
                },
                {
                    "path": "accept/async-script/source.js",
                    "goal": "async-script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["functions"],
                    "claims": ["function.contextual-early-errors"],
                    "evidence": ["fusor/quickjs.c:36543-36546"],
                    "difference": null
                },
                {
                    "path": "accept/strict-async-script/source.js",
                    "goal": "strict-async-script",
                    "quickjs": "accept",
                    "frontend": "accept",
                    "families": ["functions"],
                    "claims": ["function.contextual-early-errors"],
                    "evidence": ["fusor/quickjs.c:36543-36546"],
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
                    "evidence": ["fusor/test262_errors.txt:1-58"],
                    "diagnostic": "unexpected-token-in-expression",
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
                    "evidence": ["fusor/test262.conf:53"],
                    "diagnostic": "invalid-export-syntax",
                    "difference": null
                },
                {
                    "path": "reject/strict-script/rejections.js",
                    "goal": "strict-script",
                    "quickjs": "reject",
                    "frontend": "reject",
                    "families": ["bindings"],
                    "claims": ["binding.strict-mode-early-errors"],
                    "evidence": ["fusor/quickjs.c:36210"],
                    "diagnostic": "strict-invalid-variable-name",
                    "difference": null
                }
            ]
        });
        for case in manifest["cases"].as_array_mut().expect("cases array") {
            let object = case.as_object_mut().expect("case object");
            if !object.contains_key("diagnostic") {
                object.insert("diagnostic".to_owned(), Value::Null);
            }
            object.entry("productions").or_insert_with(|| json!([]));
        }
        // The synthetic corpus must satisfy the closed production vocabulary, so
        // every pinned production is spread across the accepting cases whose goal
        // admits it, and every accepting case receives at least one.
        for (next, production) in PINNED_PRODUCTIONS.iter().enumerate() {
            let cases = manifest["cases"].as_array_mut().expect("cases array");
            let admitting = cases
                .iter()
                .enumerate()
                .filter(|(_, case)| {
                    case["fusor"] == "accept"
                        && super::ParserGoal::from_manifest(
                            case["goal"].as_str().expect("case goal"),
                            "synthetic case goal",
                        )
                        .is_ok_and(|goal| production.goals.admits(goal))
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            assert!(
                !admitting.is_empty(),
                "no accepting case admits production `{}`",
                production.id
            );
            let index = admitting[next % admitting.len()];
            cases[index]["productions"]
                .as_array_mut()
                .expect("productions array")
                .push(json!(production.id));
        }
        for case in manifest["cases"].as_array_mut().expect("cases array") {
            if case["fusor"] != "accept"
                || !case["productions"]
                    .as_array()
                    .expect("productions array")
                    .is_empty()
            {
                continue;
            }
            let goal = super::ParserGoal::from_manifest(
                case["goal"].as_str().expect("case goal"),
                "synthetic case goal",
            )
            .expect("synthetic goals are ledger goals");
            let production = PINNED_PRODUCTIONS
                .iter()
                .find(|production| production.goals.admits(goal))
                .expect("every goal admits some production");
            case["productions"]
                .as_array_mut()
                .expect("productions array")
                .push(json!(production.id));
        }
        let covered = manifest["cases"]
            .as_array()
            .expect("cases array")
            .iter()
            .filter_map(|case| case["diagnostic"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        let cases = manifest["cases"].as_array_mut().expect("cases array");
        for diagnostic in &PINNED_DIAGNOSTICS {
            if !matches!(diagnostic.reach, DiagnosticReach::Reachable)
                || covered.contains(diagnostic.id)
            {
                continue;
            }
            cases.push(diagnostic_case(diagnostic));
        }
        manifest
    }

    /// Builds a minimal rejecting case that covers one pinned diagnostic.
    ///
    /// The synthetic corpus must satisfy the ledger's closed diagnostic
    /// requirement, so every reachable diagnostic the hand-written cases do not
    /// already declare gets a generated case with a goal its claims permit.
    fn diagnostic_case(diagnostic: &PinnedDiagnostic) -> Value {
        let claims = diagnostic
            .claims
            .iter()
            .map(|claim| {
                super::ParserClaim::from_manifest(claim, "diagnostic claim table")
                    .expect("pinned diagnostic claims are ledger claims")
            })
            .collect::<BTreeSet<_>>();
        let goal = [
            super::ParserGoal::Script,
            super::ParserGoal::StrictScript,
            super::ParserGoal::Module,
        ]
        .into_iter()
        .find(|goal| claims.iter().all(|claim| claim.allows_goal(*goal)))
        .expect("every pinned diagnostic claim set permits some goal");
        let families = claims
            .iter()
            .map(|claim| claim.family().manifest_name())
            .collect::<BTreeSet<_>>();
        let extension = if matches!(goal, super::ParserGoal::Module) {
            "mjs"
        } else {
            "js"
        };
        json!({
            "path": format!(
                "reject/{}/{}.{extension}",
                goal.manifest_name(),
                diagnostic.id
            ),
            "goal": goal.manifest_name(),
            "quickjs": "reject",
            "frontend": "reject",
            "families": families.into_iter().collect::<Vec<_>>(),
            "claims": claims
                .iter()
                .map(|claim| claim.manifest_name())
                .collect::<Vec<_>>(),
            "productions": [],
            "diagnostic": diagnostic.id,
            "evidence": diagnostic.sites,
            "difference": null
        })
    }

    struct TestCorpus {
        root: PathBuf,
    }

    impl TestCorpus {
        fn new(manifest: &Value) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "fusor-parser-manifest-{}-{}",
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
