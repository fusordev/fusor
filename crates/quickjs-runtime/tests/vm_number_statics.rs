//! Global numeric functions, `Number` statics, and `Array.isArray`, pinned to
//! the specification.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log(Number.MAX_VALUE,\
//!     Number.MIN_VALUE, Number.EPSILON, Number.MAX_SAFE_INTEGER);'
//! 1.7976931348623157e+308 5e-324 2.220446049250313e-16 9007199254740991
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Number.MAX_VALUE => 1.7976931348623157e+308  bits=0x7fefffffffffffff
//! Number.MIN_VALUE => 5e-324                   bits=0x1
//! Number.EPSILON => 2.220446049250313e-16      bits=0x3cb0000000000000
//! Number.MAX_SAFE_INTEGER => 9007199254740991
//! Number.MIN_SAFE_INTEGER => -9007199254740991
//! Number.POSITIVE_INFINITY => Infinity   Number.NEGATIVE_INFINITY => -Infinity
//! Number.NaN => NaN
//! descriptor of Number.MAX_VALUE => {w:false, e:false, c:false}
//! descriptor of Number.isInteger => {w:true, e:false, c:true}
//! isInteger(1) => true      isInteger(1.5) => false    isInteger(NaN) => false
//! isInteger(Infinity) => false                         isInteger(-0) => true
//! isInteger(2**53) => true  isSafeInteger(2**53) => false
//! isSafeInteger(9007199254740991) => true
//! isFinite(1) => true       isFinite(NaN) => false     isFinite(Infinity) => false
//! isNaN(NaN) => true        isNaN(1) => false
//! every predicate answers false for '1', true, null, and undefined
//! isInteger.length => 1   isNaN.name => "isNaN"
//! Array.isArray([]) => true   Array.isArray({}) => false
//! Array.isArray.length => 1
//! ```

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsString,
    JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime,
    RuntimeLimits,
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
                    Arc::from("<runtime Number statics>"),
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

/// Asserts a table of `expression => rendered result` pairs.
fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

#[test]
fn global_this_is_the_mutable_realm_global_binding() {
    assert_all(&[
        ("globalThis.globalThis===globalThis", "true"),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(globalThis,'globalThis');return d.writable+'|'+d.enumerable+'|'+d.configurable})()",
            "true|false|true",
        ),
        (
            "(function(){const replacement={};globalThis.globalThis=replacement;return globalThis===replacement})()",
            "true",
        ),
        ("Reflect.deleteProperty(globalThis,'globalThis')", "true"),
    ]);
}

/// The `Number` value statics carry the exact pinned binary64 values.
#[test]
fn the_number_value_statics_are_exact() {
    assert_all(&[
        ("Number.MAX_VALUE", "1.7976931348623157e+308"),
        ("Number.MIN_VALUE", "5e-324"),
        ("Number.EPSILON", "2.220446049250313e-16"),
        ("Number.MAX_SAFE_INTEGER", "9007199254740991"),
        ("Number.MIN_SAFE_INTEGER", "-9007199254740991"),
        ("Number.POSITIVE_INFINITY", "Infinity"),
        ("Number.NEGATIVE_INFINITY", "-Infinity"),
        ("Number.NaN", "NaN"),
        // The boundary is exact: `MAX_SAFE_INTEGER + 1` and `+ 2` collapse onto
        // the same binary64 value, which is what makes the former unsafe.
        (
            "Number.MAX_SAFE_INTEGER + 1 === Number.MAX_SAFE_INTEGER + 2",
            "true",
        ),
        ("1 + Number.EPSILON > 1", "true"),
        ("Number.MIN_VALUE / 2", "0"),
        ("Number.MAX_VALUE * 2", "Infinity"),
    ]);
}

/// The value statics are frozen, while the predicates are ordinary methods.
#[test]
fn the_number_statics_have_the_pinned_descriptors() {
    assert_all(&[
        (
            "Object.getOwnPropertyDescriptor(Number,'MAX_VALUE').writable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number,'MAX_VALUE').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number,'MAX_VALUE').configurable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number,'isInteger').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number,'isInteger').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number,'isInteger').configurable",
            "true",
        ),
        ("Number.isInteger.length", "1"),
        ("Number.isInteger.name", "isInteger"),
        ("Number.isSafeInteger.name", "isSafeInteger"),
        ("Number.isFinite.name", "isFinite"),
        ("Number.isNaN.name", "isNaN"),
    ]);
}

