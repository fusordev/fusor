//! Strict and mapped arguments exotic objects, pinned to ECMA-262.

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
fn duplicate_parameters_map_only_the_last_formal_position() {
    let result = string_result(
        "function inspect(a,a){const before=a;arguments[0]=7;const afterFirst=a;\
            arguments[1]=8;const afterSecond=a;a=9;\
            return before+'|'+afterFirst+'|'+afterSecond+'|'+arguments[0]+'|'+arguments[1];}\
            return inspect(1,2);",
    );
    assert_eq!(result, "2|2|8|7|9");
}

#[test]
fn an_unsupplied_last_duplicate_parameter_has_no_earlier_alias() {
    let result = string_result(
        "function inspect(a,a){arguments[0]=7;return a+'|'+arguments[0]+'|'+arguments[1];}\
            return inspect(1);",
    );
    assert_eq!(result, "undefined|7|undefined");
}

#[test]
fn each_duplicate_parameter_name_maps_its_own_last_formal_position() {
    let result = string_result(
        "function inspect(a,b,a,b){arguments[0]=10;arguments[1]=20;\
            arguments[2]=30;arguments[3]=40;a=50;b=60;\
            return a+'|'+b+'|'+arguments[0]+'|'+arguments[1]+'|'+\
                arguments[2]+'|'+arguments[3];}return inspect(1,2,3,4);",
    );
    assert_eq!(result, "50|60|10|20|50|60");
}

#[test]
fn a_captured_duplicate_parameter_uses_the_last_mapped_position() {
    let result = string_result(
        "function inspect(a,a){return [arguments,function(){return a;}];}\
            const pair=inspect(1,2);pair[0][0]=7;const first=pair[1]();\
            pair[0][1]=8;return first+'|'+pair[1]();",
    );
    assert_eq!(result, "2|8");
}

#[test]
fn a_var_arguments_declaration_reuses_the_instantiated_arguments_binding() {
    let result = string_result(
        "function inspect(){const before=arguments;var arguments;\
            return Object.prototype.toString.call(arguments)+'|'+arguments.length+'|'+\
                (before===arguments);}return inspect(1,2);",
    );
    assert_eq!(result, "[object Arguments]|2|true");

    let result =
        string_result("function inspect(){var arguments=9;return ''+arguments;}return inspect(1);");
    assert_eq!(result, "9");
}

#[test]
fn a_named_function_expression_has_a_shadowing_arguments_binding() {
    let result = string_result(
        "const inspect=function arguments(){return arguments.length+'|'+\
            (arguments.callee===inspect)+'|'+typeof arguments;};return inspect(1);",
    );
    assert_eq!(result, "1|true|object");
}

#[test]
fn implicit_arguments_shadow_outer_bindings_but_not_inner_explicit_bindings() {
    let result = string_result(
        "let arguments=9;function inspect(){return ''+arguments[0];}return inspect(4);",
    );
    assert_eq!(result, "4");

    let result = string_result(
        "let arguments=9;function inspect(){arguments=3;return arguments;}\
            return inspect()+'|'+arguments;",
    );
    assert_eq!(result, "3|9");

    let result = string_result(
        "function outer(arguments){function inner(){return ''+arguments[0];}\
            return inner(5);}return outer(9);",
    );
    assert_eq!(result, "5");

    let result = string_result(
        "function inspect(){let result;{let arguments=3;result=arguments;}\
            return result+'|'+arguments[0];}\
            return inspect(4);",
    );
    assert_eq!(result, "3|4");

    let result = string_result(
        "function inspect(){let result;try{throw 3;}catch(arguments){result=arguments;}\
            return result+'|'+arguments[0];}return inspect(4);",
    );
    assert_eq!(result, "3|4");
}

#[test]
fn parameter_lexical_and_function_declarations_suppress_arguments_instantiation() {
    let result = string_result(
        "function parameter(arguments){return arguments;}\
            function lexical(){let arguments=2;return arguments;}\
            function declaration(){function arguments(){return 3;}return arguments();}\
            return parameter(1)+'|'+lexical(9)+'|'+declaration(9);",
    );
    assert_eq!(result, "1|2|3");
}

