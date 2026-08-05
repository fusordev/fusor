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
//! "abcabc".replace("b", "X") => "aXcabc"
//! "abc".replace("", "X") => "Xabc"
//! substitution template => "a[b][a][cabc][$][$1][$<x>]cabc"
//! replace conversion order => "get,recv,search,repl"
//! "ababa".replaceAll("a", "X") => "XbXbX"
//! "ab".replaceAll("", callback) callback positions => "0,1,2"
//! "a,,b,".split(",") => ["a", "", "b", ""]
//! "aaaa".split("aa") => ["", "", ""]
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

/// The plain-string `replace` path changes only the first match and expands
/// exactly the substitutions admitted by an empty captures list.
#[test]
fn replace_applies_plain_string_search_and_substitution() {
    assert_all(&[
        ("'abcabc'.replace('b', 'X')", "aXcabc"),
        ("'abcabc'.replace('z', 'X')", "abcabc"),
        ("'abc'.replace('', 'X')", "Xabc"),
        (
            r#""abcabc".replace("b", "[$&][$`][$'][$$][$1][$<x>]")"#,
            "a[b][a][cabc][$][$1][$<x>]cabc",
        ),
        // With no captures, decimal and named-capture references stay literal.
        (
            "'a'.replace('a', '$0|$00|$01|$99|$<x>')",
            "$0|$00|$01|$99|$<x>",
        ),
        // Search and callback positions are UTF-16 code-unit based.
        (
            "(function(){let args;const r='\\uD800x'.replace('x',function(){args=arguments;return 'Y';});return r.charCodeAt(0)+','+r+'|'+args[1]+','+args[2].length;})()",
            "55296,�Y|1,2",
        ),
    ]);
}

/// An object search value gets first refusal through `@@replace`; neither the
/// receiver nor replacement is coerced when that method is present.
#[test]
fn replace_dispatches_the_symbol_protocol_before_fallback_coercion() {
    assert_all(&[
        (
            "(function(){const recv={toString(){throw 1;}};const repl={toString(){throw 2;}};const search={[Symbol.replace](r,x){return (r===recv)+'|'+(x===repl);}};return String.prototype.replace.call(recv,search,repl);})()",
            "true|true",
        ),
        (
            "(function(){let thisValue;const result={};const search={get [Symbol.replace](){return function(){'use strict';thisValue=this;return result;};}};return ('abc'.replace(search,'X')===result)+'|'+(thisValue===search);})()",
            "true|true",
        ),
        (
            "(function(){let log='';const search={get [Symbol.replace](){log+='get,';return undefined;},toString(){log+='search,';return 'b';}};const recv={toString(){log+='recv,';return 'abc';}};const repl={toString(){log+='repl';return 'X';}};const result=String.prototype.replace.call(recv,search,repl);return result+'|'+log;})()",
            "aXc|get,recv,search,repl",
        ),
        // `null` from GetMethod has the same fallback meaning as `undefined`.
        (
            "(function(){const search={[Symbol.replace]:null,toString(){return 'b';}};return 'abc'.replace(search,'X');})()",
            "aXc",
        ),
        (
            "(function(){String.prototype[Symbol.replace]=function(receiver,replacement){return this+'|'+receiver+'|'+replacement;};return 'abc'.replace('b','X');})()",
            "b|abc|X",
        ),
    ]);
    assert_throws(
        "return 'abc'.replace({[Symbol.replace]: 1}, 'X');",
        ExceptionKind::TypeError,
        "not a function",
    );
}

/// Fallback coercions and a functional replacement can each re-enter the VM.
#[test]
fn replace_preserves_fallback_and_callback_observation_order() {
    assert_all(&[
        // A non-callable replacement is converted before the search result is
        // tested, even when there is no match.
        (
            "(function(){let n=0;const result='abc'.replace('z',{toString(){n++;return 'X';}});return result+'|'+n;})()",
            "abc|1",
        ),
        // A callable replacement is not invoked when there is no match.
        (
            "(function(){let n=0;const result='abc'.replace('z',function(){n++;return 'X';});return result+'|'+n;})()",
            "abc|0",
        ),
        (
            "(function(){let log='';const result='abc'.replace('b',function(m,p,s){'use strict';log+=(this===undefined)+','+m+','+p+','+s+';';return {toString(){log+='result';return 'X';}};});return result+'|'+log;})()",
            "aXc|true,b,1,abc;result",
        ),
    ]);
}

