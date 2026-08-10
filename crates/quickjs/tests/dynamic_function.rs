use quickjs::{
    DynamicFunctionConstructionError, DynamicFunctionLimits, call_with_dynamic_function_support,
    construct_dynamic_function,
};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{
    DynamicFunctionScriptError, ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, Runtime,
    RuntimeLimits, ValueKind,
};

fn source<'source>(
    parameters: &'source [SourceFragment<'source>],
    body: &'source str,
) -> DynamicFunctionSource<'source> {
    source_kind(DynamicFunctionKind::Function, parameters, body)
}

fn source_kind<'source>(
    kind: DynamicFunctionKind,
    parameters: &'source [SourceFragment<'source>],
    body: &'source str,
) -> DynamicFunctionSource<'source> {
    DynamicFunctionSource::new(kind, parameters, SourceFragment::new(body))
}

#[test]
fn dynamic_function_compiles_parenthesized_super_member_optional_calls() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    construct_dynamic_function(
        &mut context,
        source(
            &[],
            "class C{constructor(key){void ((super[key])?.());void ((super.value)?.());}}",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("parenthesized super references remain valid optional-call callees");
}

#[test]
fn dynamic_generator_function_compiles_and_executes_through_the_facade() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let parameters = [SourceFragment::new("value")];

    let generated = construct_dynamic_function(
        &mut context,
        source_kind(
            DynamicFunctionKind::GeneratorFunction,
            &parameters,
            "yield value; return 9;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic GeneratorFunction")
    .into_value();
    let consumer_parameters = [SourceFragment::new("factory")];
    let consumer = construct_dynamic_function(
        &mut context,
        source(
            &consumer_parameters,
            "let iterator=factory(4);\
             let first=iterator.next();\
             let second=iterator.next();\
             return first.value+':'+first.done+'|'+second.value+':'+second.done;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("generator consumer")
    .into_value()
    .into_function()
    .expect("consumer function");

    let result = context
        .call(&consumer, &[generated], ExecutionLimits::default())
        .expect("consume dynamic generator");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "4:false|9:true"
    );
}

#[test]
fn intrinsic_generator_function_constructor_uses_the_dynamic_compiler_service() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let GeneratorFunction=(function*(){}).constructor;\
             let generated=GeneratorFunction('value','yield value; return 9;');\
             let iterator=generated(4);\
             let first=iterator.next();\
             let second=iterator.next();\
             return first.value+':'+first.done+'|'+second.value+':'+second.done+'|'+\
                 generated.name+':'+generated.length+'|'+\
                 (Object.getPrototypeOf(generated)===GeneratorFunction.prototype);",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("outer dynamic Function")
    .into_value()
    .into_function()
    .expect("outer function");

    let result = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("intrinsic GeneratorFunction");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "4:false|9:true|anonymous:1|true"
    );
}

#[test]
fn generator_function_construction_honors_new_target_prototype_and_fallback() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let GeneratorFunction=(function*(){}).constructor;\
             let custom=function Custom(){};\
             let expected={marker:1};\
             custom.prototype=expected;\
             let first=Reflect.construct(GeneratorFunction,['yield 1;'],custom);\
             custom.prototype=0;\
             let second=Reflect.construct(GeneratorFunction,['yield 2;'],custom);\
             return (Object.getPrototypeOf(first)===expected)+'|'+\
                 (Object.getPrototypeOf(second)===GeneratorFunction.prototype);",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("outer dynamic Function")
    .into_value()
    .into_function()
    .expect("outer function");

    let result = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("GeneratorFunction construction");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "true|true"
    );
}

#[test]
fn dynamic_generator_wrapper_escape_preserves_the_script_completion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let completion = construct_dynamic_function(
        &mut context,
        source_kind(DynamicFunctionKind::GeneratorFunction, &[], "}), ({"),
        DynamicFunctionLimits::default(),
    )
    .expect("QuickJS-compatible generator wrapper escape");
    assert_eq!(
        completion.prepared_source().generated_source(),
        "(function* anonymous(\n) {\n}), ({\n})"
    );
    assert_eq!(
        completion.value().kind().expect("live completion"),
        ValueKind::Object
    );
}

#[test]
fn intrinsic_generator_function_reports_generator_syntax_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let GeneratorFunction=(function*(){}).constructor;\
             try{GeneratorFunction('yield (');}catch(error){return error.name;}\
             return 'missing SyntaxError';",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("outer dynamic Function")
    .into_value()
    .into_function()
    .expect("outer function");

    let result = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("catch generator syntax error");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "SyntaxError"
    );
}