#[test]
fn expression_free_destructured_formals_initialize_left_to_right() {
    let result = string_result(
        "function inspect(keep,{value,...objectRest},[head,,...tail]){\
            return keep+'|'+value+'|'+objectRest.extra+'|'+head+'|'+tail.length+'|'+\
                tail[0]+'|'+tail[1]+'|'+arguments.length;}\
            return inspect(2,{value:3,extra:4},[5,6,7,8]);",
    );
    assert_eq!(result, "2|3|4|5|2|7|8|3");

    let result = string_result(
        "function capture({value}){return function(){return value;};}\
            return ''+capture({value:9})();",
    );
    assert_eq!(result, "9");

    let result = string_result(
        "let log='';const source={get value(){log+='g';return 1;}};\
            function merged({value}){function value(){return 5;}return log+'|'+value();}\
            return merged(source);",
    );
    assert_eq!(result, "g|5");

    let result = string_result(
        "function declared({value}){function other(){return 2;}return ''+(value+other());}\
            return declared({value:3});",
    );
    assert_eq!(result, "5");

    let result = string_result(
        "function assigned({value}){var value=7;return ''+value;}\
            return assigned({value:1});",
    );
    assert_eq!(result, "7");

    let result = string_result(
        "function overwritten({value}){function value(){return 5;}var value=7;return ''+value;}\
            return overwritten({value:1});",
    );
    assert_eq!(result, "7");

    let result = string_result(
        "const holder={inspect({value}){arguments[0]={value:8};\
            return value+'|'+arguments[0].value;}};\
            return holder.inspect({value:3});",
    );
    assert_eq!(result, "3|8");

    let result = string_result(
        "const holder={inspect(value){arguments[0]=8;return ''+value;}};\
            return holder.inspect(3);",
    );
    assert_eq!(result, "8");
}

#[test]
fn parameter_expressions_initialize_left_to_right_with_tdz_bindings() {
    let result = string_result(
        "function defaults(a=1,b=a+1){return defaults.length+':'+a+':'+b;}\
            const first=defaults();const second=defaults(5);const third=defaults(undefined,10);\
            let tdz=false;function forward(a=b,b=2){}\
            try{forward();}catch(error){tdz=error instanceof ReferenceError;}\
            let log='';function pattern({[log+='k']:value=3}={}){return value+':'+log;}\
            function supplied(value=arguments.length){return value+':'+arguments.length;}\
            function rest(value=1,...[tail=2]){return value+':'+tail+':'+rest.length;}\
            /* ECMA-262 keeps this parameter cell live; pinned QuickJS 2026-06-04\
               snapshots 1 here, so this is an intentional spec-first divergence. */\
            function capture(value=1,reader=function inner(){return value;}){value=4;return reader();}\
            return first+'|'+second+'|'+third+'|'+tdz+'|'+pattern()+'|'+\
                supplied(undefined,7)+'|'+rest()+'|'+capture();",
    );
    assert_eq!(result, "0:1:2|0:5:6|0:1:10|true|3:k|2:2|1:2:0|4");
}

#[test]
fn parameter_expression_body_environments_copy_and_then_diverge() {
    let result = string_result(
        "function copied(a=1){var a;return ''+a;}\
            function separated(a=1,reader=function inner(){return a}){\
                var a=2;return reader()+':'+a;}\
            function declared(a=1,reader=function inner(){return a}){\
                function a(){return 3;}return reader()+':'+a();}\
            let outer=7;function bodyOnly(value=outer){var outer=2;return value+':'+outer;}\
            function copiedArgs(value=1){var arguments;\
                return arguments.length+':'+arguments[1];}\
            function assignedArgs(value=arguments.length){var arguments;arguments=9;\
                return value+':'+arguments;}\
            function replacedArgs(value=arguments.length){function arguments(){return 3;}\
                return value+':'+arguments();}\
            function parameterArgs(arguments=4){var arguments;return ''+arguments;}\
            function pattern({value=1}={}){var value;return ''+value;}\
            function patternFunction({value=1}={}){function value(){return 3;}\
                return ''+value();}\
            const holder={method(a=1,reader=function inner(){return a}){\
                var a=2;return reader()+':'+a;}};\
            return copied()+'|'+separated()+'|'+declared()+'|'+bodyOnly()+'|'+\
                copiedArgs(5,6)+'|'+assignedArgs(undefined,6)+'|'+\
                replacedArgs(undefined,6)+'|'+parameterArgs()+'|'+pattern()+'|'+\
                patternFunction()+'|'+holder.method();",
    );
    assert_eq!(result, "1|1:2|1:3|7:2|2:6|2:9|2:3|4|1|3|1:2");
}

