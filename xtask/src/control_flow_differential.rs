//! Shared bounded runtime differential gate for executable language milestones.

use crate::{
    ProgramOutput, Status, run_program_with_arguments_bounded,
    run_program_with_arguments_bounded_input, validate_executable,
};
use quickjs::{
    DynamicFunctionLimits, call_with_dynamic_function_support, construct_dynamic_function,
};
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsString, JsValue, Runtime, RuntimeLimits,
    ValueKind,
};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub(crate) const DEFAULT_CONTROL_FLOW_CORPUS: &str = "tests/control-flow/manifest.json";
pub(crate) const DEFAULT_ASYNC_FUNCTION_CORPUS: &str = "tests/async-function/manifest.json";
pub(crate) const DEFAULT_ASYNC_GENERATOR_CORPUS: &str = "tests/async-generator/manifest.json";
pub(crate) const DEFAULT_ERROR_CORPUS: &str = "tests/error/manifest.json";
pub(crate) const DEFAULT_FUNCTION_APPLY_CORPUS: &str = "tests/function-apply/manifest.json";
pub(crate) const DEFAULT_FUNCTION_BIND_CORPUS: &str = "tests/function-bind/manifest.json";
pub(crate) const DEFAULT_GENERATOR_CORPUS: &str = "tests/generator/manifest.json";
pub(crate) const DEFAULT_ITERATOR_CORPUS: &str = "tests/iterator/manifest.json";
pub(crate) const DEFAULT_CALL_SPREAD_CORPUS: &str = "tests/call-spread/manifest.json";
pub(crate) const DEFAULT_OBJECT_LEGACY_CORPUS: &str = "tests/object-legacy/manifest.json";
pub(crate) const DEFAULT_PROMISE_CORE_CORPUS: &str = "tests/promise-core/manifest.json";
pub(crate) const DEFAULT_STRING_HTML_CORPUS: &str = "tests/string-html/manifest.json";
pub(crate) const MAX_CONTROL_FLOW_TIMEOUT_MS: u64 = 60_000;
pub(crate) const CANDIDATE_WORKER_COMMAND: &str = "__control-flow-candidate-worker";
pub(crate) const ASYNC_FUNCTION_CANDIDATE_WORKER_COMMAND: &str =
    "__async-function-candidate-worker";

const EXPECTED_ORACLE_BANNER: &str = "QuickJS version 2026-06-04";
const EXPECTED_MANIFEST_RELEASE: &str = "2026-06-04";
const MANIFEST_SCHEMA_VERSION: u64 = 1;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_CASES: usize = 128;
const MAX_CASE_ID_BYTES: usize = 64;
const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_EXPECTED_STRING_BYTES: usize = 4 * 1024;
const MAX_EXPECTED_ERROR_NAME_BYTES: usize = 64;
const MAX_EXPECTED_ERROR_MESSAGE_BYTES: usize = 2 * 1024;
const MAX_GENERATED_ORACLE_SOURCE_BYTES: usize = 512 * 1024;
const MAX_ORACLE_VERSION_STREAM_BYTES: usize = 16 * 1024;
const ORACLE_MEMORY_LIMIT_BYTES: &str = "67108864";
const ORACLE_STACK_SIZE_BYTES: &str = "1048576";
const MAX_JSON_ESCAPED_ASCII_BYTE_BYTES: usize = 6;
const MAX_ORACLE_INDEX_BYTES: usize = decimal_digits(MAX_CASES - 1);
const MAX_ORACLE_PROTOCOL_PREFIX_BYTES: usize = MAX_ORACLE_INDEX_BYTES + 1;
const MAX_ORACLE_STRING_JSON_BYTES: usize = br#"{"kind":"string","value":""}"#.len()
    + MAX_JSON_ESCAPED_ASCII_BYTE_BYTES * MAX_EXPECTED_STRING_BYTES;
const MAX_ORACLE_THROW_JSON_BYTES: usize = br#"{"kind":"throw","name":"","message":""}"#.len()
    + MAX_JSON_ESCAPED_ASCII_BYTE_BYTES
        * (MAX_EXPECTED_ERROR_NAME_BYTES + MAX_EXPECTED_ERROR_MESSAGE_BYTES);
const MAX_ORACLE_RESULT_JSON_BYTES: usize =
    if MAX_ORACLE_STRING_JSON_BYTES > MAX_ORACLE_THROW_JSON_BYTES {
        MAX_ORACLE_STRING_JSON_BYTES
    } else {
        MAX_ORACLE_THROW_JSON_BYTES
    };
const MAX_ORACLE_RESULT_LINE_BYTES: usize =
    MAX_ORACLE_PROTOCOL_PREFIX_BYTES + MAX_ORACLE_RESULT_JSON_BYTES;
const MAX_ORACLE_CASE_STREAM_BYTES: usize = MAX_ORACLE_RESULT_LINE_BYTES + 1;
const MAX_CANDIDATE_WORKER_STREAM_BYTES: usize = MAX_ORACLE_RESULT_JSON_BYTES + 1;
const MAX_REPORTED_MISMATCHES: usize = 32;
const MAX_ERROR_PREVIEW_BYTES: usize = 512;
const MAX_TEMP_DIRECTORY_ATTEMPTS: usize = 128;
const CANDIDATE_INSTRUCTION_FUEL: u64 = 1_000_000;
const CANDIDATE_SOURCE_BYTES: usize = 32 * 1024;

static TEMP_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

const fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

const REQUIRED_COVERAGE: &[&str] = &[
    "array-apply",
    "array-constructor-call",
    "array-constructor-construct",
    "array-constructor-elements",
    "array-constructor-intrinsics",
    "array-constructor-length",
    "array-constructor-range-error",
    "array-dense-literal",
    "array-elision",
    "array-elision-enumeration",
    "array-elision-evaluation-order",
    "array-evaluation-order",
    "array-for-in",
    "array-index-boundary",
    "array-index-read",
    "array-index-write",
    "array-length",
    "array-length-coercion",
    "array-length-double-coercion",
    "array-length-range-error",
    "array-length-truncate",
    "array-object-tag",
    "catch-binding",
    "catch-captured-abrupt-cell",
    "catch-captured-binding",
    "catch-control-cleanup",
    "catch-cross-frame",
    "catch-dynamic-function-syntax",
    "catch-engine-error",
    "catch-engine-error-brand",
    "catch-for-in-cleanup",
    "catch-getter-abrupt",
    "catch-normal-completion",
    "catch-optional-binding",
    "catch-rethrow",
    "captures",
    "chained-labels",
    "for-in-getter-free",
    "for-in-inherited-shadowing",
    "for-in-key-order",
    "for-in-labeled-control",
    "for-in-lexical-capture",
    "for-in-member-target",
    "for-in-nullish",
    "for-in-prototype-snapshot",
    "for-in-string-indices",
    "finally-break",
    "finally-captured-finalizer-cell",
    "finally-captured-protected-cell",
    "finally-catch",
    "finally-continue",
    "finally-nested-order",
    "finally-normal",
    "finally-override-break",
    "finally-override-continue",
    "finally-override-return",
    "finally-override-throw",
    "finally-override-throw-for-in",
    "finally-preserved-return",
    "finally-rethrow",
    "generic-labeled-break",
    "labeled-break",
    "labeled-continue",
    "labeled-switch",
    "nested-loop-switch",
    "switch-case-order",
    "switch-default-middle",
    "switch-discriminant-before-scope",
    "switch-fallthrough",
    "switch-lexical-tdz",
    "switch-no-match",
];

const FUNCTION_APPLY_REQUIRED_COVERAGE: &[&str] = &[
    "argument-limit",
    "boxed-string-array-like",
    "metadata-writable-enumerable",
    "function-array-like",
    "indexed-get-abrupt",
    "indexed-get-order",
    "inherited-index",
    "length-get-order",
    "length-does-not-wrap",
    "length-to-length-coercion",
    "missing-index-undefined",
    "mutation-between-indexed-gets",
    "native-source",
    "nonconstructable",
    "nullish-argument-list",
    "ordinary-array-like",
    "primitive-argument-list-rejection",
    "receiver-forwarding",
    "target-validation-order",
];

const FUNCTION_BIND_REQUIRED_COVERAGE: &[&str] = &[
    "argument-prepending",
    "bound-construction",
    "bound-length-rules",
    "bound-name-rules",
    "bound-tostring",
    "call-apply-forwarding",
    "instanceof-bound-unwrap",
    "instanceof-custom-symbol",
    "instanceof-nonobject-right",
    "instanceof-ordinary",
    "instanceof-plain-object-symbol",
    "instanceof-primitive-left",
    "metadata-writable-enumerable",
    "native-source",
    "native-target-binding",
    "nonconstructable",
    "nonconstructor-name",
    "receiver-override",
    "symbol-has-instance-descriptor",
    "symbol-has-instance-native",
    "target-validation-order",
    "typeof-bound",
    "new-target-substitution",
];

const CALL_SPREAD_REQUIRED_COVERAGE: &[&str] = &[
    "abrupt-close",
    "argument-order",
    "array-like-length",
    "construction",
    "construction-empty",
    "dense-spread-order",
    "empty-spread",
    "evaluation-order",
    "iterator-consumption",
    "member-receiver",
    "multiple-spreads",
    "noncallable",
    "noniterable",
    "nonobject-list",
    "string-iteration",
];

const ERROR_REQUIRED_COVERAGE: &[&str] = &[
    "aggregate-call",
    "aggregate-acquisition-abrupt",
    "aggregate-close-original-wins",
    "aggregate-construct",
    "aggregate-errors-descriptor",
    "aggregate-iteration",
    "aggregate-iteration-abrupt",
    "aggregate-missing-errors",
    "aggregate-order",
    "aggregate-step-abrupt-close",
    "caught-error",
    "cause-absent",
    "cause-abrupt",
    "cause-descriptor",
    "cause-getter-order",
    "cause-inherited",
    "cause-present",
    "constructor-metadata",
    "error-brand",
    "error-call",
    "error-call-ignores-this",
    "error-construct",
    "error-families",
    "error-is-error",
    "error-is-error-descriptor",
    "family-cause",
    "family-prototype-metadata",
    "internal-error",
    "message-absent",
    "message-abrupt",
    "message-coercion",
    "message-descriptor",
    "message-empty",
    "new-target-aggregate",
    "new-target-intrinsic-fallback",
    "new-target-prototype",
    "own-property-order",
    "prototype-chain",
    "prototype-descriptors",
    "realm-isolation",
    "realm-owned-prototype",
    "stack-descriptor",
    "stack-format",
    "stack-snapshot",
    "surface-own-properties",
    "thrown-error",
    "tostring-coercion-abrupt",
    "tostring-default-message",
    "tostring-default-name",
    "tostring-descriptor",
    "tostring-empty-message",
    "tostring-empty-name",
    "tostring-generic",
    "tostring-getter-order",
    "tostring-message-abrupt",
    "tostring-message-coercion",
    "tostring-name-abrupt",
    "tostring-name-coercion",
    "tostring-primitive-receiver",
];

