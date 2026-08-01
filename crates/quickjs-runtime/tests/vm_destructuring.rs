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
    // The pinned QuickJS reference evaluates the member base before the
    // iterator step: `o` is read (order 2) before the iterator is created
    // (order 1), then the value is stored into `o.x`.
    assert_number(&result, 521);
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
