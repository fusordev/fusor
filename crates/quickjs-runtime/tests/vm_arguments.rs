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
fn sloppy_arguments_alias_simple_parameters_and_expose_the_mapped_shape() {
    let result = string_result(
        "function inspect(a,b){const c=Object.getOwnPropertyDescriptor(arguments,'callee');\
            a=5;const fromBinding=arguments[0];arguments[1]=6;arguments[2]=9;\
            return Object.prototype.toString.call(arguments)+'|'+fromBinding+'|'+b+'|'+\
                (c.value===inspect)+c.writable+c.enumerable+c.configurable+'|'+arguments[2];}\
            return inspect(1,2,3);",
    );
    assert_eq!(result, "[object Arguments]|5|6|truetruefalsetrue|9");
}

#[test]
fn mapped_definitions_update_or_sever_parameter_aliases_exactly() {
    let result = string_result(
        "function inspect(a,b,c,d){a=10;const own=Object.getOwnPropertyDescriptor(arguments,'0').value;\
            Object.defineProperty(arguments,'0',{value:11});const valueWrite=a;a=12;const kept=arguments[0];\
            Object.defineProperty(arguments,'1',{writable:false});b=20;const frozen=arguments[1];\
            Object.defineProperty(arguments,'2',{get(){return 30},configurable:true});c=31;const accessor=arguments[2];\
            delete arguments[3];d=40;arguments[3]=41;\
            return own+'|'+valueWrite+'|'+kept+'|'+frozen+'|'+accessor+'|'+d+'|'+arguments[3];}\
            return inspect(1,2,3,4);",
    );
    assert_eq!(result, "10|11|12|2|30|40|41");
}

#[test]
fn mapped_set_honours_receiver_identity_and_survives_the_call_frame() {
    let result = string_result(
        "function inspect(a){const receiver={};const separate=Reflect.set(arguments,'0',7,receiver);\
            const unchanged=a;const direct=Reflect.set(arguments,'0',8);\
            return [arguments,function(){return a;},separate,direct,unchanged,receiver[0]];}\
            const result=inspect(1);result[0][0]=9;\
            return result[1]()+'|'+result[2]+'|'+result[3]+'|'+result[4]+'|'+result[5];",
    );
    assert_eq!(result, "9|true|true|1|7");
}

#[test]
fn a_rooted_mapped_arguments_object_keeps_its_parameter_cell_through_collection() {
    let producer = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(
                "function inspect(a){return [arguments,function(){return a;}];}return inspect(1);",
            )
            .expect("producer body"),
        ))
        .expect("producer authority");
    let consumer = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(
                "'use strict';const pair=arguments[0];pair[0][0]=13;return pair[1]();",
            )
            .expect("consumer body"),
        ))
        .expect("consumer authority");

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (pair, consumer) = {
        let mut context = runtime.context(&realm).expect("context");
        let producer = context
            .execute_dynamic_function_script(producer, ExecutionLimits::default())
            .expect("producer")
            .into_function()
            .expect("producer function");
        let pair = context
            .call(&producer, &[], ExecutionLimits::default())
            .expect("mapped pair");
        let consumer = context
            .execute_dynamic_function_script(consumer, ExecutionLimits::default())
            .expect("consumer")
            .into_function()
            .expect("consumer function");
        (pair, consumer)
    };
    runtime
        .collect_cycles()
        .expect("rooted mapped arguments collection");

    let mut context = runtime.context(&realm).expect("context after collection");
    let result = context
        .call(&consumer, &[pair], ExecutionLimits::default())
        .expect("consumer completion");
    assert!(
        result
            .as_number()
            .expect("live result")
            .expect("number")
            .strict_equals(JsNumber::from_i32(13))
    );
}

#[test]
fn sealing_preserves_mapping_while_freezing_snapshots_and_detaches() {
    let result = string_result(
        "function sealed(a){Object.seal(arguments);a=2;return arguments[0];}\
            function frozen(a){a=3;Object.freeze(arguments);a=4;\
                return arguments[0]+'|'+Object.getOwnPropertyDescriptor(arguments,'0').writable;}\
            return sealed(1)+'|'+frozen(1);",
    );
    assert_eq!(result, "2|3|false");
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

#[test]
fn mapped_arguments_binding_cell_limit_failure_is_atomic() {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8("function inspect(a){return arguments.length;}return inspect;")
                .expect("body"),
        ))
        .expect("dynamic Function authority");

    let binding_cell_limit = {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("probe runtime");
        let realm = runtime.create_realm().expect("probe realm");
        let mut context = runtime.context(&realm).expect("probe context");
        let outer = context
            .execute_dynamic_function_script(Arc::clone(&authority), ExecutionLimits::default())
            .expect("probe dynamic Function")
            .into_function()
            .expect("probe outer function");
        let function = context
            .call(&outer, &[], ExecutionLimits::default())
            .expect("probe inspect creation")
            .into_function()
            .expect("probe inspect function");
        let limit = context.runtime_usage().binding_cells();
        drop(function);
        limit
    };

    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_binding_cells(binding_cell_limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function")
        .into_function()
        .expect("outer function");
    let function = context
        .call(&outer, &[], ExecutionLimits::default())
        .expect("inspect creation")
        .into_function()
        .expect("inspect function");
    let before = context.runtime_usage();
    let argument = context.number(JsNumber::from_i32(1));
    let failure = context
        .call(&function, &[argument], ExecutionLimits::default())
        .expect_err("mapped arguments must exceed the binding-cell limit");
    assert!(
        matches!(
            failure,
            ExecutionError::LimitExceeded {
                resource: RuntimeResource::BindingCells,
                limit,
                observed,
            } if limit == binding_cell_limit && observed == binding_cell_limit + 1
        ),
        "unexpected failure: {failure:?}"
    );
    assert_eq!(context.runtime_usage(), before);
}