// `for await` and destructuring `for-of` heads are separate fail-closed milestones.
const ITERATOR_REQUIRED_COVERAGE: &[&str] = &[
    "array-spread",
    "array-spread-elision",
    "array-spread-evaluation-order",
    "array-spread-multiple",
    "array-spread-pending-hole-length",
    "array-spread-string",
    "array-spread-string-code-point",
    "for-of-array",
    "for-of-binding-throw-close",
    "for-of-body-throw-close",
    "for-of-break-close",
    "for-of-capture-freshness",
    "for-of-close-nonobject-result",
    "for-of-close-original-wins",
    "for-of-continue-no-close",
    "for-of-custom-iterator",
    "for-of-done-error-no-close",
    "for-of-done-skips-value",
    "for-of-head-computed-member",
    "for-of-head-const",
    "for-of-head-identifier",
    "for-of-head-let",
    "for-of-head-static-member",
    "for-of-head-var",
    "for-of-iterator-method-receiver",
    "for-of-labeled-close-order",
    "for-of-natural-exhaustion",
    "for-of-next-error-no-close",
    "for-of-next-receiver",
    "for-of-next-retained",
    "for-of-null-acquisition",
    "for-of-return-close",
    "for-of-step-order",
    "for-of-string-code-point",
    "for-of-undefined-acquisition",
    "for-of-value-error-no-close",
    "iterator-close",
    "iterator-close-boundary",
    "iterator-close-catch-boundary",
    "iterator-close-nested",
    "iterator-close-order",
    "iterator-close-preserves-exception",
    "iterator-done-before-value",
    "iterator-done-skips-value",
    "iterator-method-receiver",
    "iterator-method-double-lookup",
    "iterator-next-retained",
    "iterator-next-getter",
    "iterator-noncallable-method",
    "iterator-nonobject",
    "iterator-result-nonobject",
];

const OBJECT_LEGACY_REQUIRED_COVERAGE: &[&str] = &[
    "define-getter",
    "define-setter",
    "define-validation-order",
    "lookup-key-order",
    "lookup-prototype",
    "lookup-shadow",
    "proto-cycle",
    "proto-getter",
    "proto-invalid-prototype",
    "proto-nonextensible",
    "proto-nullish-order",
    "proto-primitive-receiver",
    "proto-setter",
    "surface-order",
    "symbol-key",
];

const STRING_HTML_REQUIRED_COVERAGE: &[&str] = &[
    "anchor",
    "big",
    "blink",
    "bold",
    "coercion-order",
    "fixed",
    "fontcolor",
    "fontsize",
    "italics",
    "link",
    "nullish-order",
    "quote-escape",
    "small",
    "strike",
    "sub",
    "sup",
    "surface-order",
    "trim-aliases",
];

const PROMISE_CORE_REQUIRED_COVERAGE: &[&str] = &[
    "all-input-order",
    "all-settled-records",
    "any-error-order",
    "capability-callable",
    "capability-executor-once",
    "catch-invoke",
    "combinator-empty",
    "combinator-iterator-close",
    "combinator-original-abrupt",
    "combinator-resolve-order",
    "constructor-new",
    "constructor-sync",
    "executor-callable",
    "finally-generic",
    "finally-handler-metadata",
    "finally-noncallable",
    "finally-order",
    "finally-surface",
    "finally-validation",
    "generic-reject",
    "generic-resolve",
    "new-target-prototype",
    "promise-brand",
    "promise-static-metadata",
    "promise-static-surface",
    "promise-try",
    "promise-try-abrupt",
    "prototype-abrupt",
    "prototype-fallback",
    "prototype-order",
    "reaction-deferred",
    "race-invoke",
    "resolve-identity",
    "resolve-constructor-abrupt",
    "resolve-constructor-get",
    "resolving-functions",
    "resolving-metadata",
    "species-constructor-order",
    "species-fallback",
    "species-getter",
    "species-validation",
    "then-capability",
    "then-brand",
    "thenable-get",
    "with-resolvers",
    "with-resolvers-generic",
];

const GENERATOR_REQUIRED_COVERAGE: &[&str] = &[
    "abrupt-resume-forwarding",
    "call-apply",
    "completed-next",
    "completed-return",
    "completed-throw",
    "delegate-next",
    "delegate-return",
    "delegate-throw",
    "done-before-value",
    "dynamic-generator-call",
    "dynamic-generator-fallback-prototype",
    "dynamic-generator-function",
    "dynamic-generator-metadata",
    "dynamic-generator-new-target",
    "dynamic-generator-source-order",
    "for-of-consumption",
    "function-prototype-chain",
    "generator-method",
    "instance-prototype-chain",
    "iterator-close",
    "iterator-result-identity",
    "iterator-result-validation",
    "lazy-yield-value",
    "missing-return",
    "missing-throw",
    "missing-throw-type-error",
    "next-first-argument",
    "next-resume-value",
    "nonconstructable",
    "parameter-initialization",
    "prestart-return",
    "prestart-throw",
    "reentrancy",
    "return-completion-propagation",
    "return-finally",
    "return-nested-close",
    "throw-catch",
    "throw-done-completion",
    "uncaught-completes",
    "yield",
    "yield-star",
    "yield-star-finally",
    "zero-argument-close",
];

const ASYNC_FUNCTION_REQUIRED_COVERAGE: &[&str] = &[
    "await-fulfill",
    "await-reject",
    "await-always-defers",
    "await-finally",
    "await-thenable",
    "dynamic-async-function",
    "dynamic-new-target",
    "dynamic-prototype-fallback",
    "dynamic-source-order",
    "fifo-job-order",
    "function-prototype-chain",
    "method",
    "nonconstructable",
    "parameter-abrupt-rejection",
    "parameter-initialization",
    "return-assimilation",
    "sync-prefix",
    "throw-rejects",
];

const ASYNC_GENERATOR_REQUIRED_COVERAGE: &[&str] = &[
    "async-iterator-identity",
    "await-fulfill",
    "await-reject",
    "call-deferred",
    "completed-next",
    "completed-return-await",
    "completed-throw",
    "dynamic-async-generator-function",
    "dynamic-new-target",
    "dynamic-prototype-fallback",
    "dynamic-source-order",
    "fifo-request-queue",
    "function-prototype-chain",
    "instance-prototype-chain",
    "invalid-receiver-rejects",
    "method",
    "next-promise",
    "next-resume-value",
    "nonconstructable",
    "parameter-abrupt-throw",
    "parameter-initialization",
    "return-await",
    "return-finally",
    "return-promise-resolve-abrupt",
    "return-reject",
    "throw-catch",
    "uncaught-throw",
    "yield-await-assimilation",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeDifferentialSuite {
    AsyncFunction,
    AsyncGenerator,
    ControlFlow,
    Error,
    FunctionApply,
    FunctionBind,
    Generator,
    Iterator,
    CallSpread,
    ObjectLegacy,
    PromiseCore,
    StringHtml,
}

impl RuntimeDifferentialSuite {
    const fn label(self) -> &'static str {
        match self {
            Self::AsyncFunction => "async-function",
            Self::AsyncGenerator => "async-generator",
            Self::ControlFlow => "control-flow",
            Self::Error => "error",
            Self::FunctionApply => "function-apply",
            Self::FunctionBind => "function-bind",
            Self::Generator => "generator",
            Self::Iterator => "iterator",
            Self::CallSpread => "call-spread",
            Self::ObjectLegacy => "object-legacy",
            Self::PromiseCore => "promise-core",
            Self::StringHtml => "string-html",
        }
    }

    const fn required_coverage(self) -> &'static [&'static str] {
        match self {
            Self::AsyncFunction => ASYNC_FUNCTION_REQUIRED_COVERAGE,
            Self::AsyncGenerator => ASYNC_GENERATOR_REQUIRED_COVERAGE,
            Self::ControlFlow => REQUIRED_COVERAGE,
            Self::Error => ERROR_REQUIRED_COVERAGE,
            Self::FunctionApply => FUNCTION_APPLY_REQUIRED_COVERAGE,
            Self::FunctionBind => FUNCTION_BIND_REQUIRED_COVERAGE,
            Self::Generator => GENERATOR_REQUIRED_COVERAGE,
            Self::Iterator => ITERATOR_REQUIRED_COVERAGE,
            Self::CallSpread => CALL_SPREAD_REQUIRED_COVERAGE,
            Self::ObjectLegacy => OBJECT_LEGACY_REQUIRED_COVERAGE,
            Self::PromiseCore => PROMISE_CORE_REQUIRED_COVERAGE,
            Self::StringHtml => STRING_HTML_REQUIRED_COVERAGE,
        }
    }

    const fn reads_async_result(self) -> bool {
        matches!(self, Self::AsyncFunction | Self::AsyncGenerator)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ControlFlowDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct AsyncFunctionDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct AsyncGeneratorDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ErrorDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct FunctionApplyDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct FunctionBindDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct GeneratorDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct IteratorDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CallSpreadDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ObjectLegacyDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PromiseCoreDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StringHtmlDifferentialOptions {
    pub(crate) oracle: PathBuf,
    pub(crate) corpus: PathBuf,
    pub(crate) timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlFlowCase {
    id: String,
    coverage: Vec<String>,
    body: String,
    expected: Observation,
}

#[derive(Debug, Eq, PartialEq)]
struct ControlFlowCorpus {
    cases: Vec<ControlFlowCase>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Observation {
    Undefined,
    Null,
    Boolean(bool),
    NumberBits(u64),
    String(String),
    Throw { name: String, message: String },
}

#[derive(Debug, Eq, PartialEq)]
enum CandidateObservation {
    JavaScript(Observation),
    HarnessFailure(String),
}

pub(crate) fn run_control_flow_differential(
    options: &ControlFlowDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::ControlFlow,
    )
}

pub(crate) fn run_async_function_differential(
    options: &AsyncFunctionDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::AsyncFunction,
    )
}

pub(crate) fn run_async_generator_differential(
    options: &AsyncGeneratorDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::AsyncGenerator,
    )
}

pub(crate) fn run_error_differential(options: &ErrorDifferentialOptions) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::Error,
    )
}