/// `RequireObjectCoercible` precedes even the `@@replace` getter.
#[test]
fn replace_rejects_a_nullish_receiver_before_protocol_lookup() {
    assert_all(&[(
        "(function(){let touched=false;const search={get [Symbol.replace](){touched=true;}};try{String.prototype.replace.call(null,search,'X');}catch(e){}return touched;})()",
        "false",
    )]);
    assert_throws(
        "return String.prototype.replace.call(undefined, {}, 'X');",
        ExceptionKind::TypeError,
        "null or undefined are forbidden",
    );
}

/// `replaceAll` discovers every non-overlapping UTF-16 match before applying
/// the same empty-capture substitution rules as `replace`.
#[test]
fn replace_all_applies_every_plain_string_match_and_empty_boundary() {
    assert_all(&[
        ("'ababa'.replaceAll('a', 'X')", "XbXbX"),
        ("'aaaa'.replaceAll('aa', 'X')", "XX"),
        ("'abc'.replaceAll('z', 'X')", "abc"),
        (
            r#""aba".replaceAll("a", "[$&][$`][$'][$$][$1][$<x>]")"#,
            "[a][][ba][$][$1][$<x>]b[a][ab][][$][$1][$<x>]",
        ),
        (
            "(function(){let positions=[];let result='ab'.replaceAll('',function(m,p,s){positions.push(p+':'+s.length);return '<'+p+'>';});return result+'|'+positions.join(',');})()",
            "<0>a<1>b<2>|0:2,1:2,2:2",
        ),
        (
            "(function(){let positions=[];let result='\\uD800x\\uD800'.replaceAll('\\uD800',function(m,p,s){positions.push(p+':'+s.length);return 'Y';});return result.charCodeAt(1)+'|'+positions.join(',');})()",
            "120|0:3,2:3",
        ),
    ]);
}

/// `replaceAll` performs `IsRegExp`, the global-flag check, and `GetMethod`
/// before any fallback conversion, with each getter able to re-enter the VM.
#[test]
fn replace_all_preserves_match_flags_replace_and_fallback_order() {
    assert_all(&[
        (
            "(function(){let log=[];let search={get [Symbol.match](){log.push('match');return true;},get flags(){log.push('flags');return {toString(){log.push('flags-string');return 'g';}};},get [Symbol.replace](){log.push('replace');return function(r,v){log.push('call');return (r===recv)+'|'+(v===replacement);};}};let recv={toString(){log.push('recv');return 'aba';}};let replacement={toString(){log.push('replacement');return 'X';}};let result=String.prototype.replaceAll.call(recv,search,replacement);return result+'#'+log.join(',');})()",
            "true|true#match,flags,flags-string,replace,call",
        ),
        (
            "(function(){let log=[];let search={get [Symbol.match](){log.push('match');return false;},get flags(){log.push('bad-flags');return 'g';},get [Symbol.replace](){log.push('replace');return undefined;},toString(){log.push('search');return 'a';}};let recv={toString(){log.push('recv');return 'aba';}};let replacement={toString(){log.push('replacement');return 'X';}};let result=String.prototype.replaceAll.call(recv,search,replacement);return result+'#'+log.join(',');})()",
            "XbX#match,replace,recv,search,replacement",
        ),
        (
            "(function(){String.prototype[Symbol.replace]=function(receiver,replacement){return this+'|'+receiver+'|'+replacement;};return 'aba'.replaceAll('a','X');})()",
            "a|aba|X",
        ),
        (
            "(function(){let touched=false;let search={get [Symbol.match](){touched=true;return true;}};try{String.prototype.replaceAll.call(null,search,'X');}catch(error){}return touched;})()",
            "false",
        ),
    ]);
}