#[test]
fn formal_rest_parameters_snapshot_an_independent_supplied_tail() {
    let result = string_result(
        "function inspect(fixed,...rest){const before=rest[0]+'|'+arguments[1];\
            arguments[1]=9;rest[0]=7;\
            return inspect.length+'|'+arguments.length+'|'+rest.length+'|'+before+'|'+\
                arguments[1]+'|'+rest[0];}\
            return inspect(1,2,3,4);",
    );
    assert_eq!(result, "1|4|3|2|2|9|7");

    let result = string_result(
        "function patterned(...[first,{value},...tail]){\
            return first+'|'+value+'|'+tail.length+'|'+tail[1]+'|'+arguments.length;}\
            return patterned(1,{value:2},3,4);",
    );
    assert_eq!(result, "1|2|2|4|4");

    let result = string_result(
        "function suppressed(...arguments){return arguments.length+'|'+arguments[0];}\
            return suppressed(5,6);",
    );
    assert_eq!(result, "2|5");

    let result = string_result(
        "function merged(...rest){function rest(){return 6;}return ''+rest();}\
            return merged(1,2);",
    );
    assert_eq!(result, "6");

    let result = string_result(
        "function capture(...rest){return function(){return rest[1];};}\
            return ''+capture(7,8)();",
    );
    assert_eq!(result, "8");

    let result = string_result(
        "const holder={inspect(fixed,...rest){return this===holder?'yes|'+rest[1]:'no';}};\
            return holder.inspect(1,2,3);",
    );
    assert_eq!(result, "yes|3");
}

#[test]
fn destructured_formals_use_unmapped_arguments_and_can_suppress_the_binding() {
    let result = string_result(
        "function inspect({value}){let before=value;arguments[0]={value:9};value=7;\
            let restricted=false;try{arguments.callee;}catch(error){restricted=error instanceof TypeError;}\
            return before+'|'+value+'|'+arguments[0].value+'|'+restricted;}\
            return inspect({value:1});",
    );
    assert_eq!(result, "1|7|9|true");

    let result = string_result(
        "function inspect({arguments}){return ''+arguments;}\
            return inspect({arguments:4});",
    );
    assert_eq!(result, "4");
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
fn rest_array_property_limit_failure_is_atomic() {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(
                "function inspect(...arguments){return arguments.length;}return inspect;",
            )
            .expect("body"),
        ))
        .expect("dynamic Function authority");

    let property_limit = {
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
        let limit = context.runtime_usage().object_properties();
        drop(function);
        limit
    };

    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_object_properties(property_limit))
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
    let first = context.number(JsNumber::from_i32(1));
    let second = context.number(JsNumber::from_i32(2));
    assert!(matches!(
        context.call(&function, &[first, second], ExecutionLimits::default()),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == property_limit && observed == property_limit + 3
    ));
    assert_eq!(context.runtime_usage(), before);
}

#[test]
fn rest_tail_copy_consumes_shared_instruction_fuel() {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(
                "function inspect(...arguments){return arguments.length;}return inspect;",
            )
            .expect("body"),
        ))
        .expect("dynamic Function authority");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
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
    let arguments = (0..1_000)
        .map(|value| context.number(JsNumber::from_i32(value)))
        .collect::<Vec<_>>();
    assert!(matches!(
        context.call(
            &function,
            &arguments,
            ExecutionLimits::default().with_instruction_fuel(100),
        ),
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}

#[test]
fn mapped_arguments_binding_cell_limit_failure_is_atomic() {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8("function inspect(a,a){return arguments.length;}return inspect;")
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
    let argument = context.number(JsNumber::from_i32(1));
    let inactive = context
        .call(&function, &[argument], ExecutionLimits::default())
        .expect("an unsupplied last duplicate allocates no mapped cell");
    assert!(
        inactive
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(1))
    );
    runtime
        .collect_cycles()
        .expect("collect inactive unmapped arguments object");
    let mut context = runtime.context(&realm).expect("context after collection");
    let before = context.runtime_usage();
    let first = context.number(JsNumber::from_i32(1));
    let second = context.number(JsNumber::from_i32(2));
    let failure = context
        .call(&function, &[first, second], ExecutionLimits::default())
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