pub(crate) fn run_function_apply_differential(
    options: &FunctionApplyDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::FunctionApply,
    )
}

pub(crate) fn run_function_bind_differential(
    options: &FunctionBindDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::FunctionBind,
    )
}

pub(crate) fn run_generator_differential(
    options: &GeneratorDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::Generator,
    )
}

pub(crate) fn run_iterator_differential(
    options: &IteratorDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::Iterator,
    )
}

pub(crate) fn run_call_spread_differential(
    options: &CallSpreadDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::CallSpread,
    )
}

pub(crate) fn run_object_legacy_differential(
    options: &ObjectLegacyDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::ObjectLegacy,
    )
}

pub(crate) fn run_promise_core_differential(
    options: &PromiseCoreDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::PromiseCore,
    )
}

pub(crate) fn run_string_html_differential(
    options: &StringHtmlDifferentialOptions,
) -> Result<bool, String> {
    run_runtime_differential(
        &options.oracle,
        &options.corpus,
        options.timeout,
        RuntimeDifferentialSuite::StringHtml,
    )
}

fn run_runtime_differential(
    oracle_path: &Path,
    corpus_path: &Path,
    timeout: Duration,
    suite: RuntimeDifferentialSuite,
) -> Result<bool, String> {
    validate_options(timeout, suite)?;
    validate_executable(oracle_path, &format!("{} oracle", suite.label()))?;
    validate_oracle_release(oracle_path, timeout)?;
    let corpus = load_corpus(corpus_path, suite)?;
    let oracle = observe_oracle(oracle_path, &corpus.cases, timeout, suite)?;

    for ((case, observed), index) in corpus.cases.iter().zip(&oracle).zip(0_usize..) {
        if observed != &case.expected {
            return Err(format!(
                "{} oracle result for case {index} `{}` disagrees with the pinned manifest:\n  manifest={}\n  oracle={}",
                suite.label(),
                case.id,
                format_observation(&case.expected),
                format_observation(observed)
            ));
        }
    }

    let candidate = observe_candidate(&corpus.cases, timeout, suite)?;
    let mut mismatch_count = 0_usize;
    let mut reported = Vec::new();
    for (((case, expected), actual), index) in corpus
        .cases
        .iter()
        .zip(&oracle)
        .zip(&candidate)
        .zip(0_usize..)
    {
        if matches!(actual, CandidateObservation::JavaScript(actual) if actual == expected) {
            continue;
        }
        mismatch_count = mismatch_count
            .checked_add(1)
            .ok_or_else(|| format!("{} mismatch count overflowed", suite.label()))?;
        if reported.len() < MAX_REPORTED_MISMATCHES {
            reported.push(format_mismatch(index, case, expected, actual, suite));
        }
    }

    let passed = corpus
        .cases
        .len()
        .checked_sub(mismatch_count)
        .ok_or_else(|| format!("{} pass count underflowed", suite.label()))?;
    if mismatch_count == 0 {
        println!(
            "{} differential: {passed}/{} cases match ({} required feature tags)",
            suite.label(),
            corpus.cases.len(),
            suite.required_coverage().len()
        );
        return Ok(true);
    }

    for mismatch in reported {
        eprintln!("{mismatch}");
    }
    if mismatch_count > MAX_REPORTED_MISMATCHES {
        eprintln!(
            "{} differential: omitted {} additional mismatch(es)",
            suite.label(),
            mismatch_count - MAX_REPORTED_MISMATCHES
        );
    }
    eprintln!(
        "{} differential: {passed}/{} cases match; {mismatch_count} mismatch(es)",
        suite.label(),
        corpus.cases.len()
    );
    Ok(false)
}

fn validate_options(timeout: Duration, suite: RuntimeDifferentialSuite) -> Result<(), String> {
    let milliseconds = timeout.as_millis();
    if milliseconds == 0 || milliseconds > u128::from(MAX_CONTROL_FLOW_TIMEOUT_MS) {
        return Err(format!(
            "{} timeout must be between 1 and {MAX_CONTROL_FLOW_TIMEOUT_MS} milliseconds",
            suite.label()
        ));
    }
    Ok(())
}

fn load_corpus(path: &Path, suite: RuntimeDifferentialSuite) -> Result<ControlFlowCorpus, String> {
    let label = suite.label();
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {label} corpus {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "{label} corpus {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(format!(
            "{label} corpus {} contains {} bytes; the limit is {MAX_MANIFEST_BYTES}",
            path.display(),
            metadata.len()
        ));
    }

    let requested = MAX_MANIFEST_BYTES
        .checked_add(1)
        .ok_or_else(|| format!("{label} manifest read limit overflowed"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(requested)
        .map_err(|_| format!("cannot reserve {requested} {label} manifest bytes"))?;
    File::open(path)
        .map_err(|error| format!("cannot open {label} corpus {}: {error}", path.display()))?
        .take(
            u64::try_from(requested)
                .map_err(|_| format!("{label} manifest read limit does not fit u64"))?,
        )
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label} corpus {}: {error}", path.display()))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{label} corpus {} grew beyond the {MAX_MANIFEST_BYTES}-byte limit while reading",
            path.display()
        ));
    }
    parse_corpus_for_suite(&bytes, &path.display().to_string(), suite)
}

#[cfg(test)]
fn parse_corpus(bytes: &[u8], location: &str) -> Result<ControlFlowCorpus, String> {
    parse_corpus_for_suite(bytes, location, RuntimeDifferentialSuite::ControlFlow)
}

fn parse_corpus_for_suite(
    bytes: &[u8],
    location: &str,
    suite: RuntimeDifferentialSuite,
) -> Result<ControlFlowCorpus, String> {
    let suite_label = suite.label();
    let corpus_label = format!("{suite_label} corpus {location}");
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid {corpus_label}: {error}"))?;
    let root = value
        .as_object()
        .ok_or_else(|| format!("{corpus_label} must be a JSON object"))?;
    require_exact_keys(root, &["cases", "quickjs_release", "schema"], &corpus_label)?;
    let schema = required_u64(root, "schema", &corpus_label)?;
    if schema != MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "{corpus_label} schema is {schema}; expected {MANIFEST_SCHEMA_VERSION}"
        ));
    }
    let release = required_string(root, "quickjs_release", &corpus_label)?;
    if release != EXPECTED_MANIFEST_RELEASE {
        return Err(format!(
            "{corpus_label} release is `{release}`; expected `{EXPECTED_MANIFEST_RELEASE}`"
        ));
    }
    let cases = root
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{corpus_label} field `cases` must be an array"))?;
    if cases.is_empty() {
        return Err(format!("{corpus_label} field `cases` must not be empty"));
    }
    if cases.len() > MAX_CASES {
        return Err(format!(
            "{corpus_label} contains {} cases; the limit is {MAX_CASES}",
            cases.len()
        ));
    }

    let mut parsed = Vec::new();
    parsed
        .try_reserve_exact(cases.len())
        .map_err(|_| format!("cannot reserve {} {suite_label} cases", cases.len()))?;
    let mut ids = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for (index, value) in cases.iter().enumerate() {
        let case = parse_case(value, location, index, suite)?;
        if !ids.insert(case.id.clone()) {
            return Err(format!("{corpus_label} repeats case id `{}`", case.id));
        }
        covered.extend(case.coverage.iter().cloned());
        parsed.push(case);
    }

    let missing = suite
        .required_coverage()
        .iter()
        .copied()
        .filter(|feature| !covered.contains(*feature))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "{corpus_label} is missing required coverage {missing:?}"
        ));
    }
    Ok(ControlFlowCorpus { cases: parsed })
}

fn parse_case(
    value: &Value,
    location: &str,
    index: usize,
    suite: RuntimeDifferentialSuite,
) -> Result<ControlFlowCase, String> {
    let label = format!("{} corpus {location} case {index}", suite.label());
    let object = value
        .as_object()
        .ok_or_else(|| format!("{label} must be a JSON object"))?;
    require_exact_keys(object, &["body", "covers", "expect", "id"], &label)?;

    let id = required_string_with_label(object, "id", &label)?;
    if id.is_empty()
        || id.len() > MAX_CASE_ID_BYTES
        || !id.bytes().enumerate().all(|(offset, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (byte == b'-' && offset != 0 && offset + 1 != id.len())
        })
    {
        return Err(format!(
            "{label} field `id` must be 1..={MAX_CASE_ID_BYTES} bytes of lowercase ASCII letters, digits, and interior hyphens"
        ));
    }

    let coverage_values = object
        .get("covers")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} field `covers` must be an array"))?;
    if coverage_values.is_empty() || coverage_values.len() > suite.required_coverage().len() {
        return Err(format!(
            "{label} field `covers` must contain 1..={} feature tags",
            suite.required_coverage().len()
        ));
    }
    let allowed = suite
        .required_coverage()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut coverage = Vec::new();
    coverage
        .try_reserve_exact(coverage_values.len())
        .map_err(|_| format!("cannot reserve coverage tags for {label}"))?;
    let mut unique_coverage = BTreeSet::new();
    for (coverage_index, value) in coverage_values.iter().enumerate() {
        let feature = value
            .as_str()
            .ok_or_else(|| format!("{label} field `covers[{coverage_index}]` must be a string"))?;
        if !allowed.contains(feature) {
            return Err(format!(
                "{label} field `covers[{coverage_index}]` names unknown feature `{feature}`"
            ));
        }
        if !unique_coverage.insert(feature) {
            return Err(format!("{label} repeats coverage feature `{feature}`"));
        }
        coverage.push(feature.to_owned());
    }

    let body = required_string_with_label(object, "body", &label)?;
    validate_candidate_body(body, &format!("{label} field `body`"))?;
    if suite == RuntimeDifferentialSuite::Error
        && (body.contains("async") || body.contains("await"))
    {
        return Err(format!(
            "{label} field `body` contains forbidden asynchronous syntax text"
        ));
    }

    let expected = parse_observation(
        object
            .get("expect")
            .ok_or_else(|| format!("{label} is missing field `expect`"))?,
        &format!("{label} field `expect`"),
    )?;
    Ok(ControlFlowCase {
        id: id.to_owned(),
        coverage,
        body: body.to_owned(),
        expected,
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
        "{location} fields are {actual:?}; expected {expected:?}"
    ))
}

fn required_u64(object: &Map<String, Value>, field: &str, label: &str) -> Result<u64, String> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label} field `{field}` must be an unsigned integer"))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<&'a str, String> {
    required_string_with_label(object, field, label)
}

fn required_string_with_label<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} field `{field}` must be a string"))
}