#[test]
fn replace_all_rejects_regexp_like_objects_without_a_global_flag() {
    assert_throws(
        "return 'abc'.replaceAll({[Symbol.match]:true,flags:'i',[Symbol.replace](){return 'bad';}},'X');",
        ExceptionKind::TypeError,
        "regexp must have the 'g' flag",
    );
    assert_throws(
        "return 'abc'.replaceAll({[Symbol.match]:true,flags:null,[Symbol.replace](){return 'bad';}},'X');",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return 'abc'.replaceAll({[Symbol.match]:false,[Symbol.replace]:1},'X');",
        ExceptionKind::TypeError,
        "not a function",
    );
}

/// The plain-string `split` path finds non-overlapping UTF-16 matches and
/// preserves leading, adjacent, and trailing empty substrings.
#[test]
fn split_applies_plain_string_separator_and_uint32_limit() {
    assert_all(&[
        ("'a,b,c'.split(',').join('|')", "a|b|c"),
        ("'a,,b,'.split(',').join('|')", "a||b|"),
        ("'aaaa'.split('aa').join('|')", "||"),
        ("'abc'.split('z').join('|')", "abc"),
        ("'a,b,c'.split(',', 2).join('|')", "a|b"),
        ("'a,b'.split(',', 0).length", "0"),
        ("'a,b'.split(',', -1).join('|')", "a|b"),
        ("'a,b'.split(',', 4294967297).join('|')", "a"),
        ("'a,b'.split(',', 4294967296).length", "0"),
        ("'a,b'.split(',', Infinity).length", "0"),
        ("'a,b'.split(',', NaN).length", "0"),
        ("'a,b,c'.split(',', 2.9).join('|')", "a|b"),
        ("'abc'.split(undefined).join('|')", "abc"),
        ("'null'.split(null).length", "2"),
    ]);
}

/// An empty separator splits into UTF-16 code units, including lone
/// surrogates, and the empty-subject corner cases follow the specification.
#[test]
fn split_handles_empty_separator_and_utf16_boundaries() {
    assert_all(&[
        ("'ab'.split('').join('|')", "a|b"),
        ("''.split('').length", "0"),
        ("''.split('x').length", "1"),
        ("''.split('x')[0]", ""),
        (
            "(function(){const a=String.fromCharCode(0xD800,120).split('');return a.length+'|'+a[0].charCodeAt(0)+'|'+a[1].charCodeAt(0);})()",
            "2|55296|120",
        ),
    ]);
}

/// `GetMethod(separator, @@split)` precedes every fallback coercion and a
/// callable protocol receives the original receiver and limit unchanged.
#[test]
fn split_dispatches_symbol_protocol_before_fallback_coercion() {
    assert_all(&[
        (
            "(function(){let log=[];const receiver={toString(){log.push('receiver');return 'a,b';}};const limit={valueOf(){log.push('limit');return 1;}};const separator={get [Symbol.split](){log.push('get');return function(r,l){log.push('call');return (this===separator)+'|'+(r===receiver)+'|'+(l===limit);};},toString(){throw 1;}};const result=String.prototype.split.call(receiver,separator,limit);return result+'#'+log.join(',');})()",
            "true|true|true#get,call",
        ),
        (
            "(function(){let log=[];const receiver={toString(){log.push('receiver');return 'a,b';}};const limit={valueOf(){log.push('limit');return 1;}};const separator={get [Symbol.split](){log.push('get');return undefined;},toString(){log.push('separator');return ',';}};const result=String.prototype.split.call(receiver,separator,limit);return result.join('|')+'#'+log.join(',');})()",
            "a#get,receiver,limit,separator",
        ),
        // ES2025 GetMethod applies to non-null primitives as well as objects.
        (
            "(function(){String.prototype[Symbol.split]=function(receiver,limit){return this+'|'+receiver+'|'+limit;};return 'abc'.split('b',2);})()",
            "b|abc|2",
        ),
        (
            "(function(){const separator={[Symbol.split]:null,toString(){return ',';}};return 'a,b'.split(separator).join('|');})()",
            "a|b",
        ),
    ]);
    assert_throws(
        "return 'abc'.split({[Symbol.split]: 1});",
        ExceptionKind::TypeError,
        "not a function",
    );
}