#[test]
fn ordinary_dynamic_function_compiles_the_whole_wrapper_and_executes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let parameters = [SourceFragment::new("value")];

    let completion = construct_dynamic_function(
        &mut context,
        source(&parameters, "return value;"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function");
    assert_eq!(
        completion.prepared_source().generated_source(),
        "(function anonymous(value\n) {\nreturn value;\n})"
    );
    let function = completion
        .into_value()
        .into_function()
        .expect("ordinary wrapper completion");
    let seven = context.number(JsNumber::from_i32(7));
    let result = context
        .call(&function, &[seven], ExecutionLimits::default())
        .expect("call constructed function");
    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(7))
    );
}

#[test]
fn ordinary_dynamic_function_executes_labeled_switch_control_flow() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let parameters = [SourceFragment::new("limit")];
    let body = "\
        let current=0;\
        let result=0;\
        outer: while(current<limit){\
            current++;\
            switch(current){\
                case 1: result+=1; break;\
                case 2: result+=10; continue outer;\
                default: break outer;\
            }\
            result+=100;\
        }\
        return result;";

    let function = construct_dynamic_function(
        &mut context,
        source(&parameters, body),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function with labeled switch")
    .into_value()
    .into_function()
    .expect("ordinary wrapper completion");
    let limit = context.number(JsNumber::from_i32(3));
    let result = context
        .call(&function, &[limit], ExecutionLimits::default())
        .expect("dynamic Function call");

    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(111))
    );
}

#[test]
fn ordinary_dynamic_function_executes_through_debugger_statement() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let function = construct_dynamic_function(
        &mut context,
        source(&[], "debugger; return 17;"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function containing debugger statement")
    .into_value()
    .into_function()
    .expect("ordinary wrapper completion");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("debugger statement is an execution no-op");

    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(17))
    );
}

#[test]
fn ordinary_dynamic_function_executes_the_oxc_chained_label_continue_extension() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let body = "\
        let current=0;\
        let result=0;\
        outer: inner: while(current<3){\
            current++;\
            if(current<3) continue outer;\
            result=current;\
        }\
        return result;";

    let function = construct_dynamic_function(
        &mut context,
        source(&[], body),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function with Oxc chained-label continue semantics")
    .into_value()
    .into_function()
    .expect("ordinary wrapper completion");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("chained-label dynamic Function call");

    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(3))
    );
}

#[test]
fn facade_call_supplies_the_real_oxc_compiler_to_global_function() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(&[], "return new Function('return 9;')();"),
        DynamicFunctionLimits::default(),
    )
    .expect("outer dynamic Function")
    .into_value()
    .into_function()
    .expect("outer function");

    let value = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("nested global Function");

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(9))
    );
}

#[test]
fn facade_global_function_resumes_object_source_conversion_with_real_oxc() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let body={\
                 toString:function bodySource(){return 'return 12;';}\
             };\
             return Function(body)();",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("outer dynamic Function")
    .into_value()
    .into_function()
    .expect("outer function");

    let value = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("object source conversion");

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(12))
    );
}

#[test]
fn facade_executes_static_object_methods_getters_and_setters() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let object={\
                 stored:0,\
                 set value(next){\"use strict\";this.stored=next;return 99;},\
                 get value(){\"use strict\";return this.stored;},\
                 read(){\"use strict\";return this.value;}\
             };\
             let assigned=object.value=40;\
             if(assigned!==40){return 0;}\
             if(object.read()!==40){return 0;}\
             return 82;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("Oxc object method and accessor frontend")
    .into_value()
    .into_function()
    .expect("outer function");

    let value = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("static object method and accessor execution");

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(82)),
        "the setter return is discarded while the assignment RHS and getter receiver are preserved"
    );
}

#[test]
fn facade_executes_cooked_quoted_and_canonical_numeric_object_keys() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(
            &[],
            r#"let object={
                 stored:0,
                 "\u0072ead"(){"use strict";return this.value;},
                 1e400(){return 2;},
                 set "value"(next){"use strict";this.stored=next;},
                 get "\u0076alue"(){"use strict";return this.stored;}
             };
             object.value=40;
             if(object.read.name!=="read"){return 0;}
             if(object.Infinity.name!=="Infinity"){return 0;}
             if(object.read()!==40){return 0;}
             if(object.Infinity()!==2){return 0;}
             return 42;"#,
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("quoted and numeric static-key frontend")
    .into_value()
    .into_function()
    .expect("outer function");

    let value = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("quoted and numeric object-key execution");

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(42))
    );
}

