//! The `String.prototype` method surface, pinned to the specification.
//!
//! Every expectation below was produced by the pinned oracle. The transcript
//! was generated with `/private/tmp/quickjs-2026-06-04/qjs` running the same
//! source, for example:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const S="hello";\
//!     console.log(S.charAt(5), S.charCodeAt(5), S.at(-1), S.at(-6));'
//!  NaN o undefined
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! "hello".charAt(0) => "h"     charAt(5) => ""      charAt(-1) => ""
//! "hello".charAt() => "h"      charAt(1.9) => "e"   charAt(NaN) => "h"
//! "hello".charAt(Infinity) => ""
//! "hello".charCodeAt(0) => 104 charCodeAt(5) => NaN charCodeAt(-1) => NaN
//! "a\u{1F600}b".length => 4
//! codePointAt(0) => 97   codePointAt(1) => 128512   codePointAt(2) => 56832
//! codePointAt(3) => 98   codePointAt(4) => undefined
//! "\uD800a".codePointAt(0) => 55296
//! "hello".at(0) => "h"   at(-1) => "o"   at(-5) => "h"
//! "hello".at(-6) => undefined            at(5) => undefined
//! indexOf("l") => 2      indexOf("l",3) => 3        indexOf("") => 0
//! indexOf("",3) => 3     indexOf("",99) => 5        indexOf("z") => -1
//! indexOf("l",-5) => 2   "xundefinedy".indexOf() => 1
//! lastIndexOf("l") => 3  lastIndexOf("l",2) => 2    lastIndexOf("") => 5
//! lastIndexOf("l",NaN) => 3                         lastIndexOf("l",-1) => -1
//! includes("ell") => true          includes("ell",2) => false
//! startsWith("he") => true         startsWith("ell",1) => true
//! endsWith("lo") => true           endsWith("ell",4) => true
//! slice(1) => "ello"     slice(1,3) => "el"   slice(-3) => "llo"
//! slice(-3,-1) => "ll"   slice(3,1) => ""     slice(-99,99) => "hello"
//! substring(1,3) => "el" substring(3,1) => "el"
//! substring(-1,99) => "hello"      substring(NaN,2) => "he"
//! substr(1,2) => "el"    substr(-3,2) => "ll" substr(1) => "ello"
//! substr(1,-1) => ""     substr(-99,2) => "he"
//! concat(" ","world",1,null) => "hello world1null"
//! "ab".repeat(3) => "ababab"       "ab".repeat(0) => ""
//! "ab".repeat(1.9) => "ab"
//! "ab".repeat(-1) !! RangeError: invalid repeat count
//! "ab".repeat(Infinity) !! RangeError: invalid repeat count
//! padStart(8,"xy") => "xyxhello"   padStart(8) => "   hello"
//! padStart(3) => "hello"           padStart(8,"") => "hello"
//! padEnd(8,"xy") => "helloxyx"
//! "  \t\n ab \r\n ".trim() => "ab"
//! "  ab  ".trimStart() => "ab  "   "  ab  ".trimEnd() => "  ab"
//! "\u00a0\ufeff ab \u2028".trim() => "ab"
//! "ab".isWellFormed() => true      "\uD800".isWellFormed() => false
//! "\uD800a".toWellFormed().charCodeAt(0).toString(16) => "fffd"
//! String.prototype.charAt.call(null,0) !! TypeError: null or undefined are forbidden
//! String.prototype.charAt.call(12345,2) => "3"
//! slice(1,undefined) => "ello"     substring(1,undefined) => "ello"
//! substr(1,undefined) => "ello"    lastIndexOf("l",undefined) => 3
//! padStart(8,undefined) => "   hello"
//! "ab".repeat(undefined) => ""     concat(undefined) => "helloundefined"
//! at(undefined) => "h"             charAt(undefined) => "h"
//! conversion order (recv, arg, pos) => "recv,arg,pos"
//! ```

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits,
};

#[derive(Debug)]
struct TestCompileError(String);

impl fmt::Display for TestCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for TestCompileError {}

struct TestCompiler;

impl OrdinaryDynamicFunctionCompiler for TestCompiler {
    fn compile(
        &self,
        source: OrdinaryDynamicFunctionSource,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime String methods>"),
                )
                .map_err(engine_failure)?;
                context
                    .compile_dynamic_function_script(VerificationLimits::default())
                    .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                    .map_err(engine_failure)
            },
        )
        .map_err(engine_failure)?
    }
}