fn parse_observation(value: &Value, location: &str) -> Result<Observation, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{location} must be a JSON object"))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{location} field `kind` must be a string"))?;
    match kind {
        "undefined" => {
            require_exact_keys(object, &["kind"], location)?;
            Ok(Observation::Undefined)
        }
        "null" => {
            require_exact_keys(object, &["kind"], location)?;
            Ok(Observation::Null)
        }
        "boolean" => {
            require_exact_keys(object, &["kind", "value"], location)?;
            let value = object
                .get("value")
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("{location} field `value` must be a Boolean"))?;
            Ok(Observation::Boolean(value))
        }
        "number" => {
            require_exact_keys(object, &["bits", "kind"], location)?;
            let bits = object
                .get("bits")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{location} field `bits` must be a string"))?;
            parse_number_bits(bits, location).map(Observation::NumberBits)
        }
        "string" => {
            require_exact_keys(object, &["kind", "value"], location)?;
            let value = bounded_ascii_string(
                object.get("value"),
                location,
                "value",
                MAX_EXPECTED_STRING_BYTES,
            )?;
            Ok(Observation::String(value.to_owned()))
        }
        "throw" => {
            require_exact_keys(object, &["kind", "message", "name"], location)?;
            let name = bounded_ascii_string(
                object.get("name"),
                location,
                "name",
                MAX_EXPECTED_ERROR_NAME_BYTES,
            )?;
            let message = bounded_ascii_string(
                object.get("message"),
                location,
                "message",
                MAX_EXPECTED_ERROR_MESSAGE_BYTES,
            )?;
            Ok(Observation::Throw {
                name: name.to_owned(),
                message: message.to_owned(),
            })
        }
        unknown => Err(format!(
            "{location} field `kind` has unsupported value `{unknown}`"
        )),
    }
}

fn bounded_ascii_string<'a>(
    value: Option<&'a Value>,
    location: &str,
    field: &str,
    maximum: usize,
) -> Result<&'a str, String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{location} field `{field}` must be a string"))?;
    if value.len() > maximum {
        return Err(format!(
            "{location} field `{field}` contains {} bytes; the limit is {maximum}",
            value.len()
        ));
    }
    if !value.is_ascii() || value.bytes().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return Err(format!(
            "{location} field `{field}` must be one line of ASCII text"
        ));
    }
    Ok(value)
}

fn parse_number_bits(value: &str, location: &str) -> Result<u64, String> {
    let Some(digits) = value.strip_prefix("0x") else {
        return Err(format!(
            "{location} field `bits` must use exactly 16 lowercase hexadecimal digits after `0x`"
        ));
    };
    if digits.len() != 16
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{location} field `bits` must use exactly 16 lowercase hexadecimal digits after `0x`"
        ));
    }
    u64::from_str_radix(digits, 16)
        .map_err(|error| format!("{location} field `bits` is invalid: {error}"))
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
            "runtime differential oracle {} could not report its version: status={:?}",
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
        "runtime differential oracle {} is not the pinned release; expected banner `{EXPECTED_ORACLE_BANNER}`",
        executable.display()
    ))
}

fn observe_oracle(
    executable: &Path,
    cases: &[ControlFlowCase],
    timeout: Duration,
    suite: RuntimeDifferentialSuite,
) -> Result<Vec<Observation>, String> {
    if cases.is_empty() || cases.len() > MAX_CASES {
        return Err(format!(
            "runtime differential oracle received {} cases; expected 1..={MAX_CASES}",
            cases.len()
        ));
    }
    let mut observations = Vec::new();
    observations.try_reserve_exact(cases.len()).map_err(|_| {
        format!(
            "cannot reserve {} runtime differential oracle results",
            cases.len()
        )
    })?;
    for (index, case) in cases.iter().enumerate() {
        let observation =
            observe_oracle_case(executable, case, timeout, suite).map_err(|error| {
                format!(
                    "runtime differential oracle case {index} `{}` failed: {error}",
                    case.id
                )
            })?;
        observations.push(observation);
    }
    Ok(observations)
}

fn observe_oracle_case(
    executable: &Path,
    case: &ControlFlowCase,
    timeout: Duration,
    suite: RuntimeDifferentialSuite,
) -> Result<Observation, String> {
    let source = build_oracle_source(case, suite)?;
    let temporary = TempOracleScript::create()?;
    let result = (|| {
        temporary.write_source(&source)?;
        let arguments = oracle_case_arguments(temporary.input_path());
        let output = run_program_with_arguments_bounded(
            executable,
            &arguments,
            timeout,
            MAX_ORACLE_CASE_STREAM_BYTES,
        )?;
        classify_oracle_output(executable, 1, &output)?
            .into_iter()
            .next()
            .ok_or_else(|| "runtime differential oracle returned no observation".to_owned())
    })();
    let cleanup = temporary.cleanup();
    match (result, cleanup) {
        (Ok(observation), Ok(())) => Ok(observation),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!(
            "{error}; additionally, temporary cleanup failed: {cleanup_error}"
        )),
    }
}

fn oracle_case_arguments(input: &Path) -> [&OsStr; 5] {
    [
        OsStr::new("--memory-limit"),
        OsStr::new(ORACLE_MEMORY_LIMIT_BYTES),
        OsStr::new("--stack-size"),
        OsStr::new(ORACLE_STACK_SIZE_BYTES),
        input.as_os_str(),
    ]
}

fn build_oracle_source(
    case: &ControlFlowCase,
    suite: RuntimeDifferentialSuite,
) -> Result<String, String> {
    let mut source = String::from(
        "(function(){\
         const __Function=Function,__print=print,__json=JSON.stringify,__String=String;\
         const __apply=Reflect.apply,__setFloat64=DataView.prototype.setFloat64;\
         const __getUint32=DataView.prototype.getUint32,__numberToString=Number.prototype.toString;\
         const __slice=String.prototype.slice,__buffer=new ArrayBuffer(8),__view=new DataView(__buffer);\
         function __hex32(value){return __apply(__slice,\"00000000\"+__apply(__numberToString,value,[16]),[-8]);}\
         function __normal(value){\
           if(value===undefined)return {kind:\"undefined\"};\
           if(value===null)return {kind:\"null\"};\
           const type=typeof value;\
           if(type===\"boolean\")return {kind:\"boolean\",value:value};\
           if(type===\"number\"){__apply(__setFloat64,__view,[0,value]);return {kind:\"number\",bits:\"0x\"+__hex32(__apply(__getUint32,__view,[0]))+__hex32(__apply(__getUint32,__view,[4]))};}\
           if(type===\"string\")return {kind:\"string\",value:value};\
           throw new TypeError(\"unsupported result type: \"+type);\
         }\
         function __run(index,body){let value,result;try{value=__Function(body)();}catch(error){result={kind:\"throw\",name:__String(error&&error.name),message:__String(error&&error.message)};__print(index+\"\\t\"+__json(result));return;}result=__normal(value);__print(index+\"\\t\"+__json(result));}\
         function __runAsync(index,body){let value,result;try{value=__Function(body)();}catch(error){result={kind:\"throw\",name:__String(error&&error.name),message:__String(error&&error.message)};__print(index+\"\\t\"+__json(result));return;}Promise.resolve(value.done).then(function(){try{result=__normal(value.result);}catch(error){result={kind:\"throw\",name:__String(error&&error.name),message:__String(error&&error.message)};}__print(index+\"\\t\"+__json(result));},function(error){result={kind:\"throw\",name:__String(error&&error.name),message:__String(error&&error.message)};__print(index+\"\\t\"+__json(result));});}\
         const __scopeProbe=__Function(\"return typeof __Function+\\\":\\\"+typeof __run;\")();\
         if(__scopeProbe!==\"undefined:undefined\")throw new Error(\"oracle harness lexical scope leaked\");\n",
    );
    let body = js_string_literal(&case.body)?;
    let runner = if suite.reads_async_result() {
        "__runAsync"
    } else {
        "__run"
    };
    writeln!(source, "{runner}(0,{body});}})();")
        .map_err(|_| "cannot write generated runtime differential oracle source".to_owned())?;
    if source.len() > MAX_GENERATED_ORACLE_SOURCE_BYTES {
        return Err(format!(
            "generated runtime differential oracle source contains {} bytes; the limit is {MAX_GENERATED_ORACLE_SOURCE_BYTES}",
            source.len()
        ));
    }
    Ok(source)
}

fn js_string_literal(value: &str) -> Result<String, String> {
    let literal = serde_json::to_string(value)
        .map_err(|error| format!("cannot encode runtime differential body for oracle: {error}"))?;
    Ok(literal
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

struct TempOracleScript {
    directory: PathBuf,
    input: PathBuf,
    cleaned: bool,
}

impl TempOracleScript {
    fn create() -> Result<Self, String> {
        let root = env::temp_dir();
        for _ in 0..MAX_TEMP_DIRECTORY_ATTEMPTS {
            let counter = TEMP_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let directory = root.join(format!(
                "quickjs-runtime-differential-qjs-{}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        input: directory.join("case.js"),
                        directory,
                        cleaned: false,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "cannot create runtime differential oracle temporary directory {}: {error}",
                        directory.display()
                    ));
                }
            }
        }
        Err(format!(
            "cannot create a unique runtime differential oracle temporary directory after {MAX_TEMP_DIRECTORY_ATTEMPTS} attempts"
        ))
    }

    fn input_path(&self) -> &Path {
        &self.input
    }

    fn write_source(&self, source: &str) -> Result<(), String> {
        if source.len() > MAX_GENERATED_ORACLE_SOURCE_BYTES {
            return Err(format!(
                "generated runtime differential oracle source contains {} bytes; the limit is {MAX_GENERATED_ORACLE_SOURCE_BYTES}",
                source.len()
            ));
        }
        let mut input = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.input)
            .map_err(|error| {
                format!(
                    "cannot create runtime differential oracle input {}: {error}",
                    self.input.display()
                )
            })?;
        input.write_all(source.as_bytes()).map_err(|error| {
            format!(
                "cannot write runtime differential oracle input {}: {error}",
                self.input.display()
            )
        })
    }

    fn cleanup(mut self) -> Result<(), String> {
        let result = cleanup_temp_script(&self.directory, &self.input);
        self.cleaned = result.is_ok();
        result
    }
}

impl Drop for TempOracleScript {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = cleanup_temp_script(&self.directory, &self.input);
        }
    }
}