#[test]
fn facade_wrapper_escape_can_invoke_global_function_during_script_execution() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let value = construct_dynamic_function(
        &mut context,
        source(&[], "}), Function('return 6;')() || (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("wrapper escape with nested Function")
    .into_value();

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(6))
    );
}

#[test]
fn wrapper_escape_returns_the_complete_script_completion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let completion = construct_dynamic_function(
        &mut context,
        source(&[], "}), ({ marker: 1"),
        DynamicFunctionLimits::default(),
    )
    .expect("QuickJS-compatible wrapper escape");

    assert_eq!(
        completion.value().kind().expect("live completion"),
        ValueKind::Object
    );
}

#[test]
fn wrapper_escape_observes_the_constructor_realm_global_receiver() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let body = "}), (function(){}) ? this : (function(){";

    let first = construct_dynamic_function(
        &mut context,
        source(&[], body),
        DynamicFunctionLimits::default(),
    )
    .expect("first escaped Script receiver")
    .into_value()
    .into_object()
    .expect("global object");
    let second = construct_dynamic_function(
        &mut context,
        source(&[], body),
        DynamicFunctionLimits::default(),
    )
    .expect("second escaped Script receiver")
    .into_value()
    .into_object()
    .expect("global object");

    assert!(
        first
            .same_identity(&second)
            .expect("same-runtime object identities")
    );
}

#[test]
fn sloppy_dynamic_function_this_uses_its_constructor_realm_global() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(&[], "return this;"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("ordinary dynamic function");
    let expected = construct_dynamic_function(
        &mut context,
        source(&[], "}), (function(){}) ? this : (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("escaped Script receiver")
    .into_value()
    .into_object()
    .expect("constructor-realm global object");

    let actual = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("sloppy call")
        .into_object()
        .expect("constructor-realm global object");
    assert!(
        actual
            .same_identity(&expected)
            .expect("same-runtime object identities")
    );
}

#[test]
fn separately_constructed_functions_share_constructor_realm_globals() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setter = construct_dynamic_function(
        &mut context,
        source(&[], "facadeMarker = 7; return facadeMarker;"),
        DynamicFunctionLimits::default(),
    )
    .expect("global setter")
    .into_value()
    .into_function()
    .expect("setter function");
    let getter = construct_dynamic_function(
        &mut context,
        source(&[], "return facadeMarker;"),
        DynamicFunctionLimits::default(),
    )
    .expect("global getter")
    .into_value()
    .into_function()
    .expect("getter function");

    let set = context
        .call(&setter, &[], ExecutionLimits::default())
        .expect("global write");
    assert!(
        set.as_number()
            .expect("live setter result")
            .expect("numeric setter result")
            .strict_equals(JsNumber::from_i32(7))
    );
    let get = context
        .call(&getter, &[], ExecutionLimits::default())
        .expect("global read");
    assert!(
        get.as_number()
            .expect("live getter result")
            .expect("numeric getter result")
            .strict_equals(JsNumber::from_i32(7))
    );
}

#[test]
fn escaped_program_var_is_instantiated_once_in_the_constructor_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    construct_dynamic_function(
        &mut context,
        source(&[], "}); var escapedVar = 5; (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("initialized Program var");
    construct_dynamic_function(
        &mut context,
        source(&[], "}); var escapedVar; (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("uninitialized redeclaration preserves the property value");
    let getter = construct_dynamic_function(
        &mut context,
        source(&[], "return escapedVar;"),
        DynamicFunctionLimits::default(),
    )
    .expect("separate construction resolves the persisted var")
    .into_value()
    .into_function()
    .expect("getter function");

    let value = context
        .call(&getter, &[], ExecutionLimits::default())
        .expect("read escaped Program var")
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(value.strict_equals(JsNumber::from_i32(5)));
}

#[test]
fn escaped_program_lexical_is_private_but_survives_through_a_closure() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let reader = construct_dynamic_function(
        &mut context,
        source(&[], "}); let hidden = 2; (function(){ return hidden;"),
        DynamicFunctionLimits::default(),
    )
    .expect("escaped lexical closure")
    .into_value()
    .into_function()
    .expect("reader function");
    let hidden = context
        .call(&reader, &[], ExecutionLimits::default())
        .expect("captured lexical read")
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(hidden.strict_equals(JsNumber::from_i32(2)));

    let global_probe = construct_dynamic_function(
        &mut context,
        source(&[], "return typeof hidden;"),
        DynamicFunctionLimits::default(),
    )
    .expect("separate global probe")
    .into_value()
    .into_function()
    .expect("probe function");
    let probe = context
        .call(&global_probe, &[], ExecutionLimits::default())
        .expect("probe result");
    assert_eq!(
        probe
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("short ASCII string"),
        "undefined"
    );
}

#[test]
fn escaped_program_function_is_hoisted_and_captures_program_lexicals() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let resolver = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "}); let before = declared(); function declared(){ return cell; } \
             let cell = 8; (function(){ return before;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect_err("calling the hoisted function before its lexical initializes must throw");
    let DynamicFunctionConstructionError::Runtime {
        source: DynamicFunctionScriptError::Execution(ExecutionError::Exception(tdz)),
        ..
    } = resolver
    else {
        panic!("hoisted call must fail with a JavaScript exception");
    };
    assert_eq!(tdz.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        tdz.message()
            .expect("engine-created error message")
            .to_utf8_lossy()
            .expect("short ASCII message"),
        "cell is not initialized"
    );

    let resolver = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "}); function declared(){ return cell; } let cell = 8; \
             (function(){ return declared;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("hoisted Program function")
    .into_value()
    .into_function()
    .expect("resolver function");
    let declared = context
        .call(&resolver, &[], ExecutionLimits::default())
        .expect("resolve declaration")
        .into_function()
        .expect("declared function");
    let result = context
        .call(&declared, &[], ExecutionLimits::default())
        .expect("declared function captures Program lexical")
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(result.strict_equals(JsNumber::from_i32(8)));
}

#[test]
fn synthetic_anonymous_name_is_not_a_lexical_binding() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let function = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let direct=typeof anonymous;\
             let nested=(function(){return typeof anonymous;})();\
             let evaluated=(function(){eval('');return typeof anonymous;})();\
             globalThis.anonymous='realm';\
             let resolved=anonymous;\
             delete globalThis.anonymous;\
             return direct+'|'+nested+'|'+evaluated+'|'+resolved;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function without a synthetic name binding")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function name lookup");

    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "undefined|undefined|undefined|realm"
    );
}