fn engine_failure(error: impl Error + Send + Sync + 'static) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(TestCompileError(error.to_string())),
    }
}

fn dynamic_function(context: &mut Context<'_>, body: &str) -> Function {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
}

/// Evaluates `expression` and renders the result with `String()`.
///
/// Rendering through `String()` keeps `undefined` and `NaN` observable as
/// themselves rather than collapsing them into a projection failure.
fn rendered(expression: &str) -> String {
    evaluate(&format!("return String({expression});"), |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

/// Returns the thrown exception's kind and message.
fn thrown(body: &str) -> (ExceptionKind, String) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        let kind = exception.kind().expect("engine exception kind");
        let message = exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8");
        (kind, message)
    })
}

fn assert_throws(body: &str, kind: ExceptionKind, message: &str) {
    assert_eq!((kind, message.to_owned()), thrown(body), "{body}");
}

/// Asserts a table of `expression => rendered result` pairs.
fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

/// `charAt` answers the empty string outside the range, while `charCodeAt`
/// answers `NaN` and `at` answers `undefined`.
///
/// The three differ only in how they report an out-of-range index, which is why
/// they are pinned together.
#[test]
fn the_index_accessors_differ_only_in_their_out_of_range_answer() {
    assert_all(&[
        ("'hello'.charAt(0)", "h"),
        ("'hello'.charAt(4)", "o"),
        ("'hello'.charAt(5)", ""),
        ("'hello'.charAt(-1)", ""),
        ("'hello'.charAt()", "h"),
        // The index is truncated toward zero, and `NaN` becomes `0`.
        ("'hello'.charAt(1.9)", "e"),
        ("'hello'.charAt(NaN)", "h"),
        ("'hello'.charAt(undefined)", "h"),
        ("'hello'.charAt(Infinity)", ""),
        ("'hello'.charCodeAt(0)", "104"),
        ("'hello'.charCodeAt(5)", "NaN"),
        ("'hello'.charCodeAt(-1)", "NaN"),
        ("'hello'.charCodeAt()", "104"),
        ("'hello'.at(0)", "h"),
        // Only `at` accepts a negative index counting from the end.
        ("'hello'.at(-1)", "o"),
        ("'hello'.at(-5)", "h"),
        ("'hello'.at(-6)", "undefined"),
        ("'hello'.at(5)", "undefined"),
        ("'hello'.at(1.9)", "e"),
        ("'hello'.at(undefined)", "h"),
    ]);
}

/// `codePointAt` combines a surrogate pair but returns a lone surrogate as is.
///
/// Indices stay UTF-16 code-unit indices, so the astral character occupies two
/// of them and `"a\u{1F600}b".length` is `4`.
#[test]
fn code_point_at_combines_only_a_valid_surrogate_pair() {
    assert_all(&[
        ("'a\\u{1F600}b'.length", "4"),
        ("'a\\u{1F600}b'.codePointAt(0)", "97"),
        // The leading surrogate combines with its trailing partner.
        ("'a\\u{1F600}b'.codePointAt(1)", "128512"),
        // The trailing surrogate on its own is returned unchanged.
        ("'a\\u{1F600}b'.codePointAt(2)", "56832"),
        ("'a\\u{1F600}b'.codePointAt(3)", "98"),
        ("'a\\u{1F600}b'.codePointAt(4)", "undefined"),
        // An unpaired leading surrogate is returned as itself.
        ("'\\uD800a'.codePointAt(0)", "55296"),
    ]);
}

