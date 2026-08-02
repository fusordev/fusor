use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionLimits, Function, JsNumber, JsString,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
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
        let body = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body),
        );
        with_dynamic_function_source(dynamic_source, FrontendLimits::default(), |unit, _| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("<runtime destructure>"))
                    .map_err(engine_failure)?;
            context
                .compile_dynamic_function_script(VerificationLimits::default())
                .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                .map_err(engine_failure)
        })
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

fn assert_number(value: &quickjs_runtime::JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

fn text(value: &quickjs_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8 result")
}

/// Runs `body` in a fresh runtime and renders its String completion.
fn run_text(body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(&mut context, body);
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("ordering result");
    text(&result)
}

#[test]
fn array_declaration_destructures_an_iterator_in_order() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [a, b, c] = [10, 20, 30];\
            return a * 100 + b * 10 + c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("array destructuring result");
    assert_number(&result, 1230);
}

#[test]
fn array_destructuring_uses_the_iterator_protocol_not_indexes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let iterable={\
                [Symbol.iterator](){\
                    let index=0;\
                    return {next(){index++;return index===1?{done:false,value:7}:(index===2?{done:false,value:8}:{done:true});}};\
                }\
            };\
            let [a, b] = iterable;\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("iterator destructuring result");
    assert_number(&result, 78);
}

#[test]
fn array_destructuring_assignment_overwrites_existing_bindings() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let b=0;\
            [a, b] = [3, 4];\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("destructuring assignment result");
    assert_number(&result, 34);
}

#[test]
fn array_destructuring_skips_elisions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let b=0;\
            [a, , b] = [1, 2, 3];\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("elision destructuring result");
    assert_number(&result, 13);
}

#[test]
fn array_destructuring_rest_collects_the_remaining_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [first, ...rest] = [1, 2, 3, 4];\
            return first * 100 + rest.length;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("rest destructuring result");
    assert_number(&result, 103);
}

#[test]
fn array_destructuring_defaults_replace_undefined_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let b=0;\
            [a, b = 9] = [1];\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("default destructuring result");
    assert_number(&result, 19);
}

#[test]
fn array_destructuring_rest_assignment_collects_the_remaining_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let first=0;let rest=null;\
            [first, ...rest] = [1, 2, 3, 4];\
            return first * 100 + rest.length;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("rest assignment result");
    assert_number(&result, 103);
}

#[test]
fn array_destructuring_rest_with_defaults_and_elisions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [a = 5, , ...rest] = [1];\
            return a * 1000 + rest.length;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("defaults and elisions with rest");
    // `a` reads the first value (1, so the default is not evaluated), the
    // elision consumes the exhausted iterator without an extra `next()`, and
    // the rest array is empty.
    assert_number(&result, 1000);
}

#[test]
fn array_destructuring_rest_uses_the_iterator_protocol() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let iterable={\
                [Symbol.iterator](){\
                    let index=0;\
                    return {next(){index++;return index<=3?{done:false,value:index*10}:{done:true};}};\
                }\
            };\
            let [first, ...rest] = iterable;\
            return first + rest[0] + rest[1] + rest.length;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("iterator rest destructuring result");
    assert_number(&result, 62);
}

#[test]
fn array_destructuring_rest_closes_an_early_exhausted_iterator_once() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let closed=0;\
            let iterable={\
                [Symbol.iterator](){\
                    return {next(){return {done:true};},return(){closed++;return {done:true};}};\
                }\
            };\
            let [...rest] = iterable;\
            return rest.length * 10 + closed;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("empty rest destructuring");
    // An exhausted iterator is not closed a second time; the rest array is
    // empty and `return` was never invoked.
    assert_number(&result, 0);
}

#[test]
fn array_destructuring_does_not_step_an_exhausted_iterator() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let calls=0;\
            let iterable={\
                [Symbol.iterator](){\
                    return {next(){calls++;return {done:true};}};\
                }\
            };\
            let [a, b, ...rest] = iterable;\
            return calls * 100 + (!a?10:0) + (!b?1:0) + rest.length;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("exhausted iterator destructuring");
    // Only the first `for_of_next` invokes `next()`; the element `b` and the
    // rest collector observe the pinned exhausted-record shortcut and bind
    // `undefined` / an empty array without further calls.
    assert_number(&result, 111);
}