#[test]
fn closed_direct_eval_executes_in_the_calling_function() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(&[], "return eval('let answer = 40 + 2; answer;');"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function with direct eval")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("closed direct eval")
    .as_number()
    .expect("number value")
    .expect("finite number");
    assert!(result.strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn closed_direct_eval_executes_inside_an_object_method() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let object={method(){return eval('let value=40;value+2;');}};return object.method();",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function with method direct eval")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("method direct eval")
    .as_number()
    .expect("number value")
    .expect("finite number");
    assert!(result.strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn class_field_direct_eval_rejects_arguments_before_execution_across_arrows() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let executed=0;\
             class Public{value=(()=>eval('executed++; arguments;'))();}\
             class Private{#value=eval('executed++; () => arguments;');}\
             let publicRejected=false;\
             let privateRejected=false;\
             try{new Public;}catch(error){publicRejected=error.name==='SyntaxError';}\
             try{new Private;}catch(error){privateRejected=error.name==='SyntaxError';}\
             class Boundary{value=(function(){return eval('arguments.length;');})(1,2,3);}\
             return publicRejected&&privateRejected&&executed===0&&new Boundary().value===3;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function with class field direct eval")
    .into_value()
    .into_function()
    .expect("function completion");

    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("class field direct eval early errors");
    assert_eq!(result.as_boolean().expect("live Boolean"), Some(true));
}

#[test]
fn class_private_environment_is_visible_to_direct_eval() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "class C{\
                 #field=40;\
                 #method(){return 1;}\
                 get #accessor(){return 1;}\
                 set #accessor(value){this.#field=value;}\
                 initialized=eval('this.#field+this.#method()+this.#accessor');\
                 read(){return eval('this.#field');}\
                 nested(){return eval(\"eval('this.#field')\");}\
                 escape(){return eval('()=>this.#field');}\
                 has(){return eval('#field in this');}\
                 write(){return eval('this.#accessor=42');}\
                 reject(){try{eval('this.#missing');}catch(error){return error.name;}}\
                 static #staticField=43;\
                 static read(){return eval('this.#staticField');}\
             }\
             let instance=new C;\
             let before=instance.read();\
             let escaped=instance.escape();\
             instance.write();\
             return instance.initialized+'|'+before+'|'+instance.read()+'|'+\
                 instance.nested()+'|'+escaped()+'|'+instance.has()+'|'+\
                 C.read()+'|'+instance.reject();",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function with class private direct eval")
    .into_value()
    .into_function()
    .expect("function completion");

    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("class private direct eval");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "42|40|42|42|42|true|43|SyntaxError"
    );
}