fn cleanup_temp_script(directory: &Path, input: &Path) -> Result<(), String> {
    match fs::remove_file(input) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot delete runtime differential oracle input {}: {error}",
                input.display()
            ));
        }
    }
    match fs::remove_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot delete runtime differential oracle temporary directory {}: {error}",
            directory.display()
        )),
    }
}

fn classify_oracle_output(
    executable: &Path,
    expected_count: usize,
    output: &ProgramOutput,
) -> Result<Vec<Observation>, String> {
    if output.status != Status::Exited(Some(0)) {
        return Err(format!(
            "runtime differential oracle {} failed: status={:?}; stdout={}; stderr={}",
            executable.display(),
            output.status,
            stream_preview(&output.stdout),
            stream_preview(&output.stderr)
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "runtime differential oracle {} wrote unexpected stderr: {}",
            executable.display(),
            stream_preview(&output.stderr)
        ));
    }
    parse_oracle_stdout(&output.stdout, expected_count)
}

fn parse_oracle_stdout(stdout: &[u8], expected_count: usize) -> Result<Vec<Observation>, String> {
    if expected_count == 0 {
        return Err("runtime differential oracle expected count must not be zero".to_owned());
    }
    let text = std::str::from_utf8(stdout)
        .map_err(|error| format!("runtime differential oracle stdout is not UTF-8: {error}"))?;
    let text = text.strip_suffix('\n').ok_or_else(|| {
        "runtime differential oracle stdout must end with exactly one complete result line"
            .to_owned()
    })?;
    if text.ends_with('\n') || text.contains('\r') {
        return Err(
            "runtime differential oracle stdout contains an unexpected blank or CR line".to_owned(),
        );
    }
    let lines = text.split('\n').collect::<Vec<_>>();
    if lines.len() != expected_count {
        return Err(format!(
            "runtime differential oracle emitted {} result lines; expected {expected_count}",
            lines.len()
        ));
    }

    let mut observations = Vec::new();
    observations
        .try_reserve_exact(expected_count)
        .map_err(|_| {
            format!("cannot reserve {expected_count} runtime differential oracle results")
        })?;
    for (expected_index, line) in lines.into_iter().enumerate() {
        if line.len() > MAX_ORACLE_RESULT_LINE_BYTES {
            return Err(format!(
                "runtime differential oracle line {expected_index} contains {} bytes; the limit is {MAX_ORACLE_RESULT_LINE_BYTES}",
                line.len()
            ));
        }
        let (index, encoded) = line.split_once('\t').ok_or_else(|| {
            format!("runtime differential oracle line {expected_index} has no tab separator")
        })?;
        if index != expected_index.to_string() {
            return Err(format!(
                "runtime differential oracle line {expected_index} reports non-canonical index `{index}`"
            ));
        }
        let value: Value = serde_json::from_str(encoded).map_err(|error| {
            format!("runtime differential oracle line {expected_index} has invalid JSON: {error}")
        })?;
        observations.push(parse_observation(
            &value,
            &format!("runtime differential oracle line {expected_index}"),
        )?);
    }
    Ok(observations)
}

fn observe_candidate(
    cases: &[ControlFlowCase],
    timeout: Duration,
    suite: RuntimeDifferentialSuite,
) -> Result<Vec<CandidateObservation>, String> {
    if cases.is_empty() || cases.len() > MAX_CASES {
        return Err(format!(
            "runtime differential candidate received {} cases; expected 1..={MAX_CASES}",
            cases.len()
        ));
    }
    let worker = env::current_exe().map_err(|error| {
        format!("cannot locate the runtime differential candidate worker: {error}")
    })?;
    let mut observations = Vec::new();
    observations
        .try_reserve_exact(cases.len())
        .map_err(|_| format!("cannot reserve {} candidate results", cases.len()))?;
    for case in cases {
        let worker_command = if suite.reads_async_result() {
            ASYNC_FUNCTION_CANDIDATE_WORKER_COMMAND
        } else {
            CANDIDATE_WORKER_COMMAND
        };
        let arguments = [OsStr::new(worker_command)];
        let observation = match run_program_with_arguments_bounded_input(
            &worker,
            &arguments,
            case.body.as_bytes(),
            timeout,
            MAX_CANDIDATE_WORKER_STREAM_BYTES,
        ) {
            Ok(output) => classify_candidate_worker_output(&output, timeout),
            Err(error) => CandidateObservation::HarnessFailure(error),
        };
        observations.push(observation);
    }
    Ok(observations)
}

fn classify_candidate_worker_output(
    output: &ProgramOutput,
    timeout: Duration,
) -> CandidateObservation {
    match output.status {
        Status::TimedOut => CandidateObservation::HarnessFailure(format!(
            "candidate worker timed out after {} milliseconds",
            timeout.as_millis()
        )),
        Status::Exited(Some(0)) if output.stderr.is_empty() => {
            match parse_candidate_worker_stdout(&output.stdout) {
                Ok(observation) => CandidateObservation::JavaScript(observation),
                Err(error) => CandidateObservation::HarnessFailure(error),
            }
        }
        Status::Exited(_) => CandidateObservation::HarnessFailure(format!(
            "candidate worker failed: status={:?}; stdout={}; stderr={}",
            output.status,
            stream_preview(&output.stdout),
            stream_preview(&output.stderr)
        )),
    }
}

fn parse_candidate_worker_stdout(stdout: &[u8]) -> Result<Observation, String> {
    if stdout.len() > MAX_CANDIDATE_WORKER_STREAM_BYTES {
        return Err(format!(
            "candidate worker stdout contains {} bytes; the limit is {MAX_CANDIDATE_WORKER_STREAM_BYTES}",
            stdout.len()
        ));
    }
    let text = std::str::from_utf8(stdout)
        .map_err(|error| format!("candidate worker stdout is not UTF-8: {error}"))?;
    let encoded = text.strip_suffix('\n').ok_or_else(|| {
        "candidate worker stdout must end with exactly one complete result line".to_owned()
    })?;
    if encoded.is_empty() || encoded.contains(['\n', '\r']) {
        return Err(
            "candidate worker stdout must contain exactly one LF-terminated line".to_owned(),
        );
    }
    let value: Value = serde_json::from_str(encoded)
        .map_err(|error| format!("candidate worker stdout has invalid JSON: {error}"))?;
    parse_observation(&value, "candidate worker stdout")
}

pub(crate) fn run_control_flow_candidate_worker(read_async_result: bool) -> Result<(), String> {
    let body = read_candidate_worker_body(std::io::stdin().lock())?;
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        observe_candidate_body(&body, read_async_result)
    }));
    let observation = match attempt {
        Ok(Ok(observation)) => observation,
        Ok(Err(error)) => return Err(truncate(&error)),
        Err(payload) => {
            return Err(format!("candidate panicked: {}", panic_payload(&payload)));
        }
    };
    let encoded = encode_observation(&observation)?;
    let encoded_len = encoded
        .len()
        .checked_add(1)
        .ok_or_else(|| "candidate worker result length overflowed".to_owned())?;
    if encoded_len > MAX_CANDIDATE_WORKER_STREAM_BYTES {
        return Err(format!(
            "candidate worker result contains {encoded_len} bytes; the limit is {MAX_CANDIDATE_WORKER_STREAM_BYTES}"
        ));
    }
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(encoded.as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|error| format!("cannot write candidate worker result: {error}"))
}

fn read_candidate_worker_body(input: impl Read) -> Result<String, String> {
    let requested = MAX_BODY_BYTES
        .checked_add(1)
        .ok_or_else(|| "candidate worker body read limit overflowed".to_owned())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(requested)
        .map_err(|_| format!("cannot reserve {requested} candidate worker body bytes"))?;
    input
        .take(
            u64::try_from(requested)
                .map_err(|_| "candidate worker body read limit does not fit u64".to_owned())?,
        )
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read candidate worker body: {error}"))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(format!(
            "candidate worker body contains {} bytes; the limit is {MAX_BODY_BYTES}",
            bytes.len()
        ));
    }
    let body = String::from_utf8(bytes)
        .map_err(|error| format!("candidate worker body is not UTF-8: {error}"))?;
    validate_candidate_body(&body, "candidate worker body")?;
    Ok(body)
}

fn validate_candidate_body(body: &str, label: &str) -> Result<(), String> {
    if body.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "{label} contains {} bytes; the limit is {MAX_BODY_BYTES}",
            body.len()
        ));
    }
    if body.contains("eval") || body.contains("\\u") {
        return Err(format!(
            "{label} contains forbidden eval text or a Unicode identifier escape"
        ));
    }
    Ok(())
}

fn encode_observation(observation: &Observation) -> Result<String, String> {
    let value = match observation {
        Observation::Undefined => serde_json::json!({"kind": "undefined"}),
        Observation::Null => serde_json::json!({"kind": "null"}),
        Observation::Boolean(value) => {
            serde_json::json!({"kind": "boolean", "value": value})
        }
        Observation::NumberBits(bits) => {
            serde_json::json!({"kind": "number", "bits": format!("0x{bits:016x}")})
        }
        Observation::String(value) => serde_json::json!({"kind": "string", "value": value}),
        Observation::Throw { name, message } => {
            serde_json::json!({"kind": "throw", "name": name, "message": message})
        }
    };
    serde_json::to_string(&value)
        .map_err(|error| format!("cannot encode candidate worker result: {error}"))
}