/// `Number.isInteger` and `Number.isSafeInteger` differ at the exact-integer
/// boundary.
///
/// `2**53` is an integer but not a safe one, because binary64 can no longer
/// distinguish it from its successor.
#[test]
fn the_integer_predicates_differ_at_the_safe_boundary() {
    assert_all(&[
        ("Number.isInteger(1)", "true"),
        ("Number.isInteger(0)", "true"),
        ("Number.isInteger(-0)", "true"),
        ("Number.isInteger(-1)", "true"),
        ("Number.isInteger(1.5)", "false"),
        ("Number.isInteger(NaN)", "false"),
        ("Number.isInteger(Infinity)", "false"),
        ("Number.isInteger(-Infinity)", "false"),
        ("Number.isInteger(9007199254740991)", "true"),
        // `2**53` is still an exact integer.
        ("Number.isInteger(9007199254740992)", "true"),
        ("Number.isSafeInteger(9007199254740991)", "true"),
        // ...but not a safe one.
        ("Number.isSafeInteger(9007199254740992)", "false"),
        ("Number.isSafeInteger(-9007199254740991)", "true"),
        ("Number.isSafeInteger(1.5)", "false"),
        ("Number.isSafeInteger(Infinity)", "false"),
        ("Number.isSafeInteger(NaN)", "false"),
    ]);
}

/// `Number.isFinite` and `Number.isNaN` answer only about Numbers.
#[test]
fn the_finiteness_predicates_match_the_oracle() {
    assert_all(&[
        ("Number.isFinite(1)", "true"),
        ("Number.isFinite(1.5)", "true"),
        ("Number.isFinite(-0)", "true"),
        ("Number.isFinite(NaN)", "false"),
        ("Number.isFinite(Infinity)", "false"),
        ("Number.isFinite(-Infinity)", "false"),
        ("Number.isNaN(NaN)", "true"),
        ("Number.isNaN(1)", "false"),
        ("Number.isNaN(Infinity)", "false"),
    ]);
}

/// The predicates never convert their argument.
///
/// This is what separates `Number.isNaN` from the global `isNaN`: a String that
/// would convert to a Number still answers `false`.
#[test]
fn the_predicates_never_convert_their_argument() {
    for predicate in ["isInteger", "isSafeInteger", "isFinite", "isNaN"] {
        for argument in [
            "'1'",
            "'NaN'",
            "true",
            "false",
            "null",
            "undefined",
            "{}",
            "{valueOf(){return 1;}}",
        ] {
            assert_eq!(
                rendered(&format!("Number.{predicate}({argument})")),
                "false",
                "Number.{predicate}({argument})"
            );
        }
        // An absent argument is `undefined`, so it answers `false` too.
        assert_eq!(
            rendered(&format!("Number.{predicate}()")),
            "false",
            "Number.{predicate}()"
        );
    }
}