#[test]
fn direct_eval_non_string_returns_without_requesting_a_compiler() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(&[], "return eval(42);"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("function completion");

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("non-string direct eval")
        .as_number()
        .expect("number value")
        .expect("finite number");
    assert!(result.strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn direct_eval_inherits_strict_this_and_source_strict_var_locality() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let strict_this = construct_dynamic_function(
        &mut context,
        source(&[], "\"use strict\"; return eval('this');"),
        DynamicFunctionLimits::default(),
    )
    .expect("strict dynamic Function")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &strict_this,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("strict direct eval this");
    assert_eq!(result.kind().expect("live result"), ValueKind::Undefined);

    let strict_source = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "return eval('\"use strict\"; var answer=40+2; answer;');",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &strict_source,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("source-strict direct eval")
    .as_number()
    .expect("number value")
    .expect("finite number");
    assert!(result.strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn shadowed_eval_falls_back_to_an_ordinary_call_after_identity_check() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "let replacement=function(){return 42;};return (function(eval){return eval('ignored');})(replacement);",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("ordinary shadowed eval call")
    .as_number()
    .expect("number value")
    .expect("finite number");
    assert!(result.strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn direct_eval_syntax_error_is_catchable_at_the_callsite() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "try{return eval('let =');}catch(error){return error.name;}",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("function completion");
    let result = call_with_dynamic_function_support(
        &mut context,
        &function,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("caught eval SyntaxError")
    .as_string()
    .expect("string value")
    .expect("string result")
    .to_utf8_lossy()
    .expect("ASCII error name");
    assert_eq!(result, "SyntaxError");
}

#[test]
fn syntax_failure_retains_the_exact_wrapper_without_installing_code() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = construct_dynamic_function(
            &mut context,
            source(&[], "return ("),
            DynamicFunctionLimits::default(),
        )
        .expect_err("malformed body");
        assert!(matches!(
            &error,
            DynamicFunctionConstructionError::Frontend(_)
        ));
        assert_eq!(
            error
                .prepared_source()
                .expect("parser failure retains wrapper")
                .generated_source(),
            "(function anonymous(\n) {\nreturn (\n})"
        );
    }
    assert_eq!(runtime.usage(), before);
}

#[test]
fn runtime_failure_retains_the_wrapper_and_releases_internal_script_state() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_public_roots(0)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = construct_dynamic_function(
            &mut context,
            source(&[], "return 1;"),
            DynamicFunctionLimits::default(),
        )
        .expect_err("public completion root exceeds the runtime limit");
        assert!(matches!(
            &error,
            DynamicFunctionConstructionError::Runtime { .. }
        ));
        assert_eq!(
            error
                .prepared_source()
                .expect("runtime failure retains wrapper")
                .generated_source(),
            "(function anonymous(\n) {\nreturn 1;\n})"
        );
    }

    runtime
        .collect_cycles()
        .expect("collect unpublished wrapper");
    assert_eq!(runtime.usage(), before);
}

#[test]
fn dynamic_async_generator_constructs_and_executes_awaited_yield() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let parameters = [SourceFragment::new("value")];
    let generated = construct_dynamic_function(
        &mut context,
        source_kind(
            DynamicFunctionKind::AsyncGeneratorFunction,
            &parameters,
            "yield await value;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic AsyncGeneratorFunction")
    .into_value();
    let consumer_parameters = [SourceFragment::new("factory")];
    let consumer = construct_dynamic_function(
        &mut context,
        source(
            &consumer_parameters,
            "let state={result:''};\
             factory(4).next().then(function(result){\
                 state.result=result.value+':'+result.done;\
             });\
             return state;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("async-generator consumer")
    .into_value()
    .into_function()
    .expect("consumer function");
    let read_parameters = [SourceFragment::new("state")];
    let read = construct_dynamic_function(
        &mut context,
        source(&read_parameters, "return state.result;"),
        DynamicFunctionLimits::default(),
    )
    .expect("state reader")
    .into_value()
    .into_function()
    .expect("reader function");

    let state = context
        .call(&consumer, &[generated], ExecutionLimits::default())
        .expect("consume async generator");
    let result = context
        .call(&read, &[state], ExecutionLimits::default())
        .expect("read async result");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "4:false"
    );
}