fn observe_candidate_body(body: &str, read_async_result: bool) -> Result<Observation, String> {
    validate_candidate_body(body, "candidate body")?;
    let runtime_limits = RuntimeLimits::default()
        .with_max_realms(1)
        .with_max_installed_code(16)
        .with_max_installed_templates(512)
        .with_max_installed_atoms(4_096)
        .with_max_installed_constants(4_096)
        .with_max_heap_functions(512)
        .with_max_heap_objects(512)
        .with_max_object_properties(4_096)
        .with_max_for_in_entries(8_192)
        .with_max_binding_cells(4_096)
        .with_max_realm_global_bindings(1_024)
        .with_max_public_roots(4_096)
        .with_max_active_frames(128)
        .with_max_active_frame_values(131_072);
    let mut runtime = Runtime::try_new(runtime_limits)
        .map_err(|error| format!("cannot create candidate runtime: {error}"))?;
    let realm = runtime
        .create_realm()
        .map_err(|error| format!("cannot create candidate realm: {error}"))?;
    let mut context = runtime
        .context(&realm)
        .map_err(|error| format!("cannot create candidate context: {error}"))?;

    let execution = ExecutionLimits::default()
        .with_instruction_fuel(CANDIDATE_INSTRUCTION_FUEL)
        .with_dynamic_compilations(8)
        .with_dynamic_source_code_units(CANDIDATE_SOURCE_BYTES as u64);
    let limits = DynamicFunctionLimits::default()
        .with_frontend(
            FrontendLimits::new(CANDIDATE_SOURCE_BYTES)
                .with_max_dynamic_function_fragments(16)
                .with_max_dynamic_function_origin_bytes(128),
        )
        .with_execution(execution);
    let parameters = [];
    let completion = construct_dynamic_function(
        &mut context,
        DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new(body),
        ),
        limits,
    )
    .map_err(|error| format!("dynamic Function construction failed: {error}"))?;
    let function = completion
        .into_value()
        .into_function()
        .map_err(|error| format!("dynamic Function facade returned a non-function: {error}"))?;
    let completion = call_with_dynamic_function_support(&mut context, &function, &[], limits);
    let completion = match completion {
        Ok(state) if read_async_result => read_async_candidate_result(&mut context, state, limits)?,
        other => other,
    };
    match completion {
        Ok(value) => normalize_candidate_value(&value),
        Err(ExecutionError::Exception(exception)) => {
            if let Some(kind) = exception.kind() {
                let name = exception_name(kind);
                validate_runtime_ascii(
                    name,
                    "candidate exception name",
                    MAX_EXPECTED_ERROR_NAME_BYTES,
                )?;
                let message = exception
                    .message()
                    .ok_or_else(|| "candidate engine exception has no message".to_owned())?;
                let message = decode_bounded_candidate_ascii(
                    message,
                    "candidate exception message",
                    MAX_EXPECTED_ERROR_MESSAGE_BYTES,
                )?;
                return Ok(Observation::Throw {
                    name: name.to_owned(),
                    message,
                });
            }
            let thrown = exception
                .thrown_value()
                .ok_or_else(|| "candidate explicit throw has no JavaScript value".to_owned())?
                .clone();
            normalize_candidate_thrown_value(&mut context, &thrown, limits)
        }
        Err(error) => Err(format!("candidate execution failed: {error}")),
    }
}

fn read_async_candidate_result(
    context: &mut quickjs_runtime::Context<'_>,
    state: JsValue,
    limits: DynamicFunctionLimits,
) -> Result<Result<JsValue, ExecutionError>, String> {
    let parameters = [SourceFragment::new("state")];
    let reader = construct_dynamic_function(
        context,
        DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new("return state.result;"),
        ),
        limits,
    )
    .map_err(|error| format!("async result reader construction failed: {error}"))?
    .into_value()
    .into_function()
    .map_err(|error| format!("async result reader is not a function: {error}"))?;
    Ok(call_with_dynamic_function_support(
        context,
        &reader,
        &[state],
        limits,
    ))
}

/// Observes an explicit JavaScript throw through ordinary property access, as
/// the oracle harness does. Keeping this in JavaScript is important: the host
/// must not infer an Error family from prototype identity or bypass an
/// observable `name`/`message` getter.
fn normalize_candidate_thrown_value(
    context: &mut quickjs_runtime::Context<'_>,
    thrown: &JsValue,
    limits: DynamicFunctionLimits,
) -> Result<Observation, String> {
    let parameters = [SourceFragment::new("value"), SourceFragment::new("key")];
    let observer_limits = limits.with_frontend(
        FrontendLimits::new(CANDIDATE_SOURCE_BYTES)
            .with_max_dynamic_function_fragments(3)
            .with_max_dynamic_function_origin_bytes(128),
    );
    let completion = construct_dynamic_function(
        context,
        DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new("return value && value[key];"),
        ),
        observer_limits,
    )
    .map_err(|error| format!("cannot construct candidate throw observer: {error}"))?;
    let observer = completion
        .into_value()
        .into_function()
        .map_err(|error| format!("candidate throw observer is not a function: {error}"))?;
    let name_key = context.string(
        JsString::from_utf8("name")
            .map_err(|error| format!("cannot construct candidate exception name key: {error}"))?,
    );
    let message_key =
        context.string(JsString::from_utf8("message").map_err(|error| {
            format!("cannot construct candidate exception message key: {error}")
        })?);
    let name = call_with_dynamic_function_support(
        context,
        &observer,
        &[thrown.clone(), name_key],
        observer_limits,
    )
    .map_err(|error| format!("candidate exception name observation failed: {error}"))?;
    let message = call_with_dynamic_function_support(
        context,
        &observer,
        &[thrown.clone(), message_key],
        observer_limits,
    )
    .map_err(|error| format!("candidate exception message observation failed: {error}"))?;
    let name = name
        .as_string()
        .map_err(|error| format!("cannot inspect candidate exception name: {error}"))?
        .ok_or_else(|| "candidate exception name is not a String".to_owned())?;
    let message = message
        .as_string()
        .map_err(|error| format!("cannot inspect candidate exception message: {error}"))?
        .ok_or_else(|| "candidate exception message is not a String".to_owned())?;
    Ok(Observation::Throw {
        name: decode_bounded_candidate_ascii(
            name,
            "candidate exception name",
            MAX_EXPECTED_ERROR_NAME_BYTES,
        )?,
        message: decode_bounded_candidate_ascii(
            message,
            "candidate exception message",
            MAX_EXPECTED_ERROR_MESSAGE_BYTES,
        )?,
    })
}

fn normalize_candidate_value(value: &JsValue) -> Result<Observation, String> {
    match value
        .kind()
        .map_err(|error| format!("cannot inspect candidate result: {error}"))?
    {
        ValueKind::Undefined => Ok(Observation::Undefined),
        ValueKind::Null => Ok(Observation::Null),
        ValueKind::Boolean => value
            .as_boolean()
            .map_err(|error| format!("cannot inspect candidate Boolean: {error}"))?
            .map(Observation::Boolean)
            .ok_or_else(|| "candidate Boolean kind has no Boolean payload".to_owned()),
        ValueKind::Number => value
            .as_number()
            .map_err(|error| format!("cannot inspect candidate Number: {error}"))?
            .map(|number| Observation::NumberBits(number.as_f64().to_bits()))
            .ok_or_else(|| "candidate Number kind has no Number payload".to_owned()),
        ValueKind::String => {
            let string = value
                .as_string()
                .map_err(|error| format!("cannot inspect candidate String: {error}"))?
                .ok_or_else(|| "candidate String kind has no String payload".to_owned())?;
            decode_bounded_candidate_ascii(
                string,
                "candidate String result",
                MAX_EXPECTED_STRING_BYTES,
            )
            .map(Observation::String)
        }
        // A `BigInt` has no primitive observation form in this corpus: the
        // fixtures compare printed output, and a `BigInt` must be rendered
        // explicitly with `String(...)` so the expectation is unambiguous.
        kind
        @ (ValueKind::BigInt | ValueKind::Symbol | ValueKind::Function | ValueKind::Object) => {
            Err(format!(
                "candidate returned unsupported result kind {kind}; the runtime differential corpus must use primitive observations"
            ))
        }
    }
}

fn decode_bounded_candidate_ascii(
    value: &JsString,
    role: &str,
    maximum: usize,
) -> Result<String, String> {
    let code_units = usize::try_from(value.len())
        .map_err(|_| format!("{role} length does not fit the host address space"))?;
    if code_units > maximum {
        return Err(format!(
            "{role} contains {code_units} UTF-16 code units; the pre-conversion limit is {maximum}"
        ));
    }
    let decoded = value
        .to_utf8_lossy()
        .map_err(|error| format!("cannot decode {role}: {error}"))?;
    validate_runtime_ascii(&decoded, role, maximum)?;
    Ok(decoded)
}

const fn exception_name(kind: ExceptionKind) -> &'static str {
    match kind {
        ExceptionKind::InternalError => "InternalError",
        ExceptionKind::RangeError => "RangeError",
        ExceptionKind::ReferenceError => "ReferenceError",
        ExceptionKind::SyntaxError => "SyntaxError",
        ExceptionKind::TypeError => "TypeError",
        ExceptionKind::UriError => "URIError",
    }
}