/// The searches agree on an empty needle and on a clamped start position.
#[test]
fn the_search_methods_match_the_oracle() {
    assert_all(&[
        ("'hello'.indexOf('l')", "2"),
        ("'hello'.indexOf('l', 3)", "3"),
        ("'hello'.indexOf('z')", "-1"),
        // An empty needle matches at the clamped start position.
        ("'hello'.indexOf('')", "0"),
        ("'hello'.indexOf('', 3)", "3"),
        ("'hello'.indexOf('', 99)", "5"),
        // A negative start clamps to zero rather than counting from the end.
        ("'hello'.indexOf('l', -5)", "2"),
        // An absent needle is converted with `ToString`, giving `"undefined"`.
        ("'xundefinedy'.indexOf()", "1"),
        ("'hello'.lastIndexOf('l')", "3"),
        ("'hello'.lastIndexOf('l', 2)", "2"),
        ("'hello'.lastIndexOf('')", "5"),
        // `NaN` means "search from the end", which is why the position keeps
        // its Number shape instead of truncating to zero.
        ("'hello'.lastIndexOf('l', NaN)", "3"),
        ("'hello'.lastIndexOf('l', undefined)", "3"),
        ("'hello'.lastIndexOf('l', -1)", "-1"),
        ("'hello'.includes('ell')", "true"),
        ("'hello'.includes('ell', 2)", "false"),
        ("'hello'.startsWith('he')", "true"),
        ("'hello'.startsWith('ell', 1)", "true"),
        ("'hello'.endsWith('lo')", "true"),
        ("'hello'.endsWith('ell', 4)", "true"),
    ]);
}

/// `slice`, `substring`, and `substr` resolve their endpoints differently.
///
/// `slice` accepts negative endpoints and yields the empty string when they
/// cross; `substring` clamps and then swaps them; the Annex B `substr` takes a
/// length rather than an end index.
#[test]
fn the_extraction_methods_resolve_endpoints_differently() {
    assert_all(&[
        ("'hello'.slice(1)", "ello"),
        ("'hello'.slice(1, 3)", "el"),
        ("'hello'.slice(-3)", "llo"),
        ("'hello'.slice(-3, -1)", "ll"),
        // Crossed endpoints yield the empty string rather than swapping.
        ("'hello'.slice(3, 1)", ""),
        ("'hello'.slice(-99, 99)", "hello"),
        ("'hello'.slice(1, undefined)", "ello"),
        ("'hello'.substring(1, 3)", "el"),
        // `substring` swaps crossed endpoints instead.
        ("'hello'.substring(3, 1)", "el"),
        ("'hello'.substring(-1, 99)", "hello"),
        ("'hello'.substring(NaN, 2)", "he"),
        ("'hello'.substring(1, undefined)", "ello"),
        ("'hello'.substr(1, 2)", "el"),
        ("'hello'.substr(-3, 2)", "ll"),
        ("'hello'.substr(1)", "ello"),
        // A negative length yields the empty string.
        ("'hello'.substr(1, -1)", ""),
        ("'hello'.substr(-99, 2)", "he"),
        ("'hello'.substr(1, undefined)", "ello"),
    ]);
}

/// `concat` converts every argument with `ToString`, in order.
#[test]
fn concat_converts_every_argument_to_a_string() {
    assert_all(&[
        ("'hello'.concat(' ', 'world', 1, null)", "hello world1null"),
        ("'hello'.concat()", "hello"),
        ("'hello'.concat(undefined)", "helloundefined"),
    ]);
}

/// `repeat` truncates its count and rejects a negative or infinite one.
#[test]
fn repeat_rejects_a_negative_or_infinite_count() {
    assert_all(&[
        ("'ab'.repeat(3)", "ababab"),
        ("'ab'.repeat(0)", ""),
        ("'ab'.repeat(1.9)", "ab"),
        // An absent count truncates to zero rather than throwing.
        ("'ab'.repeat(undefined)", ""),
    ]);
    for source in ["return 'ab'.repeat(-1);", "return 'ab'.repeat(Infinity);"] {
        assert_throws(source, ExceptionKind::RangeError, "invalid repeat count");
    }
}

/// Padding repeats the filler and truncates it to the exact width.
#[test]
fn padding_repeats_and_truncates_its_filler() {
    assert_all(&[
        ("'hello'.padStart(8, 'xy')", "xyxhello"),
        // An absent filler defaults to a single space.
        ("'hello'.padStart(8)", "   hello"),
        ("'hello'.padStart(8, undefined)", "   hello"),
        // A target inside the subject leaves it unchanged.
        ("'hello'.padStart(3)", "hello"),
        // An empty filler cannot pad, so the subject is unchanged.
        ("'hello'.padStart(8, '')", "hello"),
        ("'hello'.padEnd(8, 'xy')", "helloxyx"),
    ]);
}