#[test]
fn nested_array_patterns_destructure_an_inner_iterator() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [[a, b], c] = [[1, 2], 3];\
            return a * 100 + b * 10 + c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested array destructuring result");
    assert_number(&result, 123);
}

#[test]
fn nested_array_pattern_assignment_destructures_an_inner_iterator() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let b=0;let c=0;\
            [[a, b], c] = [[4, 5], 6];\
            return a * 100 + b * 10 + c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested array assignment result");
    assert_number(&result, 456);
}

#[test]
fn nested_array_pattern_with_default_and_rest() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [[a = 9, ...rest], b] = [[1], 2];\
            return a * 1000 + rest.length * 100 + b * 10;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested defaults and rest");
    // `a` reads the inner first value (1, so the default is not evaluated),
    // the inner rest array is empty, and `b` reads the outer second value.
    assert_number(&result, 1020);
}

#[test]
fn nested_array_pattern_default_applies_before_destructuring() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [a, [b]] = [1, [2]];\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested default value");
    assert_number(&result, 12);
}

#[test]
fn nested_array_pattern_defaults_evaluate_when_value_is_undefined() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let [[a] = [7]] = [];\
            return a;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested default on exhausted iterator");
    assert_number(&result, 7);
}

#[test]
fn nested_array_pattern_assignment_with_rest() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let rest=null;\
            [a, [...rest]] = [1, [2, 3]];\
            return a * 100 + rest[0] * 10 + rest[1];",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested rest assignment");
    assert_number(&result, 123);
}

#[test]
fn array_destructuring_static_member_targets_assign_through_the_reference() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:0,y:0};\
            [o.x, o.y] = [3, 4];\
            return o.x * 10 + o.y;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("static member destructuring result");
    assert_number(&result, 34);
}

#[test]
fn array_destructuring_computed_member_targets_assign_through_the_reference() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={};let keys=['a','b'];\
            [o[keys[0]], o[keys[1]]] = [7, 8];\
            return o.a * 10 + o.b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("computed member destructuring result");
    assert_number(&result, 78);
}

#[test]
fn array_destructuring_member_target_with_default() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:0};\
            [o.x = 9] = [];\
            return o.x;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("member default destructuring result");
    assert_number(&result, 9);
}

#[test]
fn array_destructuring_member_rest_target_collects_into_the_reference() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:null};\
            [o.x, ...o.rest] = [1, 2, 3];\
            return o.x * 100 + o.rest[0] * 10 + o.rest[1];",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("member rest destructuring result");
    assert_number(&result, 123);
}

#[test]
fn array_destructuring_computed_member_rest_target() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:null};let key='rest';\
            [o.x, ...o[key]] = [1, 2, 3];\
            return o.x * 100 + o.rest[0] * 10 + o.rest[1];",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("computed member rest result");
    assert_number(&result, 123);
}

#[test]
fn array_destructuring_member_targets_evaluate_bases_before_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let order=[];\
            let o={x:0};\
            let iterable={\
                [Symbol.iterator](){\
                    order[order.length]=2;\
                    let i=0;\
                    return {next(){i++;return i===1?{done:false,value:5}:{done:true};}};\
                }\
            };\
            [o.x] = iterable;\
            order[order.length]=1;\
            return o.x * 100 + order[0] * 10 + order[1];",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("member base ordering");
    // The iterator is acquired first (order 2 recorded inside
    // `[Symbol.iterator]`), then the value is stored into `o.x`, and the
    // trailing statement records order 1.
    assert_number(&result, 521);
}

/// Array-assignment member targets evaluate their base before the iterator
/// step, and after the iterator is acquired.
///
/// ECMAScript pins this order in `IteratorDestructuringAssignmentEvaluation`
/// for `AssignmentElement : DestructuringAssignmentTarget Initializer?`: the
/// target reference is evaluated first, then `IteratorStepValue` runs. Its note
/// states that "Left to right evaluation order is maintained by evaluating a
/// `DestructuringAssignmentTarget` that is not a destructuring pattern prior to
/// accessing the iterator or evaluating the `Initializer`." The pinned
/// `QuickJS` reference agrees (`get_lvalue` precedes `OP_for_of_next`,
/// `quickjs.c:26596-26612`), and both engines observe
/// `getIterator,base,next0`.
#[test]
fn array_assignment_member_bases_evaluate_before_the_iterator_step() {
    assert_eq!(
        run_text(
            "\
            let order='';\
            function base(){order+='base,';return {};}\
            let iterable={\
                [Symbol.iterator](){\
                    order+='getIterator,';\
                    let i=0;\
                    return {next(){order+='next'+i+',';i++;return i===1?{done:false,value:5}:{done:true};}};\
                }\
            };\
            [base().x] = iterable;\
            return order;",
        ),
        "getIterator,base,next0,"
    );
}