/// `RequireObjectCoercible` precedes even the `@@split` getter, while the
/// fallback always converts receiver, limit, and separator in normative order.
#[test]
fn split_preserves_nullish_and_fallback_observation_order() {
    assert_all(&[
        (
            "(function(){let touched=false;const separator={get [Symbol.split](){touched=true;}};try{String.prototype.split.call(null,separator);}catch(error){}return touched;})()",
            "false",
        ),
        // The separator conversion is observable even when limit becomes 0.
        (
            "(function(){let log=[];const receiver={toString(){log.push('receiver');return 'a,b';}};const limit={valueOf(){log.push('limit');return 0;}};const separator={get [Symbol.split](){log.push('get');return undefined;},toString(){log.push('separator');return ',';}};const result=String.prototype.split.call(receiver,separator,limit);return result.length+'#'+log.join(',');})()",
            "0#get,receiver,limit,separator",
        ),
        (
            "(function(){let log=[];const separator={get [Symbol.split](){log.push('get');return undefined;},toString(){log.push('separator');throw 3;}};const receiver={toString(){log.push('receiver');return 'a,b';}};const limit={valueOf(){log.push('limit');throw 2;}};try{String.prototype.split.call(receiver,separator,limit);}catch(error){log.push('throw'+error);}return log.join(',');})()",
            "get,receiver,limit,throw2",
        ),
        (
            "(function(){let log=[];const separator={get [Symbol.split](){log.push('get');throw 1;}};const receiver={toString(){log.push('receiver');return 'a,b';}};try{String.prototype.split.call(receiver,separator);}catch(error){log.push('throw'+error);}return log.join(',');})()",
            "get,throw1",
        ),
    ]);
    assert_throws(
        "return String.prototype.split.call(undefined, ',');",
        ExceptionKind::TypeError,
        "null or undefined are forbidden",
    );
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

/// Default case conversion uses full, context-sensitive Unicode mappings.
#[test]
fn unicode_case_conversion_handles_context_expansion_and_surrogates() {
    assert_all(&[
        // Final sigma is context-sensitive and cannot be implemented by
        // lowercasing one Rust `char` at a time.
        ("'ΟΣ'.toLowerCase()", "ος"),
        // Full uppercasing can expand one code point into several.
        ("'Straße'.toUpperCase()", "STRASSE"),
        ("'\\u0130'.toLowerCase()", "i̇"),
        // Locale-named methods use the deterministic root locale in this
        // no-Intl profile and ignore their reserved arguments.
        ("'I'.toLocaleLowerCase('tr')", "i"),
        ("'i'.toLocaleUpperCase('tr')", "I"),
        (
            "(function(){let used=false;const locale={toString(){used=true;return 'tr';}};'I'.toLocaleLowerCase(locale);return used;})()",
            "false",
        ),
        // Unicode transforms must preserve an ECMAScript lone surrogate.
        (
            "'\\uD800A'.toLowerCase().charCodeAt(0).toString(16)+'|'+ '\\uD800A'.toLowerCase().charAt(1)",
            "d800|a",
        ),
    ]);
}

/// All four Unicode normalization forms are exact and preserve lone
/// surrogates rather than replacing them with `U+FFFD`.
#[test]
fn normalization_supports_all_forms_and_exact_conversion_order() {
    assert_all(&[
        ("'\\u212B'.normalize().charCodeAt(0).toString(16)", "c5"),
        (
            "'\\u212B'.normalize(undefined).charCodeAt(0).toString(16)",
            "c5",
        ),
        (
            "'\\u212B'.normalize('NFD').charCodeAt(0).toString(16)+'|'+ '\\u212B'.normalize('NFD').charCodeAt(1).toString(16)",
            "41|30a",
        ),
        ("'\\uFB00'.normalize('NFC')==='\\uFB00'", "true"),
        ("'\\uFB00'.normalize('NFKC')", "ff"),
        ("'\\uFB00'.normalize('NFKD')", "ff"),
        ("'\\uD800'.normalize().charCodeAt(0).toString(16)", "d800"),
        (
            "(function(){let log='';const recv={toString(){log+='recv,';return '\\u212B';}};const form={toString(){log+='form';return 'NFD';}};String.prototype.normalize.call(recv,form);return log;})()",
            "recv,form",
        ),
    ]);
    assert_throws(
        "return 'x'.normalize('bad');",
        ExceptionKind::RangeError,
        "bad normalization form",
    );
}

/// The deterministic no-Intl comparator orders NFC representatives, making
/// every canonically equivalent pair compare equal without folding
/// compatibility equivalents together.
#[test]
fn locale_compare_honours_canonical_equivalence_and_total_order() {
    assert_all(&[
        ("'\\u212B'.localeCompare('A\\u030A')", "0"),
        ("'\\u2126'.localeCompare('\\u03A9')", "0"),
        ("'\\u1E69'.localeCompare('s\\u0307\\u0323')", "0"),
        ("'\\u1E0B\\u0323'.localeCompare('\\u1E0D\\u0307')", "0"),
        ("'\\u1100\\u1161'.localeCompare('\\uAC00')", "0"),
        ("'a'.localeCompare('b') < 0", "true"),
        ("'b'.localeCompare('a') > 0", "true"),
        ("'\\uFB00'.localeCompare('ff') !== 0", "true"),
        (
            "(function(){let log='';const recv={toString(){log+='recv,';return 'a';}};const that={toString(){log+='that';return 'b';}};const ignored={toString(){log+=',ignored';return 'x';}};String.prototype.localeCompare.call(recv,that,ignored);return log;})()",
            "recv,that",
        ),
    ]);
}

#[test]
fn unicode_transforms_consume_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, "return 'A'.repeat(1000).toLowerCase();");
    assert!(matches!(
        context.call(
            &run,
            &[],
            ExecutionLimits::default().with_instruction_fuel(100),
        ),
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
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
        ("String.prototype.replace.length", "2"),
        ("String.prototype.replace.name", "replace"),
        ("String.prototype.replaceAll.length", "2"),
        ("String.prototype.replaceAll.name", "replaceAll"),
        ("String.prototype.match.length", "1"),
        ("String.prototype.match.name", "match"),
        ("String.prototype.search.length", "1"),
        ("String.prototype.search.name", "search"),
        ("String.prototype.split.length", "2"),
        ("String.prototype.split.name", "split"),
        ("String.prototype.trim.length", "0"),
        ("String.prototype.trim.name", "trim"),
        ("String.prototype.isWellFormed.length", "0"),
        ("String.prototype.localeCompare.length", "1"),
        ("String.prototype.normalize.length", "0"),
        ("String.prototype.toLocaleLowerCase.length", "0"),
        ("String.prototype.toLocaleUpperCase.length", "0"),
        ("String.prototype.toLowerCase.length", "0"),
        ("String.prototype.toUpperCase.length", "0"),
        ("String.prototype.localeCompare.name", "localeCompare"),
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

#[test]
fn annex_b_html_methods_cover_the_complete_create_html_surface() {
    assert_eq!(
        rendered(
            "(function(){let s='x';return [s.anchor('a\\\"b'),s.big(),s.blink(),s.bold(),\
             s.fixed(),s.fontcolor('r\\\"d'),s.fontsize(3),s.italics(),s.link('u\\\"v'),\
             s.small(),s.strike(),s.sub(),s.sup()].join('|');})()"
        ),
        "<a name=\"a&quot;b\">x</a>|<big>x</big>|<blink>x</blink>|<b>x</b>|\
         <tt>x</tt>|<font color=\"r&quot;d\">x</font>|<font size=\"3\">x</font>|\
         <i>x</i>|<a href=\"u&quot;v\">x</a>|<small>x</small>|<strike>x</strike>|\
         <sub>x</sub>|<sup>x</sup>"
    );
}

#[test]
fn annex_b_html_conversion_order_and_trim_aliases_follow_the_specification() {
    assert_eq!(
        rendered(
            "(function(){\
                let log='';\
                let receiver={toString(){log+='receiver|';return 'x';}};\
                let attribute={toString(){log+='attribute';return 'v\\\"q';}};\
                let html=String.prototype.anchor.call(receiver,attribute);\
                let nullish=false;\
                try{String.prototype.link.call(null,{toString(){log+='bad';return 'u';}});}\
                catch(error){nullish=error.name==='TypeError';}\
                return html+'#'+log+'#'+nullish+'#'+\
                    (String.prototype.trimEnd===String.prototype.trimRight)+'#'+\
                    (String.prototype.trimStart===String.prototype.trimLeft)+'#'+\
                    String.prototype.trimRight.name+'#'+String.prototype.trimLeft.name;\
            })()"
        ),
        "<a name=\"v&quot;q\">x</a>#receiver|attribute#true#true#true#trimEnd#trimStart"
    );
}

#[test]
fn supported_string_prototype_names_preserve_the_pinned_quickjs_order() {
    assert_eq!(
        rendered("Object.getOwnPropertyNames(String.prototype).join('|')"),
        "length|at|charCodeAt|charAt|concat|codePointAt|isWellFormed|toWellFormed|\
         indexOf|lastIndexOf|includes|endsWith|startsWith|match|matchAll|search|split|substring|substr|slice|repeat|\
         replace|replaceAll|padEnd|padStart|trim|trimEnd|trimRight|trimStart|trimLeft|toString|\
         valueOf|toLowerCase|toUpperCase|toLocaleLowerCase|toLocaleUpperCase|anchor|big|\
         blink|bold|fixed|fontcolor|fontsize|italics|link|small|strike|sub|sup|\
         constructor|normalize|localeCompare"
    );
    assert_all(&[
        ("String.prototype.anchor.length", "1"),
        ("String.prototype.anchor.name", "anchor"),
        ("String.prototype.big.length", "0"),
        ("String.prototype.fontcolor.length", "1"),
        ("String.prototype.fontsize.length", "1"),
        ("String.prototype.link.length", "1"),
        (
            "Object.getOwnPropertyDescriptor(String.prototype,'anchor').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(String.prototype,'anchor').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(String.prototype,'anchor').configurable",
            "true",
        ),
    ]);
}

#[test]
fn match_and_search_dispatch_before_receiver_coercion() {
    assert_eq!(
        rendered(
            "(function(){var log=[];var receiver={toString:function(){log.push('receiver');throw 1}};\
              var regexp={get [Symbol.match](){log.push('get');return function(value){\
                log.push('call');return (this===regexp)+'|'+(value===receiver)}}};\
              var result=String.prototype.match.call(receiver,regexp);return result+'#'+log.join(',');})()"
        ),
        "true|true#get,call"
    );
    assert_eq!(
        rendered(
            "(function(){var log=[];var receiver={toString:function(){log.push('receiver');throw 1}};\
              var regexp={get [Symbol.search](){log.push('get');return function(value){\
                log.push('call');return (this===regexp)+'|'+(value===receiver)}}};\
              var result=String.prototype.search.call(receiver,regexp);return result+'#'+log.join(',');})()"
        ),
        "true|true#get,call"
    );
}

#[test]
fn match_and_search_fallback_construct_regexp_and_invoke_the_protocol() {
    assert_eq!(
        rendered(
            "(function(){var log=[];var receiver={toString:function(){log.push('receiver');return 'abc'}};\
              var pattern={get [Symbol.match](){log.push('match');return undefined},\
                toString:function(){log.push('pattern');return '.'}};\
              RegExp.prototype[Symbol.match]=function(value){log.push('invoke');return this.source+'|'+value};\
              var result=String.prototype.match.call(receiver,pattern);return result+'#'+log.join(',');})()"
        ),
        ".|abc#match,receiver,match,pattern,invoke"
    );
    assert_all(&[
        ("'abc'.match('.')[0]", "a"),
        ("'abc'.search('b')", "1"),
        ("'abc'.search('z')", "-1"),
    ]);
}

#[test]
fn match_and_search_observe_primitive_wrapper_protocols_per_es2025() {
    assert_eq!(
        rendered(
            "(function(){String.prototype[Symbol.match]=function(value){return 'match:'+this+':'+value};\
              String.prototype[Symbol.search]=function(value){return 'search:'+this+':'+value};\
              return String.prototype.match.call('abc','b')+'|'+String.prototype.search.call('abc','b');})()"
        ),
        "match:b:abc|search:b:abc"
    );
}

#[test]
fn match_and_search_reject_nullish_receivers_before_protocol_lookup() {
    assert_eq!(
        rendered(
            "(function(){var touched=false;var regexp={get [Symbol.match](){touched=true}};\
              try{String.prototype.match.call(null,regexp)}catch(error){}return touched;})()"
        ),
        "false"
    );
    assert_eq!(
        thrown("return String.prototype.search.call(undefined, {});").0,
        ExceptionKind::TypeError
    );
}