/// Trimming removes the same whitespace set `StringToNumber` skips.
#[test]
fn trimming_removes_whitespace_and_line_terminators() {
    assert_all(&[
        ("'  \\t\\n ab \\r\\n '.trim()", "ab"),
        ("'  ab  '.trimStart()", "ab  "),
        ("'  ab  '.trimEnd()", "  ab"),
        // The set includes `U+00A0`, `U+FEFF`, and `U+2028`.
        ("'\\u00a0\\ufeff ab \\u2028'.trim()", "ab"),
    ]);
}

/// The well-formed methods detect and replace an unpaired surrogate.
#[test]
fn the_well_formed_methods_handle_unpaired_surrogates() {
    assert_all(&[
        ("'ab'.isWellFormed()", "true"),
        ("'a\\u{1F600}b'.isWellFormed()", "true"),
        ("'\\uD800'.isWellFormed()", "false"),
        // A trailing surrogate with no leading partner is unpaired too.
        ("'\\uDC00'.isWellFormed()", "false"),
        // Each unpaired surrogate becomes `U+FFFD`.
        (
            "'\\uD800a'.toWellFormed().charCodeAt(0).toString(16)",
            "fffd",
        ),
        ("'a\\u{1F600}b'.toWellFormed().length", "4"),
    ]);
}

/// A nullish receiver throws before any argument is converted.
#[test]
fn a_nullish_receiver_is_rejected_before_any_conversion() {
    for receiver in ["null", "undefined"] {
        assert_throws(
            &format!("return String.prototype.charAt.call({receiver}, 0);"),
            ExceptionKind::TypeError,
            "null or undefined are forbidden",
        );
    }
}

/// A non-string receiver is converted with `ToString`.
#[test]
fn a_non_string_receiver_is_converted_to_a_string() {
    assert_all(&[
        ("String.prototype.charAt.call(12345, 2)", "3"),
        ("String.prototype.indexOf.call(12345, '34')", "2"),
        ("String.prototype.slice.call(true, 1)", "rue"),
    ]);
}

/// The receiver is converted before any argument, and arguments follow their
/// declaration order.
///
/// Oracle: the same source logs `recv,arg,pos`.
#[test]
fn conversions_run_in_receiver_then_argument_order() {
    assert_eq!(
        rendered(
            "(function(){\
                let log='';\
                const recv={toString(){log+='recv,';return 'abc';}};\
                const arg={toString(){log+='arg,';return 'b';}};\
                const pos={valueOf(){log+='pos';return 0;}};\
                String.prototype.indexOf.call(recv, arg, pos);\
                return log;\
            })()"
        ),
        "recv,arg,pos"
    );
}

/// A conversion that re-enters the interpreter still produces the right result.
///
/// This is what the resumable state machine exists for: each `toString` and
/// `valueOf` below runs user bytecode in the middle of the method.
#[test]
fn a_re_entrant_conversion_still_produces_the_right_result() {
    assert_all(&[
        (
            "String.prototype.slice.call(\
                {toString(){return 'abcdef';}},\
                {valueOf(){return 1;}},\
                {valueOf(){return 4;}})",
            "bcd",
        ),
        (
            "'hello world'.padStart(\
                {valueOf(){return 13;}},\
                {toString(){return '-';}})",
            "--hello world",
        ),
        (
            "'abcabc'.lastIndexOf({toString(){return 'b';}}, {valueOf(){return 3;}})",
            "1",
        ),
    ]);
}

/// The installed methods carry the pinned `name` and `length`.
#[test]
fn the_installed_methods_have_the_pinned_shape() {
    assert_all(&[
        ("String.prototype.charAt.name", "charAt"),
        ("String.prototype.charAt.length", "1"),
        // `slice`, `substr`, and `substring` are the arity-2 methods.
        ("String.prototype.slice.length", "2"),
        ("String.prototype.substr.length", "2"),
        ("String.prototype.substring.length", "2"),
        ("String.prototype.trim.length", "0"),
        ("String.prototype.trim.name", "trim"),
        ("String.prototype.isWellFormed.length", "0"),
        ("typeof String.prototype.indexOf", "function"),
        // Each method is writable and configurable but not enumerable.
        (
            "Object.getOwnPropertyDescriptor(String.prototype,'charAt').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(String.prototype,'charAt').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(String.prototype,'charAt').configurable",
            "true",
        ),
    ]);
}