/// A computed member target evaluates its base, then its key, then steps the
/// iterator.
#[test]
fn array_assignment_computed_keys_evaluate_before_the_iterator_step() {
    assert_eq!(
        run_text(
            "\
            let order='';\
            function base(){order+='base,';return {};}\
            function key(){order+='key,';return 'k';}\
            let iterable={\
                [Symbol.iterator](){\
                    order+='getIterator,';\
                    let i=0;\
                    return {next(){order+='next'+i+',';i++;return i===1?{done:false,value:5}:{done:true};}};\
                }\
            };\
            [base()[key()]] = iterable;\
            return order;",
        ),
        "getIterator,base,key,next0,"
    );
}

/// A rest target's reference is evaluated before the remaining iterator values
/// are collected, exactly as `AssignmentRestElement` requires.
#[test]
fn array_assignment_rest_targets_evaluate_before_collecting_values() {
    assert_eq!(
        run_text(
            "\
            let order='';\
            function base(){order+='base,';return {};}\
            let iterable={\
                [Symbol.iterator](){\
                    order+='getIterator,';\
                    let i=0;\
                    return {next(){order+='next'+i+',';i++;return i===1?{done:false,value:5}:{done:true};}};\
                }\
            };\
            [...base().r] = iterable;\
            return order;",
        ),
        "getIterator,base,next0,next1,"
    );
}

/// Multiple member targets interleave each base evaluation with its own step.
#[test]
fn array_assignment_member_bases_interleave_with_each_iterator_step() {
    assert_eq!(
        run_text(
            "\
            let order='';\
            let o={};\
            function base(tag){order+='base'+tag+',';return o;}\
            let iterable={\
                [Symbol.iterator](){\
                    order+='getIterator,';\
                    let i=0;\
                    return {next(){i++;order+='next'+i+',';return i<=2?{done:false,value:i}:{done:true};}};\
                }\
            };\
            [base(1).x, base(2).y] = iterable;\
            return order+o.x+o.y;",
        ),
        "getIterator,base1,next1,base2,next2,12"
    );
}

#[test]
fn captured_cells_coexist_with_realm_global_references() {
    // Regression: a function whose nested closure captures a local cell and
    // that also references the realm-global `undefined` name used to break
    // the captured cell's environment wiring.
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let calls=0;\
            let iterable={\
                [Symbol.iterator](){\
                    return {next(){calls++;return {done:true};}};\
                }\
            };\
            let [a, b] = iterable;\
            return calls + (calls===undefined?1:0) + (a===undefined?10:0) + (b===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("realm-global and capture result");
    // `next()` runs once, `undefined` resolves through the global object,
    // and the exhausted destructured bindings are `undefined`.
    assert_number(&result, 12);
}

#[test]
fn global_undefined_nan_infinity_resolve_as_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            return (undefined===undefined?1:0) + (NaN!==NaN?1:0) + (Infinity>1e308?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("global value properties");
    assert_number(&result, 3);
}

#[test]
fn object_declaration_destructures_named_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {a, b} = {a: 10, b: 20};\
            return a * 100 + b * 10;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object destructuring result");
    assert_number(&result, 1200);
}

#[test]
fn object_declaration_destructures_renamed_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {a: x, b: y} = {a: 3, b: 4};\
            return x * 10 + y;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("renamed object destructuring");
    assert_number(&result, 34);
}

#[test]
fn object_declaration_defaults_replace_undefined_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {a = 5, b = 9} = {a: 1};\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object default result");
    assert_number(&result, 19);
}

#[test]
fn object_declaration_computed_keys_resolve_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let key='a';\
            let {[key]: x, ['b']: y} = {a: 7, b: 8};\
            return x * 10 + y;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("computed object key result");
    assert_number(&result, 78);
}

