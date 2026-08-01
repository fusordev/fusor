use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    DynamicFunctionCompileFailure, ExecutionLimits, JsString, JsValue,
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
        let parameter_text = source
            .parameters()
            .iter()
            .map(JsString::to_utf8_lossy)
            .collect::<Result<Vec<_>, _>>()
            .map_err(engine_failure)?;
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let parameters = parameter_text
            .iter()
            .map(|parameter| SourceFragment::new(parameter.as_str()))
            .collect::<Vec<_>>();
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(unit, Arc::from("<bind>"))
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

fn call<T>(body: &str, inspect: impl FnOnce(&JsValue) -> T) -> T {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function");
    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("bind operation");
    inspect(&value)
}

/// Runs `body` and renders any caught exception as `name:message`.
fn caught(body: &str) -> String {
    let wrapped =
        format!("try {{ {body} }} catch (error) {{ return error.name + \":\" + error.message; }}");
    call(&wrapped, |value| {
        value
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn boolean(value: &JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn string(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8 result")
}

#[test]
fn bind_metadata_matches_the_pinned_function_prototype_graph() {
    let result = call(
        "let b = Function.prototype.bind;\n\
         let listed = false;\n\
         for (let k in Function.prototype) if (k === \"bind\") listed = true;\n\
         return b.name + \":\" + b.length + \":\" + listed;",
        string,
    );
    assert_eq!(result, "bind:1:false");
}

#[test]
fn bind_is_writable_and_not_constructable() {
    let result = call(
        "let b = Function.prototype.bind;\n\
         Function.prototype.bind = 1;\n\
         let writable = Function.prototype.bind === 1;\n\
         Function.prototype.bind = b;\n\
         try { new b(); } catch (error) { return writable + \":\" + error.name + \":\" + error.message; }",
        string,
    );
    assert_eq!(result, "true:TypeError:bind is not a constructor");
}

#[test]
fn bind_target_must_be_callable() {
    assert_eq!(
        caught("return Function.prototype.bind.call({}, null);"),
        "TypeError:not a function"
    );
}

#[test]
fn bound_function_metadata_uses_the_exact_quickjs_length_rules() {
    assert_eq!(
        call(
            "function f(a, b, c) {}\n\
             let b = f.bind(null);\n\
             return b.name + \":\" + b.length;",
            string,
        ),
        "bound f:3"
    );
    assert_eq!(
        call(
            "function f(a, b, c) {}\n\
             return f.bind(null, 1).length + \":\" + f.bind(null, 1, 2, 3).length;",
            string,
        ),
        "2:0"
    );
    assert_eq!(
        call(
            "function f(a, b) {}\n\
             let inner = f.bind(null, 1);\n\
             return inner.length + \":\" + inner.bind(null).length;",
            string,
        ),
        "1:1"
    );
    assert_eq!(
        call(
            "let apply = Function.prototype.apply;\n\
             return apply.bind(null).length + \":\" + apply.bind(null, 1).length;",
            string,
        ),
        "2:1"
    );
}

#[test]
fn bound_function_name_is_bound_prefix_without_conversion() {
    assert_eq!(
        call(
            "function f() {}\n\
             return f.bind(null).bind(null).name;",
            string,
        ),
        "bound bound f"
    );
}

#[test]
fn bound_call_overrides_receiver_and_prepends_arguments() {
    assert_eq!(
        call(
            "function f(a, b) { return this.tag + \":\" + a + \":\" + b; }\n\
             return f.bind({tag: \"bound\"}, 1)(2);",
            string,
        ),
        "bound:1:2"
    );
    assert_eq!(
        call(
            "function f() { return this.tag; }\n\
             let receiver = { tag: \"bound\" };\n\
             return f.bind(receiver).call({ tag: \"other\" });",
            string,
        ),
        "bound"
    );
}

#[test]
fn bound_function_works_through_call_and_apply() {
    assert_eq!(
        call(
            "function f(a, b) { return this.tag + \":\" + a + \":\" + b; }\n\
             let bound = f.bind({tag: \"b\"}, 1);\n\
             return bound.call({tag: \"other\"}, 2) + \"|\" + bound.apply({tag: \"other\"}, [2]);",
            string,
        ),
        "b:1:2|b:1:2"
    );
}

#[test]
fn bound_native_targets_keep_native_dispatch() {
    assert_eq!(
        call(
            "function f(a, b) { return a + \":\" + b; }\n\
             let apply = Function.prototype.apply.bind(f);\n\
             return apply(null, [\"x\", \"y\"]);",
            string,
        ),
        "x:y"
    );
    assert_eq!(
        call(
            "function f(a, b) { return \"\" + (a + b); }\n\
             return Function.prototype.call.bind(f)(null, 3, 4);",
            string,
        ),
        "7"
    );
}

#[test]
fn bound_construction_uses_bound_args_and_original_new_target() {
    assert_eq!(
        call(
            "function C(a, b) { this.sum = a + b; }\n\
             return \"\" + new (C.bind(null, 2))(3).sum;",
            string,
        ),
        "5"
    );
    assert_eq!(
        call(
            "function C() { this.tag = \"c\"; }\n\
             let bound = C.bind(null);\n\
             return \"\" + ((new bound()) instanceof C);",
            string,
        ),
        "true"
    );
}

#[test]
fn bound_nonconstructor_construction_uses_the_bound_name() {
    assert_eq!(
        caught("return new (Function.prototype.apply.bind(null))();"),
        "TypeError:bound apply is not a constructor"
    );
}

#[test]
fn bound_function_is_not_constructable_when_target_is_not() {
    assert_eq!(
        caught("let f = {m(){}}.m;\nreturn new (f.bind(null))();"),
        "TypeError:bound m is not a constructor"
    );
}

#[test]
fn instanceof_uses_the_ordinary_prototype_chain() {
    assert_eq!(
        call(
            "function C() {}\n\
             let c = new C();\n\
             return (c instanceof C) + \":\" + ({} instanceof C) + \":\" + (1 instanceof C) + \":\" + (null instanceof C);",
            string,
        ),
        "true:false:false:false"
    );
    assert_eq!(
        call(
            "function A() {}\n\
             function B() {}\n\
             B.prototype = new A();\n\
             return (new B() instanceof A) + \":\" + (new A() instanceof B);",
            string,
        ),
        "true:false"
    );
}

#[test]
fn instanceof_rejects_non_object_right_operands() {
    assert_eq!(
        caught("return {} instanceof 1;"),
        "TypeError:invalid 'instanceof' right operand"
    );
    assert_eq!(
        caught("return {} instanceof {};"),
        "TypeError:invalid 'instanceof' right operand"
    );
}

#[test]
fn instanceof_consults_a_custom_symbol_has_instance_method() {
    assert!(call(
        "function C() {}\n\
         let guard = { [Symbol.hasInstance](v) { return v > 2; } };\n\
         C[Symbol.hasInstance] = guard[Symbol.hasInstance];\n\
         return 3 instanceof C;",
        boolean,
    ));
    assert!(!call(
        "function C() {}\n\
         let guard = { [Symbol.hasInstance](v) { return v > 2; } };\n\
         C[Symbol.hasInstance] = guard[Symbol.hasInstance];\n\
         return 1 instanceof C;",
        boolean,
    ));
}

#[test]
fn instanceof_uses_symbol_has_instance_on_plain_objects() {
    assert!(call(
        "let guard = { [Symbol.hasInstance](v) { return v === 7; } };\n\
         return 7 instanceof guard;",
        boolean,
    ));
    assert!(!call(
        "let guard = { [Symbol.hasInstance](v) { return v === 7; } };\n\
         return 8 instanceof guard;",
        boolean,
    ));
}

#[test]
fn function_prototype_symbol_has_instance_runs_the_ordinary_path() {
    assert!(!call(
        "return Function.prototype[Symbol.hasInstance].call(1, {});",
        boolean,
    ));
    assert!(call(
        "function C() {}\n\
         return Function.prototype[Symbol.hasInstance].call(C, new C());",
        boolean,
    ));
    assert!(!call(
        "function C() {}\n\
         return Function.prototype[Symbol.hasInstance].call(C, {});",
        boolean,
    ));
}

#[test]
fn instanceof_unwraps_bound_functions() {
    assert!(call(
        "function C() {}\n\
         return new C() instanceof C.bind(null);",
        boolean,
    ));
    assert!(!call(
        "function C() {}\n\
         return {} instanceof C.bind(null);",
        boolean,
    ));
}

#[test]
fn bound_functions_are_typeof_function_and_render_native_source() {
    assert_eq!(
        call(
            "function f() {}\n\
             let bound = f.bind(null);\n\
             return typeof bound;",
            string,
        ),
        "function"
    );
    assert!(call(
        "function f() {}\n\
         let bound = f.bind({tag: 1});\n\
         return bound.toString().length === \"function bound f() {\\n    [native code]\\n}\".length;",
        boolean,
    ));
}