fn validate_runtime_ascii(value: &str, role: &str, maximum: usize) -> Result<(), String> {
    if value.len() > maximum {
        return Err(format!(
            "{role} contains {} bytes; the limit is {maximum}",
            value.len()
        ));
    }
    if !value.is_ascii() || value.bytes().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return Err(format!("{role} must be one line of ASCII text"));
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

fn format_mismatch(
    index: usize,
    case: &ControlFlowCase,
    expected: &Observation,
    actual: &CandidateObservation,
    suite: RuntimeDifferentialSuite,
) -> String {
    let actual = match actual {
        CandidateObservation::JavaScript(observation) => format_observation(observation),
        CandidateObservation::HarnessFailure(error) => {
            format!("<candidate failure: {}>", truncate(error))
        }
    };
    format!(
        "{} mismatch: case {index} `{}` covers {:?}\n  expected={}\n  actual={actual}",
        suite.label(),
        case.id,
        case.coverage,
        format_observation(expected)
    )
}

fn format_observation(observation: &Observation) -> String {
    match observation {
        Observation::Undefined => "undefined".to_owned(),
        Observation::Null => "null".to_owned(),
        Observation::Boolean(value) => format!("Boolean({value})"),
        Observation::NumberBits(bits) => format!("Number(bits=0x{bits:016x})"),
        Observation::String(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"<unprintable String>\"".to_owned())
        }
        Observation::Throw { name, message } => {
            format!("{name}: {}", truncate(message))
        }
    }
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
        CandidateObservation, EXPECTED_MANIFEST_RELEASE, MAX_BODY_BYTES,
        MAX_CANDIDATE_WORKER_STREAM_BYTES, MAX_EXPECTED_ERROR_MESSAGE_BYTES,
        MAX_EXPECTED_ERROR_NAME_BYTES, MAX_EXPECTED_STRING_BYTES, MAX_ORACLE_CASE_STREAM_BYTES,
        MAX_ORACLE_RESULT_LINE_BYTES, ORACLE_MEMORY_LIMIT_BYTES, ORACLE_STACK_SIZE_BYTES,
        Observation, REQUIRED_COVERAGE, RuntimeDifferentialSuite, TempOracleScript,
        build_oracle_source, classify_candidate_worker_output, decode_bounded_candidate_ascii,
        encode_observation, oracle_case_arguments, parse_candidate_worker_stdout, parse_corpus,
        parse_corpus_for_suite, parse_oracle_stdout, read_candidate_worker_body,
    };
    use crate::{ProgramOutput, Status};
    use quickjs_runtime::JsString;
    use serde_json::{Value, json};
    use std::ffi::OsStr;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;
    use std::time::Duration;

    fn complete_manifest() -> Value {
        let cases = REQUIRED_COVERAGE
            .iter()
            .enumerate()
            .map(|(index, feature)| {
                json!({
                    "id": format!("case-{index}"),
                    "covers": [feature],
                    "body": "return \"ok\";",
                    "expect": {"kind": "string", "value": "ok"}
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": 1,
            "quickjs_release": EXPECTED_MANIFEST_RELEASE,
            "cases": cases
        })
    }

    fn complete_function_apply_manifest() -> Value {
        let cases = super::FUNCTION_APPLY_REQUIRED_COVERAGE
            .iter()
            .enumerate()
            .map(|(index, feature)| {
                json!({
                    "id": format!("apply-case-{index}"),
                    "covers": [feature],
                    "body": "return \"ok\";",
                    "expect": {"kind": "string", "value": "ok"}
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": 1,
            "quickjs_release": EXPECTED_MANIFEST_RELEASE,
            "cases": cases
        })
    }

    fn complete_function_bind_manifest() -> Value {
        let cases = super::FUNCTION_BIND_REQUIRED_COVERAGE
            .iter()
            .enumerate()
            .map(|(index, feature)| {
                json!({
                    "id": format!("bind-case-{index}"),
                    "covers": [feature],
                    "body": "return \"ok\";",
                    "expect": {"kind": "string", "value": "ok"}
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": 1,
            "quickjs_release": EXPECTED_MANIFEST_RELEASE,
            "cases": cases
        })
    }

    fn complete_error_manifest() -> Value {
        let cases = super::ERROR_REQUIRED_COVERAGE
            .iter()
            .enumerate()
            .map(|(index, feature)| {
                json!({
                    "id": format!("error-case-{index}"),
                    "covers": [feature],
                    "body": "return \"ok\";",
                    "expect": {"kind": "string", "value": "ok"}
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": 1,
            "quickjs_release": EXPECTED_MANIFEST_RELEASE,
            "cases": cases
        })
    }

    fn complete_iterator_manifest() -> Value {
        let cases = super::ITERATOR_REQUIRED_COVERAGE
            .iter()
            .enumerate()
            .map(|(index, feature)| {
                json!({
                    "id": format!("iterator-case-{index}"),
                    "covers": [feature],
                    "body": "return \"ok\";",
                    "expect": {"kind": "string", "value": "ok"}
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": 1,
            "quickjs_release": EXPECTED_MANIFEST_RELEASE,
            "cases": cases
        })
    }

    fn complete_call_spread_manifest() -> Value {
        let cases = super::CALL_SPREAD_REQUIRED_COVERAGE
            .iter()
            .enumerate()
            .map(|(index, feature)| {
                json!({
                    "id": format!("call-spread-case-{index}"),
                    "covers": [feature],
                    "body": "return \"ok\";",
                    "expect": {"kind": "string", "value": "ok"}
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema": 1,
            "quickjs_release": EXPECTED_MANIFEST_RELEASE,
            "cases": cases
        })
    }

    fn parse(value: &Value) -> Result<super::ControlFlowCorpus, String> {
        parse_corpus(
            &serde_json::to_vec(value).expect("serialize manifest"),
            "test.json",
        )
    }

    #[test]
    fn accepts_a_complete_strict_manifest() {
        let corpus = parse(&complete_manifest()).expect("valid manifest");
        assert_eq!(corpus.cases.len(), REQUIRED_COVERAGE.len());
        assert_eq!(corpus.cases[0].id, "case-0");
        assert_eq!(
            corpus.cases[0].expected,
            Observation::String("ok".to_owned())
        );
    }

    #[test]
    fn accepts_a_complete_function_apply_manifest_with_its_own_coverage_contract() {
        let manifest = complete_function_apply_manifest();
        let corpus = parse_corpus_for_suite(
            &serde_json::to_vec(&manifest).expect("serialize manifest"),
            "function-apply.json",
            RuntimeDifferentialSuite::FunctionApply,
        )
        .expect("valid Function.prototype.apply manifest");
        assert_eq!(
            corpus.cases.len(),
            super::FUNCTION_APPLY_REQUIRED_COVERAGE.len()
        );
        assert_eq!(corpus.cases[0].id, "apply-case-0");
    }

    #[test]
    fn accepts_a_complete_function_bind_manifest_with_its_own_coverage_contract() {
        let manifest = complete_function_bind_manifest();
        let corpus = parse_corpus_for_suite(
            &serde_json::to_vec(&manifest).expect("serialize manifest"),
            "function-bind.json",
            RuntimeDifferentialSuite::FunctionBind,
        )
        .expect("valid Function.prototype.bind manifest");
        assert_eq!(
            corpus.cases.len(),
            super::FUNCTION_BIND_REQUIRED_COVERAGE.len()
        );
        assert_eq!(corpus.cases[0].id, "bind-case-0");
    }

    #[test]
    fn function_bind_manifest_rejects_cross_suite_coverage_tags() {
        let mut manifest = complete_function_bind_manifest();
        manifest["cases"][0]["covers"][0] = Value::String("labeled-break".to_owned());
        assert!(
            parse_corpus_for_suite(
                &serde_json::to_vec(&manifest).expect("serialize manifest"),
                "function-bind.json",
                RuntimeDifferentialSuite::FunctionBind,
            )
            .expect_err("cross-suite coverage tag")
            .contains("unknown feature")
        );
    }

    #[test]
    fn checked_in_function_bind_manifest_satisfies_the_strict_contract() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/function-bind/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in function-bind manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::FunctionBind,
        )
        .expect("checked-in function-bind manifest");
        assert_eq!(corpus.cases.len(), 21);
    }

    #[test]
    fn checked_in_generator_manifest_satisfies_the_strict_contract() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/generator/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in generator manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::Generator,
        )
        .expect("checked-in generator manifest");
        assert_eq!(corpus.cases.len(), 18);
        assert_eq!(super::GENERATOR_REQUIRED_COVERAGE.len(), 43);
    }

    #[test]
    fn checked_in_async_function_manifest_satisfies_the_strict_contract() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/async-function/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in async-function manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::AsyncFunction,
        )
        .expect("checked-in async-function manifest");
        assert_eq!(corpus.cases.len(), 9);
        assert_eq!(super::ASYNC_FUNCTION_REQUIRED_COVERAGE.len(), 18);
    }

    #[test]
    fn checked_in_async_generator_manifest_satisfies_the_strict_contract() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/async-generator/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in async-generator manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::AsyncGenerator,
        )
        .expect("checked-in async-generator manifest");
        assert_eq!(corpus.cases.len(), 12);
        assert_eq!(super::ASYNC_GENERATOR_REQUIRED_COVERAGE.len(), 28);
    }

    #[test]
    fn checked_in_object_legacy_manifest_satisfies_the_strict_contract() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/object-legacy/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in object-legacy manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::ObjectLegacy,
        )
        .expect("checked-in object-legacy manifest");
        assert_eq!(corpus.cases.len(), 15);
        assert_eq!(
            super::OBJECT_LEGACY_REQUIRED_COVERAGE.len(),
            corpus.cases.len()
        );
    }

    #[test]
    fn checked_in_promise_core_manifest_satisfies_the_strict_contract() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/promise-core/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in Promise core manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::PromiseCore,
        )
        .expect("checked-in Promise core manifest");
        assert_eq!(corpus.cases.len(), 29);
    }

    #[test]
    fn checked_in_string_html_manifest_satisfies_the_strict_contract() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/string-html/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in string-html manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::StringHtml,
        )
        .expect("checked-in string-html manifest");
        assert_eq!(corpus.cases.len(), 6);
    }

    #[test]
    fn accepts_a_complete_call_spread_manifest_with_its_own_coverage_contract() {
        let manifest = complete_call_spread_manifest();
        let corpus = parse_corpus_for_suite(
            &serde_json::to_vec(&manifest).expect("serialize manifest"),
            "call-spread.json",
            RuntimeDifferentialSuite::CallSpread,
        )
        .expect("valid call-spread manifest");
        assert_eq!(
            corpus.cases.len(),
            super::CALL_SPREAD_REQUIRED_COVERAGE.len()
        );
        assert_eq!(corpus.cases[0].id, "call-spread-case-0");
    }

    #[test]
    fn call_spread_manifest_rejects_cross_suite_coverage_tags() {
        let mut manifest = complete_call_spread_manifest();
        manifest["cases"][0]["covers"][0] = Value::String("labeled-break".to_owned());
        assert!(
            parse_corpus_for_suite(
                &serde_json::to_vec(&manifest).expect("serialize manifest"),
                "call-spread.json",
                RuntimeDifferentialSuite::CallSpread,
            )
            .expect_err("cross-suite coverage tag")
            .contains("unknown feature")
        );
    }

    #[test]
    fn checked_in_call_spread_manifest_satisfies_the_strict_contract() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/call-spread/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in call-spread manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::CallSpread,
        )
        .expect("checked-in call-spread manifest");
        assert_eq!(corpus.cases.len(), 15);
    }

    #[test]
    fn accepts_a_complete_error_manifest_with_its_own_coverage_contract() {
        let manifest = complete_error_manifest();
        let corpus = parse_corpus_for_suite(
            &serde_json::to_vec(&manifest).expect("serialize manifest"),
            "error.json",
            RuntimeDifferentialSuite::Error,
        )
        .expect("valid Error manifest");
        assert_eq!(corpus.cases.len(), super::ERROR_REQUIRED_COVERAGE.len());
        assert_eq!(corpus.cases[0].id, "error-case-0");
    }

    #[test]
    fn error_manifest_rejects_cross_suite_coverage_and_async_text() {
        let mut manifest = complete_error_manifest();
        manifest["cases"][0]["covers"][0] = Value::String("labeled-break".to_owned());
        assert!(
            parse_corpus_for_suite(
                &serde_json::to_vec(&manifest).expect("serialize manifest"),
                "error.json",
                RuntimeDifferentialSuite::Error,
            )
            .expect_err("cross-suite coverage tag")
            .contains("unknown feature")
        );

        for body in [
            "async function unsupported() {}",
            "return await unsupported;",
        ] {
            let mut manifest = complete_error_manifest();
            manifest["cases"][0]["body"] = Value::String(body.to_owned());
            assert!(
                parse_corpus_for_suite(
                    &serde_json::to_vec(&manifest).expect("serialize manifest"),
                    "error.json",
                    RuntimeDifferentialSuite::Error,
                )
                .expect_err("asynchronous source")
                .contains("forbidden asynchronous syntax text")
            );
        }
    }

    #[test]
    fn checked_in_error_manifest_satisfies_the_strict_contract() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/error/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in Error manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::Error,
        )
        .expect("checked-in Error manifest");
        assert_eq!(super::ERROR_REQUIRED_COVERAGE.len(), 59);
        assert_eq!(corpus.cases.len(), 35);
    }

    #[test]
    fn function_apply_manifest_rejects_control_flow_coverage_tags() {
        let mut manifest = complete_function_apply_manifest();
        manifest["cases"][0]["covers"][0] = Value::String("labeled-break".to_owned());
        assert!(
            parse_corpus_for_suite(
                &serde_json::to_vec(&manifest).expect("serialize manifest"),
                "function-apply.json",
                RuntimeDifferentialSuite::FunctionApply,
            )
            .expect_err("cross-suite coverage tag")
            .contains("unknown feature")
        );
    }

    #[test]
    fn checked_in_function_apply_manifest_satisfies_the_strict_contract() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/function-apply/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in function-apply manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::FunctionApply,
        )
        .expect("checked-in function-apply manifest");
        assert_eq!(corpus.cases.len(), 15);
    }

    #[test]
    fn accepts_a_complete_iterator_manifest_with_its_own_coverage_contract() {
        let manifest = complete_iterator_manifest();
        let corpus = parse_corpus_for_suite(
            &serde_json::to_vec(&manifest).expect("serialize manifest"),
            "iterator.json",
            RuntimeDifferentialSuite::Iterator,
        )
        .expect("valid iterator manifest");
        assert_eq!(corpus.cases.len(), super::ITERATOR_REQUIRED_COVERAGE.len());
        assert_eq!(corpus.cases[0].id, "iterator-case-0");
    }

    #[test]
    fn iterator_manifest_rejects_cross_suite_coverage_tags() {
        let mut manifest = complete_iterator_manifest();
        manifest["cases"][0]["covers"][0] = Value::String("labeled-break".to_owned());
        assert!(
            parse_corpus_for_suite(
                &serde_json::to_vec(&manifest).expect("serialize manifest"),
                "iterator.json",
                RuntimeDifferentialSuite::Iterator,
            )
            .expect_err("cross-suite coverage tag")
            .contains("unknown feature")
        );
    }

    #[test]
    fn checked_in_iterator_manifest_satisfies_the_strict_contract() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/iterator/manifest.json");
        let bytes = fs::read(&path).expect("read checked-in iterator manifest");
        let corpus = parse_corpus_for_suite(
            &bytes,
            &path.display().to_string(),
            RuntimeDifferentialSuite::Iterator,
        )
        .expect("checked-in iterator manifest");
        assert_eq!(super::ITERATOR_REQUIRED_COVERAGE.len(), 51);
        assert_eq!(corpus.cases.len(), 40);
    }

    #[test]
    fn rejects_unknown_manifest_fields() {
        let mut manifest = complete_manifest();
        manifest
            .as_object_mut()
            .expect("root object")
            .insert("unexpected".to_owned(), Value::Bool(true));
        assert!(
            parse(&manifest)
                .expect_err("unknown field")
                .contains("expected")
        );
    }

    #[test]
    fn rejects_missing_required_coverage() {
        let mut manifest = complete_manifest();
        manifest["cases"].as_array_mut().expect("cases").remove(0);
        assert!(
            parse(&manifest)
                .expect_err("missing coverage")
                .contains("missing required coverage")
        );
    }

    #[test]
    fn rejects_duplicate_case_ids() {
        let mut manifest = complete_manifest();
        manifest["cases"][1]["id"] = manifest["cases"][0]["id"].clone();
        assert!(
            parse(&manifest)
                .expect_err("duplicate id")
                .contains("repeats case id")
        );
    }

    #[test]
    fn rejects_eval_and_escaped_identifiers() {
        for body in ["return eval(\"1\");", "return e\\\\u0076al(\"1\");"] {
            let mut manifest = complete_manifest();
            manifest["cases"][0]["body"] = Value::String(body.to_owned());
            assert!(
                parse(&manifest)
                    .expect_err("forbidden source")
                    .contains("forbidden eval text")
            );
        }
    }

    #[test]
    fn oracle_protocol_is_indexed_and_strict() {
        let output = concat!(
            "0\t{\"kind\":\"string\",\"value\":\"ok\"}\n",
            "1\t{\"kind\":\"throw\",\"name\":\"ReferenceError\",\"message\":\"x is not initialized\"}\n"
        );
        assert_eq!(
            parse_oracle_stdout(output.as_bytes(), 2).expect("valid protocol"),
            [
                Observation::String("ok".to_owned()),
                Observation::Throw {
                    name: "ReferenceError".to_owned(),
                    message: "x is not initialized".to_owned()
                }
            ]
        );
        assert!(parse_oracle_stdout(output.as_bytes(), 1).is_err());
        assert!(
            parse_oracle_stdout(b"00\t{\"kind\":\"undefined\"}\n", 1)
                .expect_err("noncanonical index")
                .contains("non-canonical")
        );
    }

    #[test]
    fn oracle_source_iife_isolates_harness_bindings_and_embeds_the_body() {
        let corpus = parse(&complete_manifest()).expect("valid manifest");
        let source = build_oracle_source(&corpus.cases[0], RuntimeDifferentialSuite::ControlFlow)
            .expect("oracle source");
        assert!(source.starts_with("(function(){const __Function=Function"));
        assert!(source.contains("__Function(body)()"));
        assert!(source.contains(
            "const __scopeProbe=__Function(\"return typeof __Function+\\\":\\\"+typeof __run;\")();"
        ));
        assert!(source.ends_with("__run(0,\"return \\\"ok\\\";\");})();\n"));
    }

    #[test]
    fn oracle_case_arguments_are_exact_and_resource_bounded() {
        let input = Path::new("bounded-control-flow-case.js");
        assert_eq!(
            oracle_case_arguments(input),
            [
                OsStr::new("--memory-limit"),
                OsStr::new(ORACLE_MEMORY_LIMIT_BYTES),
                OsStr::new("--stack-size"),
                OsStr::new(ORACLE_STACK_SIZE_BYTES),
                input.as_os_str(),
            ]
        );
        assert_eq!(ORACLE_MEMORY_LIMIT_BYTES, "67108864");
        assert_eq!(ORACLE_STACK_SIZE_BYTES, "1048576");
    }

    #[test]
    fn oracle_result_bound_accepts_worst_case_json_escaping() {
        let string = "\0".repeat(MAX_EXPECTED_STRING_BYTES);
        let encoded = serde_json::to_string(&json!({
            "kind": "string",
            "value": string
        }))
        .expect("encode boundary String observation");
        let line = format!("0\t{encoded}");
        assert!(line.len() <= MAX_ORACLE_RESULT_LINE_BYTES);
        assert_eq!(
            parse_oracle_stdout(format!("{line}\n").as_bytes(), 1).expect("boundary result"),
            [Observation::String(string)]
        );

        let name = "\0".repeat(MAX_EXPECTED_ERROR_NAME_BYTES);
        let message = "\0".repeat(MAX_EXPECTED_ERROR_MESSAGE_BYTES);
        let encoded = serde_json::to_string(&json!({
            "kind": "throw",
            "name": name,
            "message": message
        }))
        .expect("encode boundary throw observation");
        let line = format!("0\t{encoded}");
        assert!(line.len() <= MAX_ORACLE_RESULT_LINE_BYTES);
        assert_eq!(
            MAX_ORACLE_CASE_STREAM_BYTES,
            MAX_ORACLE_RESULT_LINE_BYTES + 1
        );
    }

    #[test]
    fn candidate_ascii_guard_rejects_an_oversized_rope_before_flattening() {
        let left =
            JsString::from_latin1(&vec![b'l'; MAX_EXPECTED_STRING_BYTES]).expect("left leaf");
        let right = JsString::from_latin1(&vec![b'r'; 513]).expect("right leaf");
        let rope = left.concat(&right).expect("oversized rope");
        assert_eq!(
            decode_bounded_candidate_ascii(
                &rope,
                "candidate String result",
                MAX_EXPECTED_STRING_BYTES
            )
            .expect_err("rope exceeds pre-conversion limit"),
            "candidate String result contains 4609 UTF-16 code units; the pre-conversion limit is 4096"
        );
    }

    #[test]
    fn candidate_worker_protocol_round_trips_bounded_observations() {
        let observations = [
            Observation::Undefined,
            Observation::Null,
            Observation::Boolean(true),
            Observation::NumberBits(f64::NEG_INFINITY.to_bits()),
            Observation::String("\0quoted\"".to_owned()),
            Observation::Throw {
                name: "ReferenceError".to_owned(),
                message: "x is not initialized".to_owned(),
            },
        ];
        for observation in observations {
            let encoded = format!(
                "{}\n",
                encode_observation(&observation).expect("encode worker observation")
            );
            assert!(encoded.len() <= MAX_CANDIDATE_WORKER_STREAM_BYTES);
            assert_eq!(
                parse_candidate_worker_stdout(encoded.as_bytes())
                    .expect("parse worker observation"),
                observation
            );
        }
        assert!(
            parse_candidate_worker_stdout(b"{\"kind\":\"undefined\"}\n\n")
                .expect_err("extra line")
                .contains("exactly one")
        );
    }

    #[test]
    fn candidate_worker_timeout_is_a_case_local_harness_failure() {
        assert_eq!(
            classify_candidate_worker_output(
                &ProgramOutput {
                    status: Status::TimedOut,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
                Duration::from_millis(125)
            ),
            CandidateObservation::HarnessFailure(
                "candidate worker timed out after 125 milliseconds".to_owned()
            )
        );
    }

    #[test]
    fn candidate_worker_rejects_the_first_excess_body_byte() {
        let body = vec![b'x'; MAX_BODY_BYTES + 1];
        assert_eq!(
            read_candidate_worker_body(Cursor::new(body)).expect_err("oversized worker body"),
            format!(
                "candidate worker body contains {} bytes; the limit is {MAX_BODY_BYTES}",
                MAX_BODY_BYTES + 1
            )
        );
    }

    #[test]
    fn temporary_oracle_scripts_are_file_backed_and_cleaned() {
        let temporary = TempOracleScript::create().expect("unique temporary directory");
        let directory = temporary.directory.clone();
        let input = temporary.input.clone();
        temporary
            .write_source("print(\"ok\");")
            .expect("bounded oracle source");
        assert_eq!(
            fs::read_to_string(&input).expect("read oracle input"),
            "print(\"ok\");"
        );
        temporary.cleanup().expect("exact temporary cleanup");
        assert!(!input.exists());
        assert!(!directory.exists());
    }
}
