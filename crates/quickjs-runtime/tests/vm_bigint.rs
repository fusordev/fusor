//! The `BigInt` value domain, pinned to the ECMAScript specification.
//!
//! Where the pinned `QuickJS` 2026-06-04 engine and the specification agree, the
//! oracle transcript is cited. Where they disagree the specification governs and
//! the divergence carries an identifier; see `QJS-BIGINT-001` in `PORTING.md`.
//!
//! Oracle transcript for the agreeing behaviors:
//!
//! ```text
//! typeof 1n => [bigint]
//! !!0n => [false]        !!1n => [true]        !!-1n => [true]
//! 1n===1n => [true]      1n===1 => [false]     -0n===0n => [true]
//! String(1n) => [1]      String(-1n) => [-1]   1n+'' => [1]
//! (255n).toString(16) => [ff]
//! (1n).valueOf() => [1]
//! Object.prototype.toString.call(1n) => [[object BigInt]]
//! BigInt.prototype[Symbol.toStringTag] => [BigInt]
//! obj[1n] key => [1|v]
//! 1n+1 !! TypeError: cannot convert bigint to number
//! +1n  !! TypeError: bigint argument with unary +
//! -1n => [-1]            ~1n => [-2]
//! 1n>>>1n !! TypeError: bigint operands are forbidden for >>>
//! 1n+'s' => [1s]         's'+1n => [s1]
//! 1n+2n => [3]           5n/2n => [2]          2n**10n => [1024]
//! BigInt(5) => [5]       BigInt(true) => [1]   BigInt('0x10') => [16]
//! BigInt(1.5) !! RangeError: cannot convert to BigInt: not an integer
//! BigInt(NaN) !! RangeError: cannot convert NaN or Infinity to BigInt
//! BigInt('1.5') !! SyntaxError: invalid bigint literal
//! BigInt(null) !! TypeError: cannot convert to BigInt
//! new BigInt(1) !! TypeError: BigInt is not a constructor
//! BigInt.length => [1]   BigInt.name => [BigInt]
//! BigInt own names => [length,name,asUintN,asIntN,prototype]
//! BigInt.prototype own => [toString,valueOf,constructor]
//! Object(1n) typeof => [object|true]
//! BigInt.prototype.valueOf.call({}) !! TypeError: not a BigInt
//! (1n).toString(1) !! RangeError: radix must be between 2 and 36
//! BigInt.asIntN(8,255n) => [-1n]   BigInt.asUintN(8,-1n) => [255n]
//! BigInt.asIntN(0,5n) => [0n]      BigInt.asIntN(1,1n) => [-1n]
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
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime BigInt>"))
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