#[test]
fn object_declaration_nested_patterns() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {a: [x, y], b: {c}} = {a: [1, 2], b: {c: 3}};\
            return x * 100 + y * 10 + c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("nested object destructuring");
    assert_number(&result, 123);
}

#[test]
fn object_assignment_destructures_named_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let b=0;\
            ({a, b} = {a: 5, b: 6});\
            return a * 10 + b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object assignment result");
    assert_number(&result, 56);
}

#[test]
fn object_assignment_with_defaults_and_renames() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let x=0;let y=0;\
            ({a: x, b: y = 9} = {a: 1});\
            return x * 10 + y;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("renamed object assignment");
    assert_number(&result, 19);
}

#[test]
fn object_destructuring_boxes_primitive_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {length} = 'abc';\
            let {x} = 5;\
            return length * 10 + (x===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("primitive object destructuring");
    // `'abc'` boxes to a String wrapper whose `length` reads 3; `5` boxes to
    // a Number wrapper with no own `x`.
    assert_number(&result, 31);
}

#[test]
fn object_destructuring_invokes_getters() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let calls=0;\
            let obj={\
                get a(){calls++;return 42;}\
            };\
            let {a} = obj;\
            return a * 100 + calls;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("getter object destructuring");
    assert_number(&result, 4201);
}

#[test]
fn object_destructuring_rejects_null_and_undefined() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let caught=0;\
            try{let {a} = null;}catch(e){caught = e.message==='cannot convert to object'?1:0;}\
            try{let {a} = undefined;}catch(e){caught += e.message==='cannot convert to object'?2:0;}\
            return caught;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("null and undefined object destructuring");
    assert_number(&result, 3);
}

#[test]
fn object_assignment_expression_value_is_the_rhs() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;\
            let result = ({a} = {a: 7});\
            return result.a * 10 + a;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object assignment expression value");
    assert_number(&result, 77);
}

#[test]
fn object_assignment_static_member_targets_assign_through_the_reference() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:0,y:0};\
            ({x: o.x, y: o.y} = {x: 3, y: 4});\
            return o.x * 10 + o.y;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object static member destructuring result");
    assert_number(&result, 34);
}

#[test]
fn object_assignment_computed_member_targets_assign_through_the_reference() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={};let keys=['a','b'];\
            ({a: o[keys[0]], b: o[keys[1]]} = {a: 7, b: 8});\
            return o.a * 10 + o.b;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object computed member destructuring result");
    assert_number(&result, 78);
}

#[test]
fn object_assignment_member_target_with_default() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:0};\
            ({x: o.x = 9} = {});\
            return o.x;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object member default destructuring result");
    assert_number(&result, 9);
}

#[test]
fn object_assignment_member_targets_evaluate_bases_after_reading() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let order=[];\
            let o={x:0};\
            let src={get a(){order[order.length]=1;return 5;}};\
            function base(){order[order.length]=2;return o;};\
            ({a: base().x} = src);\
            return o.x * 100 + order[0] * 10 + order[1];",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object member base ordering");
    // The pinned QuickJS order reads the property from the source (the
    // getter records 1) before evaluating the member base (records 2), then
    // stores into the reference; the array-pattern counterpart evaluates
    // the base before the step instead.
    assert_number(&result, 512);
}

#[test]
fn object_assignment_nested_patterns_with_member_targets() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={x:0};\
            ({a: {b: o.x}} = {a: {b: 4}});\
            return o.x;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object nested member destructuring");
    assert_number(&result, 4);
}

#[test]
fn object_assignment_rest_collects_remaining_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let rest;\
            let result = ({a, ...rest} = {a: 1, b: 2, c: 3});\
            return a * 1000 + rest.b * 100 + rest.c * 10 + (rest.a===undefined?1:0) + result.b * 10000;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest assignment result");
    assert_number(&result, 21231);
}

#[test]
fn object_assignment_rest_excludes_destructured_keys_including_computed() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let key='a';let x=0;let b=0;let rest;\
            ({[key]: x, b, ...rest} = {a: 1, b: 2, c: 3});\
            return x * 100 + b * 10 + rest.c + (rest.a===undefined?1:0) + (rest.b===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("computed object rest assignment result");
    assert_number(&result, 125);
}

