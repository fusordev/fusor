//! Strict-function unmapped arguments objects, pinned to ECMA-262.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, JsNumber, JsString, JsValue,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
    RuntimeResource,
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
                    Arc::from("<runtime arguments>"),
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

fn evaluate<T>(body: &str, project: impl FnOnce(JsValue) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    let function = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("completed");
    project(result)
}

fn string_result(body: &str) -> String {
    evaluate(body, |value| {
        value
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

#[test]
fn strict_arguments_preserve_the_complete_supplied_list() {
    evaluate(
        "function inspect(a){'use strict';return arguments.length*1000+arguments[0]*100+arguments[1]*10+arguments[2];}return inspect(1,2,3);",
        |result| {
            assert!(
                result
                    .as_number()
                    .expect("live value")
                    .expect("number")
                    .strict_equals(JsNumber::from_i32(3123))
            );
        },
    );
}

#[test]
fn a_strict_dynamic_function_owns_its_arguments_binding() {
    evaluate("'use strict';return arguments.length;", |result| {
        assert!(
            result
                .as_number()
                .expect("live value")
                .expect("number")
                .strict_equals(JsNumber::from_i32(0))
        );
    });
}

#[test]
fn strict_arguments_are_unmapped_and_have_the_specification_properties() {
    let result = string_result(
        "function inspect(a){'use strict';\
            const d=Object.getOwnPropertyDescriptor(arguments,'length');\
            const c=Object.getOwnPropertyDescriptor(arguments,'callee');\
            const i=Object.getOwnPropertyDescriptor(arguments,Symbol.iterator);\
            arguments[0]=7;const original=a;a=9;\
            let throws=false;try{arguments.callee;}catch(e){throws=e instanceof TypeError;}\
            return Object.prototype.toString.call(arguments)+'|'+\
                d.writable+d.enumerable+d.configurable+'|'+\
                (c.get===c.set)+c.enumerable+c.configurable+'|'+\
                (i.value===Array.prototype.values)+i.writable+i.enumerable+i.configurable+'|'+\
                original+'|'+arguments[0]+'|'+a+'|'+throws;}return inspect(3);",
    );
    assert_eq!(
        result,
        "[object Arguments]|truefalsetrue|truefalsefalse|truetruefalsetrue|3|7|9|true"
    );
}

#[test]
fn strict_arguments_use_the_generic_array_values_iterator() {
    let result = string_result(
        "function inspect(){'use strict';const iterator=arguments[Symbol.iterator]();\
            const first=iterator.next();const second=iterator.next();const end=iterator.next();\
            return first.value+'|'+first.done+'|'+second.value+'|'+second.done+'|'+end.done;}\
            return inspect(4,8);",
    );
    assert_eq!(result, "4|false|8|false|true");
}

#[test]
fn arguments_property_limit_failure_is_atomic() {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8("'use strict';return arguments.length;").expect("body"),
        ))
        .expect("dynamic Function authority");

    let property_limit = {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("probe runtime");
        let realm = runtime.create_realm().expect("probe realm");
        let mut context = runtime.context(&realm).expect("probe context");
        let function = context
            .execute_dynamic_function_script(Arc::clone(&authority), ExecutionLimits::default())
            .expect("probe dynamic Function")
            .into_function()
            .expect("probe function");
        let limit = context.runtime_usage().object_properties();
        drop(function);
        limit
    };

    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_object_properties(property_limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function")
        .into_function()
        .expect("function");
    let before = context.runtime_usage();
    assert!(matches!(
        context.call(&function, &[], ExecutionLimits::default()),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == property_limit && observed == property_limit + 3
    ));
    assert_eq!(context.runtime_usage(), before);
}