fn text(body: &str) -> String {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn boolean(body: &str) -> bool {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_boolean()
            .expect("live value")
            .expect("Boolean")
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

/// Oracle: `typeof 1n => [bigint]`.
#[test]
fn typeof_a_bigint_is_bigint() {
    assert_eq!(text("return typeof BigInt(1);"), "bigint");
    assert_eq!(text("return typeof BigInt('0x10');"), "bigint");
}

/// Oracle: `!!0n => [false]`, `!!1n => [true]`, `!!-1n => [true]`.
///
/// `0n` is the only falsy `BigInt`: the domain has no negative zero and no NaN.
#[test]
fn only_zero_is_falsy() {
    assert!(!boolean("return !!BigInt(0);"));
    assert!(boolean("return !!BigInt(1);"));
    assert!(boolean("return !!BigInt(-1);"));
}

/// Oracle: `1n===1n => [true]`, `1n===1 => [false]`, `-0n===0n => [true]`.
#[test]
fn strict_equality_compares_values_and_never_crosses_domains() {
    assert!(boolean("return BigInt(1)===BigInt(1);"));
    assert!(boolean("return BigInt(0)===BigInt(-0);"));
    assert!(!boolean("return BigInt(1)===1;"));
    assert!(!boolean("return BigInt(1)===BigInt(2);"));
    // Large values compare by magnitude, not by identity.
    assert!(boolean(
        "return BigInt('18446744073709551616')===BigInt('18446744073709551616');"
    ));
}

/// Oracle: `String(1n) => [1]`, `String(-1n) => [-1]`, `1n+'' => [1]`.
///
/// `ToString` produces no `n` suffix: the suffix belongs to the literal grammar.
#[test]
fn string_conversion_omits_the_literal_suffix() {
    assert_eq!(text("return String(BigInt(1));"), "1");
    assert_eq!(text("return String(BigInt(-1));"), "-1");
    assert_eq!(text("return BigInt(1)+'';"), "1");
    assert_eq!(
        text("return String(BigInt('18446744073709551616'));"),
        "18446744073709551616"
    );
}

/// Oracle: `1n+'s' => [1s]` and `'s'+1n => [s1]`.
///
/// String concatenation is the one `+` form a `BigInt` participates in, because
/// it stringifies rather than converting to a Number.
#[test]
fn addition_with_a_string_concatenates() {
    assert_eq!(text("return BigInt(1)+'s';"), "1s");
    assert_eq!(text("return 's'+BigInt(1);"), "s1");
}

/// Oracle: `(255n).toString(16) => [ff]` and
/// `(1n).toString(1) !! RangeError: radix must be between 2 and 36`.
#[test]
fn prototype_to_string_accepts_a_radix() {
    assert_eq!(text("return BigInt(255).toString(16);"), "ff");
    assert_eq!(text("return BigInt(255).toString(2);"), "11111111");
    assert_eq!(text("return BigInt(255).toString();"), "255");
    assert_eq!(text("return BigInt(-255).toString(16);"), "-ff");
    assert_throws(
        "return BigInt(1).toString(1);",
        ExceptionKind::RangeError,
        "radix must be between 2 and 36",
    );
    assert_throws(
        "return BigInt(1).toString(37);",
        ExceptionKind::RangeError,
        "radix must be between 2 and 36",
    );
}

/// Oracle: `(1n).valueOf() => [1]` and
/// `BigInt.prototype.valueOf.call({}) !! TypeError: not a BigInt`.
#[test]
fn prototype_value_of_requires_a_bigint_receiver() {
    assert!(boolean("return BigInt(1).valueOf()===BigInt(1);"));
    assert_eq!(text("return typeof BigInt(1).valueOf();"), "bigint");
    assert_throws(
        "return BigInt.prototype.valueOf.call({});",
        ExceptionKind::TypeError,
        "not a BigInt",
    );
    assert_throws(
        "return BigInt.prototype.toString.call(5);",
        ExceptionKind::TypeError,
        "not a BigInt",
    );
    // An `Object(bigint)` wrapper is an accepted receiver.
    assert!(boolean(
        "return BigInt.prototype.valueOf.call(Object(BigInt(7)))===BigInt(7);"
    ));
}

/// Oracle: `Object.prototype.toString.call(1n) => [[object BigInt]]` and
/// `BigInt.prototype[Symbol.toStringTag] => [BigInt]`.
#[test]
fn object_prototype_to_string_tags_a_bigint() {
    assert_eq!(
        text("return Object.prototype.toString.call(BigInt(1));"),
        "[object BigInt]"
    );
    assert_eq!(
        text("return BigInt.prototype[Symbol.toStringTag];"),
        "BigInt"
    );
    assert_eq!(
        text("return Object.prototype.toString.call(Object(BigInt(1)));"),
        "[object BigInt]"
    );
}

/// Oracle: `obj[1n] key => [1|v]`. `ToPropertyKey` stringifies, so a `BigInt`
/// key addresses the same slot as the equivalent Number.
#[test]
fn a_bigint_property_key_stringifies() {
    assert!(boolean(
        "var o={};o[BigInt(1)]='v';return o[1]==='v'&&o['1']==='v';"
    ));
    assert_eq!(
        text("var o={};o[BigInt(1)]='v';return Object.keys(o).join(',');"),
        "1"
    );
}

/// Oracle: `1n+1`, `1n-1`, `1n*1`, `1n/1`, `1n%1`, `1n**1`, `1n&1`, `1n|1`,
/// `1n^1`, `1n<<1`, `1n>>1` all report
/// `TypeError: cannot convert bigint to number`.
///
/// This is the property that keeps the two numeric domains separate.
#[test]
fn mixing_a_bigint_with_a_number_throws() {
    for operator in ["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>"] {
        assert_throws(
            &format!("return BigInt(1){operator}1;"),
            ExceptionKind::TypeError,
            "cannot convert bigint to number",
        );
        // The reverse operand order fails identically.
        assert_throws(
            &format!("return 1{operator}BigInt(1);"),
            ExceptionKind::TypeError,
            "cannot convert bigint to number",
        );
    }
}

/// Oracle: `Math.max(1n)` and `Number(1n)` show that an explicit numeric
/// coercion is where a `BigInt` is rejected. `Number(1n)` is `1` upstream, but
/// `Number` is a deliberate conversion rather than an operator.
#[test]
fn an_implicit_numeric_coercion_rejects_a_bigint() {
    assert_throws(
        "return BigInt(1)|0;",
        ExceptionKind::TypeError,
        "cannot convert bigint to number",
    );
}

/// Oracle: `+1n !! TypeError: bigint argument with unary +`, while `-1n` is
/// `-1n` and `~1n` is `-2n`.
#[test]
fn unary_operators_follow_the_bigint_domain() {
    assert_throws(
        "return +BigInt(1);",
        ExceptionKind::TypeError,
        "bigint argument with unary +",
    );
    assert!(boolean("return -BigInt(1)===BigInt(-1);"));
    assert!(boolean("return ~BigInt(1)===BigInt(-2);"));
    assert_eq!(text("return String(-BigInt(1));"), "-1");
    assert_eq!(text("return String(~BigInt(1));"), "-2");
}

/// Oracle: `1n>>>1n !! TypeError: bigint operands are forbidden for >>>`.
///
/// Unsigned right shift has no meaning for an unbounded two's-complement value.
#[test]
fn unsigned_right_shift_forbids_bigint_operands() {
    assert_throws(
        "return BigInt(1)>>>BigInt(1);",
        ExceptionKind::TypeError,
        "bigint operands are forbidden for >>>",
    );
}

/// Oracle: `1n+2n => [3]`, `5n/2n => [2]`, `5n%2n => [1]`, `2n**10n => [1024]`.
#[test]
fn same_domain_arithmetic_stays_in_the_bigint_domain() {
    assert_eq!(text("return String(BigInt(1)+BigInt(2));"), "3");
    assert_eq!(text("return String(BigInt(5)-BigInt(2));"), "3");
    assert_eq!(text("return String(BigInt(5)*BigInt(2));"), "10");
    // Division truncates toward zero.
    assert_eq!(text("return String(BigInt(5)/BigInt(2));"), "2");
    assert_eq!(text("return String(BigInt(-5)/BigInt(2));"), "-2");
    assert_eq!(text("return String(BigInt(5)%BigInt(2));"), "1");
    assert_eq!(text("return String(BigInt(-5)%BigInt(2));"), "-1");
    assert_eq!(text("return String(BigInt(2)**BigInt(10));"), "1024");
    assert_eq!(
        text("return String(BigInt(2)**BigInt(64));"),
        "18446744073709551616"
    );
    assert_eq!(text("return typeof (BigInt(1)+BigInt(2));"), "bigint");
}

/// Same-domain bitwise operators work on the two's-complement value.
#[test]
fn same_domain_bitwise_operators_use_two_s_complement() {
    assert_eq!(text("return String(BigInt(12)&BigInt(10));"), "8");
    assert_eq!(text("return String(BigInt(12)|BigInt(10));"), "14");
    assert_eq!(text("return String(BigInt(12)^BigInt(10));"), "6");
    assert_eq!(text("return String(BigInt(-1)&BigInt(255));"), "255");
    assert_eq!(
        text("return String(BigInt(1)<<BigInt(64));"),
        "18446744073709551616"
    );
    assert_eq!(text("return String(BigInt(-1)>>BigInt(1));"), "-1");
}

/// Oracle: `BigInt(5) => [5]`, `BigInt(true) => [1]`, `BigInt('0x10') => [16]`.
#[test]
fn the_constructor_coerces_its_argument() {
    assert_eq!(text("return String(BigInt(5));"), "5");
    assert_eq!(text("return String(BigInt(true));"), "1");
    assert_eq!(text("return String(BigInt(false));"), "0");
    assert_eq!(text("return String(BigInt('0x10'));"), "16");
    assert_eq!(text("return String(BigInt('0b101'));"), "5");
    assert_eq!(text("return String(BigInt(''));"), "0");
    assert_eq!(text("return String(BigInt('  12  '));"), "12");
    assert_eq!(text("return String(BigInt('-7'));"), "-7");
}

/// Oracle: `BigInt(1.5)`, `BigInt(NaN)`, `BigInt('1.5')`, `BigInt(null)`, and
/// `BigInt(Symbol())` each report a distinct pinned message.
#[test]
fn the_constructor_rejects_inconvertible_arguments() {
    assert_throws(
        "return BigInt(1.5);",
        ExceptionKind::RangeError,
        "cannot convert to BigInt: not an integer",
    );
    assert_throws(
        "return BigInt(NaN);",
        ExceptionKind::RangeError,
        "cannot convert NaN or Infinity to BigInt",
    );
    assert_throws(
        "return BigInt(Infinity);",
        ExceptionKind::RangeError,
        "cannot convert NaN or Infinity to BigInt",
    );
    assert_throws(
        "return BigInt('1.5');",
        ExceptionKind::SyntaxError,
        "invalid bigint literal",
    );
    assert_throws(
        "return BigInt(null);",
        ExceptionKind::TypeError,
        "cannot convert to BigInt",
    );
    assert_throws(
        "return BigInt(undefined);",
        ExceptionKind::TypeError,
        "cannot convert to BigInt",
    );
    assert_throws(
        "return BigInt(Symbol());",
        ExceptionKind::TypeError,
        "cannot convert to BigInt",
    );
}

/// Oracle: `new BigInt(1) !! TypeError: BigInt is not a constructor`.
#[test]
fn the_constructor_is_not_constructable() {
    assert_throws(
        "return new BigInt(1);",
        ExceptionKind::TypeError,
        "BigInt is not a constructor",
    );
}

/// Oracle: `BigInt.length => [1]`, `BigInt.name => [BigInt]`,
/// `BigInt own names => [length,name,asUintN,asIntN,prototype]`,
/// `BigInt.prototype own => [toString,valueOf,constructor]`.
///
/// The prototype deliberately has no `toLocaleString`.
#[test]
fn the_constructor_has_the_pinned_shape() {
    assert_eq!(text("return String(BigInt.length);"), "1");
    assert_eq!(text("return BigInt.name;"), "BigInt");
    assert!(boolean("return BigInt.prototype.constructor===BigInt;"));
    assert_eq!(text("return typeof BigInt.asIntN;"), "function");
    assert_eq!(text("return typeof BigInt.asUintN;"), "function");
    assert_eq!(text("return String(BigInt.asIntN.length);"), "2");
    assert_eq!(text("return String(BigInt.asUintN.length);"), "2");
    assert_eq!(
        text("return typeof BigInt.prototype.toLocaleString;"),
        "undefined"
    );
}

/// Oracle: `Object(1n) typeof => [object|true]`.
#[test]
fn object_boxes_a_bigint_into_a_wrapper() {
    assert_eq!(text("return typeof Object(BigInt(1));"), "object");
    assert!(boolean("return Object(BigInt(1)) instanceof BigInt;"));
    assert!(boolean(
        "return Object.getPrototypeOf(Object(BigInt(1)))===BigInt.prototype;"
    ));
    // A `BigInt` primitive reads its methods through `BigInt.prototype` without
    // an observable wrapper.
    assert!(boolean(
        "return Object.getPrototypeOf(BigInt(1))===BigInt.prototype;"
    ));
}

/// Oracle: `BigInt.asIntN(8,255n) => [-1n]`, `BigInt.asUintN(8,-1n) => [255n]`,
/// `BigInt.asIntN(0,5n) => [0n]`, `BigInt.asIntN(1,1n) => [-1n]`.
#[test]
fn as_int_n_truncates_to_a_signed_width() {
    assert_eq!(text("return String(BigInt.asIntN(8,BigInt(255)));"), "-1");
    assert_eq!(text("return String(BigInt.asIntN(0,BigInt(5)));"), "0");
    assert_eq!(text("return String(BigInt.asIntN(1,BigInt(1)));"), "-1");
    assert_eq!(text("return String(BigInt.asIntN(8,BigInt(127)));"), "127");
    assert_eq!(text("return String(BigInt.asIntN(8,BigInt(128)));"), "-128");
    assert_eq!(text("return String(BigInt.asIntN(64,BigInt(-1)));"), "-1");
    // `bits` is converted with `ToIndex`, so it truncates.
    assert_eq!(text("return String(BigInt.asIntN(8.9,BigInt(255)));"), "-1");
}

/// `BigInt.asUintN` is always non-negative.
///
/// The pinned engine returns its argument unchanged once the width spans the
/// value, so it reports `-1n` for widths of 64 and above (`quickjs.c:56092`).
/// ECMAScript defines the result modulo `2**bits`, and V8 agrees with the
/// specification, so this port follows it. See `QJS-BIGINT-001`.
#[test]
fn as_uint_n_is_always_non_negative() {
    // Widths below 64 agree with both engines.
    assert_eq!(text("return String(BigInt.asUintN(8,BigInt(-1)));"), "255");
    assert_eq!(
        text("return String(BigInt.asUintN(32,BigInt(-1)));"),
        "4294967295"
    );
    assert_eq!(text("return String(BigInt.asUintN(8,BigInt(256)));"), "0");
    assert_eq!(text("return String(BigInt.asUintN(0,BigInt(5)));"), "0");
    // Widths of 64 and above follow the specification.
    assert_eq!(
        text("return String(BigInt.asUintN(64,BigInt(-1)));"),
        "18446744073709551615"
    );
    assert_eq!(
        text("return String(BigInt.asUintN(65,BigInt(-1)));"),
        "36893488147419103231"
    );
    assert!(boolean("return BigInt.asUintN(64,BigInt(-1))>BigInt(0);"));
}

/// `ToIndex` bounds the requested width.
#[test]
fn a_truncation_width_outside_the_index_range_is_rejected() {
    assert_throws(
        "return BigInt.asUintN(-1,BigInt(5));",
        ExceptionKind::RangeError,
        "invalid array index",
    );
    assert_throws(
        "return BigInt.asIntN(9007199254740992,BigInt(5));",
        ExceptionKind::RangeError,
        "invalid array index",
    );
    // `NaN` and `undefined` convert to zero rather than failing.
    assert_eq!(text("return String(BigInt.asUintN(NaN,BigInt(5)));"), "0");
}

/// Division and remainder by zero report the pinned `RangeError`.
#[test]
fn division_by_zero_is_a_range_error() {
    assert_throws(
        "return BigInt(1)/BigInt(0);",
        ExceptionKind::RangeError,
        "division by zero",
    );
    assert_throws(
        "return BigInt(1)%BigInt(0);",
        ExceptionKind::RangeError,
        "division by zero",
    );
}

/// A negative exponent is a `RangeError`, since the result would not be integral.
#[test]
fn a_negative_exponent_is_a_range_error() {
    assert_throws(
        "return BigInt(2)**BigInt(-1);",
        ExceptionKind::RangeError,
        "exponent must be non-negative",
    );
}

/// Oracle: `1n<2 => [true]`, `2n>1 => [true]`, `1n<=1 => [true]`,
/// `1n<NaN => [false]`, `1n>NaN => [false]`, `1n<'2' => [true]`,
/// `1n<1.5 => [true]`, `2n<1.5 => [false]`.
///
/// Relational comparison is the one place the two numeric domains mix, and the
/// comparison is mathematical rather than rounded.
#[test]
fn relational_comparison_mixes_the_numeric_domains() {
    assert!(boolean("return BigInt(1)<2;"));
    assert!(boolean("return BigInt(2)>1;"));
    assert!(boolean("return BigInt(1)<=1;"));
    assert!(boolean("return BigInt(1)>=1;"));
    assert!(!boolean("return BigInt(2)<1;"));
    // A fractional Number resolves without rounding the BigInt.
    assert!(boolean("return BigInt(1)<1.5;"));
    assert!(!boolean("return BigInt(2)<1.5;"));
    assert!(boolean("return BigInt(2)>1.5;"));
    // A String operand is parsed as a BigInt literal.
    assert!(boolean("return BigInt(1)<'2';"));
    assert!(!boolean("return BigInt(3)<'2';"));
    // Infinities order against every finite BigInt.
    assert!(boolean("return BigInt(1)<Infinity;"));
    assert!(boolean("return BigInt(1)>-Infinity;"));
}

/// Oracle: `1n<NaN => [false]` and `1n>NaN => [false]`.
///
/// `NaN` is unordered, so every relational operator is `false` in both
/// directions.
#[test]
fn a_nan_operand_is_unordered_against_a_bigint() {
    for operator in ["<", "<=", ">", ">="] {
        assert!(
            !boolean(&format!("return BigInt(1){operator}NaN;")),
            "BigInt(1){operator}NaN"
        );
        assert!(
            !boolean(&format!("return NaN{operator}BigInt(1);")),
            "NaN{operator}BigInt(1)"
        );
    }
}

/// A `BigInt` beyond binary64's exact range still compares correctly, which is
/// what makes the comparison mathematical rather than a rounded conversion.
#[test]
fn comparison_stays_exact_beyond_the_binary64_integer_range() {
    // 2**53 + 1 is not representable as a Number, so a rounded comparison would
    // report equality here.
    assert!(boolean(
        "return BigInt('9007199254740993')>9007199254740992;"
    ));
    assert!(!boolean(
        "return BigInt('9007199254740993')<=9007199254740992;"
    ));
    // The Number literal `18446744073709551615` rounds up to exactly 2**64, so
    // the comparison is equality rather than greater-than. The oracle agrees:
    // `18446744073709551616n == 18446744073709551615` is `true`.
    assert!(boolean(
        "return BigInt('18446744073709551616')==18446744073709551615;"
    ));
    assert!(!boolean(
        "return BigInt('18446744073709551616')>18446744073709551615;"
    ));
}

/// Oracle: `1n==1 => [true]`, `0n==false => [true]`, `1n=='1' => [true]`,
/// `1n=='1.0' => [false]`, `1n==null => [false]`, `1n==undefined => [false]`,
/// `1n==1.5 => [false]`, `1n==NaN => [false]`.
///
/// Loose equality mixes the domains by mathematical value, unlike strict
/// equality.
#[test]
fn loose_equality_compares_across_the_domains_by_value() {
    assert!(boolean("return BigInt(1)==1;"));
    assert!(boolean("return 1==BigInt(1);"));
    assert!(boolean("return BigInt(0)==false;"));
    assert!(boolean("return BigInt(1)=='1';"));
    assert!(!boolean("return BigInt(1)=='1.0';"));
    assert!(!boolean("return BigInt(1)==null;"));
    assert!(!boolean("return BigInt(1)==undefined;"));
    assert!(!boolean("return BigInt(1)==1.5;"));
    assert!(!boolean("return BigInt(1)==NaN;"));
    assert!(boolean("return BigInt(1)!=2;"));
    // Exactness holds here too.
    assert!(!boolean(
        "return BigInt('9007199254740993')==9007199254740992;"
    ));
}

/// Oracle: `++ on bigint => [2]` and `-- on bigint => [0]`.
#[test]
fn increment_and_decrement_stay_in_the_bigint_domain() {
    assert_eq!(
        text("var x=BigInt(1);x++;return String(x)+'|'+typeof x;"),
        "2|bigint"
    );
    assert_eq!(text("var x=BigInt(1);x--;return String(x);"), "0");
    assert_eq!(text("var x=BigInt(1);++x;return String(x);"), "2");
    assert_eq!(text("var x=BigInt(1);--x;return String(x);"), "0");
    // The postfix form yields the original value.
    assert_eq!(
        text("var x=BigInt(1);var y=x++;return String(y)+','+String(x);"),
        "1,2"
    );
}

/// A `BigInt` literal executes rather than failing closed at installation.
///
/// The compiler emits `push_bigint_i32` for a literal that fits `i32`, mirroring
/// upstream's short-bigint fast path (`quickjs.c:26733-26737`).
#[test]
fn a_bigint_literal_produces_a_bigint_value() {
    assert_eq!(text("return typeof 1n;"), "bigint");
    assert_eq!(text("return String(1n);"), "1");
    assert_eq!(text("return String(-1n);"), "-1");
    assert_eq!(text("return String(0n);"), "0");
    assert_eq!(text("return String(0x10n);"), "16");
    assert!(boolean("return 1n===1n;"));
    assert!(!boolean("return 1n===1;"));
    assert_eq!(text("return String(2147483647n);"), "2147483647");
}

/// Literal operands reach the same operator paths as constructed values.
#[test]
fn bigint_literals_participate_in_the_operators() {
    assert_eq!(text("return String(1n+2n);"), "3");
    assert_eq!(text("return String(5n/2n);"), "2");
    assert_eq!(text("return String(2n**10n);"), "1024");
    assert_eq!(text("return String(~1n);"), "-2");
    assert!(boolean("return 1n<2;"));
    assert!(boolean("return 1n==1;"));
    assert_eq!(text("return (255n).toString(16);"), "ff");
    assert_eq!(text("return String(BigInt.asIntN(8,255n));"), "-1");
}