#[test]
fn object_assignment_rest_into_a_member_target() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let o={rest:null};\
            ({a, ...o.rest} = {a: 1, b: 2, c: 3});\
            return a * 1000 + o.rest.b * 100 + o.rest.c * 10 + (o.rest.a===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest member target result");
    assert_number(&result, 1231);
}

#[test]
fn object_assignment_rest_with_defaults_and_nested_patterns() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let x=0;let y=0;let rest;\
            ({a = 9, b: [x, y], ...rest} = {b: [1, 2], c: 3});\
            return a * 1000 + x * 100 + y * 10 + rest.c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest assignment with defaults and nested");
    assert_number(&result, 9123);
}

#[test]
fn object_pattern_as_an_array_assignment_element() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let a=0;let c=0;\
            [a, {b: c}] = [1, {b: 2}];\
            return a * 10 + c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object-in-array destructuring");
    assert_number(&result, 12);
}

#[test]
fn object_rest_collects_remaining_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {a, ...rest} = {a: 1, b: 2, c: 3};\
            return a * 1000 + rest.b * 100 + rest.c * 10 + (rest.a===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest result");
    assert_number(&result, 1231);
}

#[test]
fn object_rest_excludes_destructured_keys_including_computed() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let key='a';\
            let {[key]: x, ...rest} = {a: 1, b: 2, c: 3};\
            return x * 100 + rest.b * 10 + rest.c + (rest.a===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("computed object rest result");
    assert_number(&result, 124);
}

#[test]
fn object_rest_skips_non_enumerable_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let obj={a: 1};\
            let {...rest} = obj;\
            return rest.a * 10 + (rest.b===undefined?1:0);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest enumerable");
    assert_number(&result, 11);
}

#[test]
fn object_rest_copies_getter_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let calls=0;\
            let obj={\
                get a(){calls++;return 42;}\
            };\
            let {...rest} = obj;\
            return rest.a * 100 + calls;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest getter");
    assert_number(&result, 4201);
}

#[test]
fn object_rest_copies_enumerable_symbol_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let symbol=Symbol('rest');let object={a:1};object[symbol]=2;\
            let {...rest}=object;\
            return rest.a+rest[symbol]*10+Object.getOwnPropertySymbols(rest).length*100;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest Symbol result");
    assert_number(&result, 121);
}

#[test]
fn object_rest_rechecks_snapshotted_descriptors_after_getters() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let object={};\
            function first(){\
                Object.defineProperty(object,'hidden',{enumerable:true});\
                delete object.deleted;object.added=4;return 1;\
            }\
            Object.defineProperty(object,'a',{get:first,enumerable:true});\
            Object.defineProperty(object,'hidden',{value:2,configurable:true});\
            object.deleted=3;let {...rest}=object;\
            return rest.a*100+rest.hidden*10+\
                (Object.hasOwn(rest,'deleted')?0:1)+\
                (Object.hasOwn(rest,'added')?0:2);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest descriptor mutation result");
    assert_number(&result, 123);
}

#[test]
fn object_rest_with_defaults_and_nested_patterns() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {a = 9, b: [x, y], ...rest} = {b: [1, 2], c: 3};\
            return a * 1000 + x * 100 + y * 10 + rest.c;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("object rest with defaults and nested");
    assert_number(&result, 9123);
}

#[test]
fn object_rest_on_primitive_string_boxes_indices() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let {...rest} = 'ab';\
            return rest[0] === 'a' && rest[1] === 'b' && rest.length === undefined ? 1 : 0;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("string object rest");
    // The String wrapper exposes indices 0 ('a') and 1 ('b') as own
    // enumerable string properties; the non-enumerable `length` is not
    // copied, so the rest object has no own `length`. The copied values are
    // the string code units themselves, so arithmetic like `rest[0] * 100`
    // coerces them to NaN exactly as it does in QuickJS.
    assert_number(&result, 1);
}

#[test]
fn array_destructuring_step_failures_do_not_call_return() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let closed=false;\
            let iterable={\
                [Symbol.iterator](){\
                    return {next(){throw new Error('boom');},return(){closed=true;return {done:true};}};\
                }\
            };\
            try{let [a] = iterable;}catch(e){return e.message==='boom'&&!closed;}",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("abrupt destructuring");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));
}