/// The global predicates perform `ToNumber`, unlike their `Number` statics.
#[test]
fn the_global_predicates_coerce_and_propagate_abrupt_completions() {
    assert_all(&[
        ("isFinite(1)", "true"),
        ("isFinite('1')", "true"),
        ("isFinite('x')", "false"),
        ("isFinite(null)", "true"),
        ("isFinite(undefined)", "false"),
        ("isFinite(Infinity)", "false"),
        ("isNaN(NaN)", "true"),
        ("isNaN('x')", "true"),
        ("isNaN('1')", "false"),
        ("isNaN(null)", "false"),
        ("isNaN(undefined)", "true"),
        (
            "(function(){let log='';const value={valueOf(){log+='v';return '2';},toString(){log+='s';return '3';}};return isFinite(value)+'|'+log;})()",
            "true|v",
        ),
        (
            "(function(){try{isNaN({valueOf(){throw 41;}});}catch(error){return error===41;}})()",
            "true",
        ),
        (
            "(function(){try{isFinite(BigInt(1));}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
        (
            "(function(){try{isNaN(Symbol('x'));}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

/// `parseFloat` consumes the longest `StrDecimalLiteral` prefix after
/// `ToString` and leading-whitespace removal.
#[test]
fn global_parse_float_uses_the_specification_prefix_grammar() {
    assert_all(&[
        ("parseFloat('  -1.25e2tail')", "-125"),
        ("parseFloat('1e')", "1"),
        ("parseFloat('1e+')", "1"),
        ("parseFloat('.5x')", "0.5"),
        ("parseFloat('+Infinitytail')", "Infinity"),
        ("parseFloat('-Infinityx')", "-Infinity"),
        ("String(parseFloat('.'))", "NaN"),
        ("Object.is(parseFloat('-0x'),-0)", "true"),
        ("parseFloat('0x10')", "0"),
        ("parseFloat('1\\ud8002')", "1"),
        ("parseFloat(BigInt(10))", "10"),
        (
            "(function(){let log='';const value={toString(){log+='s';return '2.5x';},valueOf(){log+='v';return 3;}};return parseFloat(value)+'|'+log;})()",
            "2.5|s",
        ),
        (
            "(function(){try{parseFloat(Symbol('x'));}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

/// `parseInt` converts its input before its radix, applies `ToInt32` to the
/// radix, and rounds accepted integer prefixes to binary64.
#[test]
fn global_parse_int_preserves_conversion_order_and_radix_semantics() {
    assert_all(&[
        ("parseInt('  -0xFzz')", "-15"),
        ("parseInt('0x10')", "16"),
        ("parseInt('0x10',16)", "16"),
        ("parseInt('0x10',10)", "0"),
        ("parseInt('08')", "8"),
        ("parseInt('11',2)", "3"),
        ("parseInt('z',36)", "35"),
        ("String(parseInt('1',1))", "NaN"),
        ("String(parseInt('1',37))", "NaN"),
        ("Object.is(parseInt('-0',10),-0)", "true"),
        ("parseInt('10',4294967298)", "2"),
        ("parseInt('900719925474099267',10)", "900719925474099300"),
        ("parseInt('ffffffffffffffff',16)", "18446744073709552000"),
        ("parseInt(BigInt(10),10)", "10"),
        (
            "(function(){let log='';const input={toString(){log+='s';return '10';}};const radix={valueOf(){log+='r';return 2;}};return parseInt(input,radix)+'|'+log;})()",
            "2|sr",
        ),
        (
            "(function(){let touched=false;try{parseInt({toString(){throw 41;}},{valueOf(){touched=true;return 2;}});}catch(error){return error===41&&!touched;}})()",
            "true",
        ),
        (
            "(function(){try{parseInt('10',Symbol('x'));}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

/// The parser statics are aliases of the corresponding realm-global function
/// identities, and every installed property carries the ordinary built-in
/// descriptor.
#[test]
fn global_numeric_function_identities_and_descriptors_are_exact() {
    assert_all(&[
        ("isFinite.name+','+isFinite.length", "isFinite,1"),
        ("isNaN.name+','+isNaN.length", "isNaN,1"),
        ("parseFloat.name+','+parseFloat.length", "parseFloat,1"),
        ("parseInt.name+','+parseInt.length", "parseInt,2"),
        ("Number.parseFloat===parseFloat", "true"),
        ("Number.parseInt===parseInt", "true"),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(this,'parseInt');return d.value===parseInt&&d.writable&&!d.enumerable&&d.configurable;})()",
            "true",
        ),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(Number,'parseFloat');return d.value===parseFloat&&d.writable&&!d.enumerable&&d.configurable;})()",
            "true",
        ),
    ]);
}

/// Prefix scans debit the shared execution budget in proportion to their
/// UTF-16 input instead of hiding unbounded native work behind one call.
#[test]
fn numeric_prefix_scans_consume_instruction_fuel() {
    for parser in ["parseFloat", "parseInt"] {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let input = "1".repeat(1_000);
        let body = format!("return {parser}('{input}');");
        let run = dynamic_function(&mut context, &body);
        let result = context.call(
            &run,
            &[],
            ExecutionLimits::default().with_instruction_fuel(100),
        );
        assert!(
            matches!(
                result,
                Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
            ),
            "{parser} must charge its input scan"
        );
    }
}

/// `Array.isArray` recognizes only a real Array.
#[test]
fn array_is_array_recognizes_only_an_array() {
    assert_all(&[
        ("Array.isArray([])", "true"),
        ("Array.isArray([1,2,3])", "true"),
        ("Array.isArray(new Array(3))", "true"),
        ("Array.isArray({})", "false"),
        ("Array.isArray('abc')", "false"),
        ("Array.isArray(1)", "false"),
        ("Array.isArray(null)", "false"),
        ("Array.isArray(undefined)", "false"),
        ("Array.isArray()", "false"),
        // An array-like is not an Array.
        ("Array.isArray({length:0})", "false"),
        // Neither is `Array.prototype`'s own constructor.
        ("Array.isArray(Array)", "false"),
        ("Array.isArray.length", "1"),
        ("Array.isArray.name", "isArray"),
    ]);
}
