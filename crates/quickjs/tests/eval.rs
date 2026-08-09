use quickjs::{ScriptEvaluationError, ScriptLimits, evaluate_script};
use quickjs_runtime::{
    ExecutionError, GlobalScriptError, InstallError, JsNumber, Runtime, RuntimeLimits,
    RuntimeResource, ValueKind,
};

fn evaluate<T>(source: &str, inspect: impl FnOnce(&quickjs_runtime::JsValue) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let value = evaluate_script(
        &mut context,
        source,
        "indirect-eval.js",
        ScriptLimits::default(),
    )
    .expect("Script evaluation");
    inspect(&value)
}

fn number(value: &quickjs_runtime::JsValue) -> JsNumber {
    value.as_number().expect("live value").expect("Number")
}

fn string(value: &quickjs_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn indirect_eval_returns_non_string_arguments_unchanged() {
    evaluate("(0, eval)(41) + 1;", |value| {
        assert!(number(value).strict_equals(JsNumber::from_i32(42)));
    });
}

#[test]
fn eval_intrinsic_has_the_standard_global_descriptor() {
    evaluate(
        "let descriptor = Object.getOwnPropertyDescriptor(globalThis, 'eval'); typeof eval === 'function' && eval.length === 1 && eval.name === 'eval' && descriptor.writable && !descriptor.enumerable && descriptor.configurable;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_binding_defaults_apply_named_evaluation_to_anonymous_definitions() {
    evaluate(
        r#"
        let definitions = [
            ["function() {}", false],
            ["function named() {}", true],
            ["function*() {}", false],
            ["function* named() {}", true],
            ["async function() {}", false],
            ["async function named() {}", true],
            ["() => {}", false],
            ["async () => {}", false],
            ["class {}", false],
            ["class named {}", true],
        ];
        let failures = [];
        function check(actual, expected, context) {
            if (actual !== expected) failures.push(context + ":" + actual + "!=" + expected);
        }
        for (let [definition, named] of definitions) {
            let property = eval(`(function({ value = ${definition} }) { return value; })`);
            check(property({}).name, named ? "named" : "value", "property " + definition);
            let element = eval(`(function([value = ${definition}]) { return value; })`);
            check(element([]).name, named ? "named" : "value", "element " + definition);
            let parameter = eval(`(function(value = ${definition}) { return value; })`);
            check(parameter().name, named ? "named" : "value", "parameter " + definition);
        }
        let pattern = eval(`(function({ name } = class {}) { return name; })`);
        check(pattern(), "", "pattern class");
        failures.join("|");
        "#,
        |value| assert_eq!(string(value), ""),
    );
}

#[test]
fn closed_direct_eval_returns_the_script_completion() {
    evaluate(
        "function local(){return eval('let answer=40+2;answer;');} local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn spread_direct_eval_materializes_the_iterator_and_evaluates_only_the_first_argument() {
    evaluate(
        "let elements=['x=1;','x=2;'],nextCount=0;let iterable={[Symbol.iterator](){return{next(){let index=nextCount++;return index<elements.length?{done:false,value:elements[index]}:{done:true};}};}};let result=(function(){let x='local';eval(...iterable);return x;})();result===1&&nextCount===3;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn empty_spreads_around_direct_eval_preserve_argument_list_order() {
    evaluate(
        "let nextCount=0;let empty={[Symbol.iterator](){return{next(){nextCount++;return{done:true};}};}};let missing=eval(...empty);let x=1;eval(...empty,'x=2;');eval('x=3;',...empty);missing===undefined&&x===3&&nextCount===3;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn spread_eval_identity_fallback_preserves_bare_and_with_reference_receivers() {
    evaluate(
        "let bareThis,withThis;let replacement=function(a,b){'use strict';bareThis=this;return a+b;};let bare=(function(eval){return eval(...[20,22]);})(replacement);let object={eval:function(a,b){'use strict';withThis=this;return a+b;}};let referenced;with(object){referenced=eval(...[19,23]);}bare===42&&referenced===42&&bareThis===undefined&&withThis===object;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn canonical_spread_eval_from_with_remains_direct_and_cleans_its_reference_receiver() {
    evaluate(
        "let object={answer:1,eval};let result;with(object){result=eval(...['answer=42;answer;']);}result===42&&object.answer===42;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn static_initializer_direct_eval_inherits_the_class_this_binding() {
    evaluate(
        "let block;let Box=class{static value='test';static direct=eval('this.value')+'262';static arrow=(()=>eval('this'))();static{block=eval('this');}static ordinary=(function(){return eval('this');}).call({marker:7});};Box.direct==='test262'&&Box.arrow===Box&&block===Box&&Box.ordinary.marker===7;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn class_field_direct_eval_resolves_the_initialized_inner_name_binding() {
    evaluate(
        "let Box=class Inner{field=eval('Inner');static field=eval('Inner');};Box.field===Box&&new Box().field===Box;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_in_inline_class_code_inherits_strictness() {
    evaluate(
        "function check(){try{class Box{static[eval(\"Object.preventExtensions({}).value=1\")];}return false;}catch(error){return error.name==='TypeError';}}check();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn inline_class_regions_use_strict_reference_semantics() {
    evaluate(
        "let computed=false;try{let target=Object.preventExtensions({});class C{[target.value=1](){}}}catch(error){computed=error.name==='TypeError';}\
         let heritage=false;try{let target=Object.preventExtensions({});let Base=function(){};class C extends(target.value=Base){}}catch(error){heritage=error.name==='TypeError';}\
         let field=false;try{let target=Object.preventExtensions({});class C{static value=(target.value=1);}}catch(error){field=error.name==='TypeError';}\
         let block=false;try{let target=Object.preventExtensions({});class C{static{target.value=1;}}}catch(error){block=error.name==='TypeError';}\
         let superWrite=false;try{class Base{}Object.defineProperty(Base,'value',{value:1,writable:false});class C extends Base{static value=(super.value=2);}}catch(error){superWrite=error.name==='TypeError';}\
         let functionName=false;try{(function self(){class C{static[self=1];}})();}catch(error){functionName=error.name==='TypeError';}\
         let unresolved=false;try{class C{static[__class_strict_missing__=1];}}catch(error){unresolved=error.name==='ReferenceError';}\
         delete globalThis.__class_strict_missing__;\
         let deletion=false;try{let target={};Object.defineProperty(target,'value',{value:1,configurable:false});class C{static[delete target.value];}}catch(error){deletion=error.name==='TypeError';}\
         computed&&heritage&&field&&block&&superWrite&&functionName&&unresolved&&deletion;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn eval_preserves_lone_surrogates_in_legacy_regexp_literals() {
    evaluate(
        "let unit=String.fromCharCode(0xD800);\
         let source='/\\\\'+unit+'/';\
         let direct=eval(source);\
         let indirect=(0,eval)(source);\
         direct.source===('\\\\'+unit)&&direct.test(unit)&&\
           indirect.source===('\\\\'+unit)&&indirect.test(unit);",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn eval_legacy_string_escapes_follow_the_eval_source_strictness() {
    evaluate(
        r#"let sloppy=eval("'\\141'");let strict=false;try{eval("'use strict'; '\\1';");}catch(error){strict=error.constructor===SyntaxError;}sloppy==='a'&&strict;"#,
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_inside_with_observes_the_object_environment() {
    evaluate(
        "let object={name:'str2'};with(object){eval(\"'str2'===name\");}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_inside_nested_with_uses_the_innermost_object_environment() {
    evaluate(
        "let outer={name:'outer',eval};let inner={name:'inner'};with(outer){with(inner){eval('name');}}",
        |value| assert_eq!(string(value), "inner"),
    );
}

#[test]
fn noncanonical_eval_from_with_receives_the_object_as_this() {
    evaluate(
        "let object;object={eval:function(source){'use strict';return this===object&&source==='payload';}};with(object){eval('payload');}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn canonical_eval_stored_on_with_object_remains_direct_eval() {
    evaluate(
        "let object={answer:42,eval};with(object){eval('answer');}",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn unscopable_eval_property_falls_back_to_the_realm_intrinsic() {
    evaluate(
        "let object={answer:42,eval:function(){return 0;}};object[Symbol.unscopables]={eval:true};with(object){eval('answer');}",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn lexical_eval_binding_inside_with_has_no_object_receiver() {
    evaluate(
        "let object={eval:function(){return false;}};let replacement=function(){'use strict';return this===undefined;};let result;with(object){{let eval=replacement;result=eval();}}result;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_inside_with_writes_and_deletes_object_properties() {
    evaluate(
        "let object={answer:1,removed:2};with(object){eval('answer=42;delete removed;');}object.answer+'|'+('removed' in object);",
        |value| assert_eq!(string(value), "42|false"),
    );
}

#[test]
fn direct_eval_inside_with_preserves_method_call_references() {
    evaluate(
        "let object={method:function(){'use strict';return this;}};with(object){eval('method()===object');}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn escaped_eval_closure_retains_the_with_object_environment() {
    evaluate(
        "let object={answer:1};let read;with(object){read=eval('()=>answer');}object.answer=42;read();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn eval_lexical_declaration_shadows_the_with_object_property() {
    evaluate(
        "let object={answer:1};let result;with(object){result=eval('let answer=42;answer;');}result+'|'+object.answer;",
        |value| assert_eq!(string(value), "42|1"),
    );
}

#[test]
fn sloppy_eval_var_initializer_resolves_through_with_before_variable_environment() {
    evaluate(
        "function run(){let object={answer:1};with(object){eval('var answer=42;');}return object.answer+'|'+String(answer);}run();",
        |value| assert_eq!(string(value), "42|undefined"),
    );
}

#[test]
fn nested_direct_eval_retains_the_ambient_with_environment() {
    evaluate(
        "let object={answer:42};with(object){eval(\"eval('answer');\");}",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_observes_a_captured_outer_with_environment() {
    evaluate(
        "function make(object){with(object){return function(){return eval('answer');};}}make({answer:42})();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn eval_variable_environment_precedes_a_captured_outer_with_environment() {
    evaluate(
        "function make(object){with(object){return function(){var answer=1;eval('var answer=42;');return object.answer+'|'+answer;};}}make({answer:1})();",
        |value| assert_eq!(string(value), "1|42"),
    );
}

#[test]
fn direct_eval_reads_arguments_and_writes_live_lexicals() {
    evaluate(
        "function local(argument){let value=1;eval('value=argument+1');return value;}local(41);",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn strict_direct_eval_writes_the_same_live_lexical_cell() {
    evaluate(
        "function local(){'use strict';let value=1;eval('value=42');return value;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_preserves_caller_const_assignment_semantics() {
    evaluate(
        "function local(){const value=1;try{eval('value=2');return false;}catch(error){return error.constructor===TypeError&&value===1;}}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_direct_eval_ignores_a_named_function_binding_write() {
    evaluate(
        "let named=function self(){eval('self=1');return typeof self;};named();",
        |value| assert_eq!(string(value), "function"),
    );
}

#[test]
fn strict_direct_eval_rejects_a_named_function_binding_write() {
    evaluate(
        "let named=function self(){try{eval('\"use strict\";self=1');return false;}catch(error){return error.constructor===TypeError;}};named();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_observes_caller_lexical_tdz() {
    evaluate(
        "function local(){try{return eval('value');}catch(error){return error.constructor===ReferenceError;}let value=1;}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_closures_retain_live_caller_cells() {
    evaluate(
        "function local(){let value=1;let read=eval('()=>value');value=42;return read();}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn escaped_direct_eval_closures_retain_caller_cells_after_return() {
    evaluate(
        "function local(){let value=40;return eval('()=>++value');}let increment=local();increment();increment();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_direct_eval_var_declaration_reuses_an_existing_parameter_cell() {
    evaluate(
        "function local(answer){eval('var answer=42;');return answer;}local(1);",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_direct_eval_var_without_initializer_preserves_an_existing_local() {
    evaluate(
        "function local(){var answer=42;eval('var answer;');return answer;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_direct_eval_creates_a_new_function_variable_binding() {
    evaluate(
        "var answer=1;function local(){eval('var answer=42;');return answer;}local()+answer;",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(43))),
    );
}

#[test]
fn sloppy_direct_eval_creates_a_new_function_declaration_binding() {
    evaluate(
        "function local(){eval('function answer(){return 42;}');return answer();}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn caller_closure_created_before_eval_observes_a_new_variable() {
    evaluate(
        "function local(){let read=()=>answer;eval('var answer=42;');return read;}local()();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn eval_variable_environments_are_distinct_per_activation() {
    evaluate(
        "function local(value){eval('var answer=value;');return ()=>answer;}let one=local(1),two=local(2);one()*10+two();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(12))),
    );
}

#[test]
fn escaped_caller_closure_direct_eval_observes_an_outer_eval_variable() {
    evaluate(
        "function outer(){eval('var answer=42;');return function(){return eval('answer;');};}outer()();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn eval_created_variable_bindings_are_deletable() {
    evaluate(
        "function local(){let deleted,missing,read;eval('var answer=42;deleted=delete answer;missing=typeof answer===\"undefined\";read=()=>answer;');try{read();return false;}catch(error){return deleted&&missing&&error.constructor===ReferenceError;}}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn eval_created_function_bindings_are_deletable() {
    evaluate(
        "function local(){let initial,deleted,read;eval('initial=answer();deleted=delete answer;read=()=>answer;function answer(){return 42;}');try{read();return false;}catch(error){return initial===42&&deleted&&error.constructor===ReferenceError;}}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn eval_reuses_and_preserves_a_static_local_binding() {
    evaluate(
        "function local(){var answer=0;let deleted=eval('var answer=42;delete answer;');return deleted+'|'+answer;}local();",
        |value| assert_eq!(string(value), "false|42"),
    );
}

#[test]
fn eval_declaration_instantiation_survives_an_eval_body_throw() {
    evaluate(
        "function local(){try{eval('var answer=42;throw 1;');}catch(error){}return answer;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn nested_sloppy_direct_eval_reuses_the_same_function_variable_cell() {
    evaluate(
        "function local(){var answer=1;eval('answer=2;eval(\"answer=42;\");');return answer;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_direct_eval_function_declaration_replaces_an_existing_var_cell() {
    evaluate(
        "function local(){var answer=1;eval('function answer(){return 42;}');return answer();}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_direct_eval_targets_the_body_var_inside_non_simple_parameters() {
    evaluate(
        "function local(value=1){var value;eval('var value=42;');return value;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn parameter_initializer_eval_rejects_the_ordinary_arguments_binding() {
    evaluate(
        "function local(value=eval('var arguments=42')){return false;}try{local();false;}catch(error){error.constructor===SyntaxError;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn arrow_parameter_initializer_eval_can_create_arguments() {
    evaluate(
        "let local=(value=eval('var arguments=42'),read=()=>arguments)=>arguments+read();local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(84))),
    );
}

#[test]
fn arrow_parameter_eval_binding_is_separate_from_a_body_function_declaration() {
    evaluate(
        "const old=globalThis.arguments;const f=(p=eval(\"var arguments='param'\"),q=()=>arguments)=>{function arguments(){}return typeof arguments+'|'+q()+'|'+(globalThis.arguments===old);};f();",
        |value| assert_eq!(string(value), "function|param|true"),
    );
}

#[test]
fn arrow_parameter_eval_binding_is_separate_from_a_body_lexical_declaration() {
    evaluate(
        "const old=globalThis.arguments;const f=(p=eval(\"var arguments='param'\"),q=()=>arguments)=>{let arguments='local';return arguments+'|'+q()+'|'+(globalThis.arguments===old);};f();",
        |value| assert_eq!(string(value), "local|param|true"),
    );
}

#[test]
fn arrow_parameter_eval_binding_is_separate_from_a_body_var_declaration() {
    evaluate(
        "const old=globalThis.arguments;const f=(p=eval(\"var arguments='param'\"),q=()=>arguments)=>{var arguments='local';return arguments+'|'+q()+'|'+(globalThis.arguments===old);};f();",
        |value| assert_eq!(string(value), "local|param|true"),
    );
}

#[test]
fn arrow_parameter_initializer_eval_rejects_an_arguments_parameter() {
    evaluate(
        "let local=(arguments=eval('var arguments=42'))=>false;try{local();false;}catch(error){error.constructor===SyntaxError;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn parameter_closure_cannot_observe_a_later_body_eval_variable() {
    evaluate(
        "function local(read=()=>typeof bodyOnly){eval('var bodyOnly=1');return read();}local();",
        |value| assert_eq!(string(value), "undefined"),
    );
}

#[test]
fn body_eval_variable_shadows_a_non_simple_parameter() {
    evaluate(
        "function local(value=1){eval('var value=42');return value;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn body_closure_observes_a_later_eval_shadow_of_a_parameter() {
    evaluate(
        "function local(value=1){let read=()=>value;eval('var value=42');return read();}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn nested_body_closure_retains_the_eval_shadow_boundary() {
    evaluate(
        "function outer(value=1){let middle=()=>()=>value;eval('var value=42');return middle()();}outer();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn inner_direct_eval_can_shadow_an_outer_parameter_capture() {
    evaluate(
        "function outer(value=1){return function inner(){eval('var value=42');return value;};}outer()();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn inner_direct_eval_can_shadow_an_outer_var_capture() {
    evaluate(
        "function outer(){var value=0;function inner(){eval('var value=42');return value;}return inner();}outer();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn an_outer_eval_variable_does_not_shadow_an_inner_parameter() {
    evaluate(
        "function outer(){eval('var value=42');return function inner(value=1){return value;};}outer()();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(1))),
    );
}

#[test]
fn body_eval_variable_shadows_the_non_simple_arguments_object() {
    evaluate(
        "function local(value=1){eval('var arguments=42');return arguments;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_direct_eval_rejects_a_function_body_lexical_collision() {
    evaluate(
        "function local(){let answer=1;try{eval('var answer=42;');}catch(error){return error instanceof SyntaxError&&answer===1;}return false;}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_direct_eval_var_initializer_resolves_to_a_matching_catch_parameter() {
    evaluate(
        "var err='outer',observed,escaped;\
         try{throw 'caught';}catch(err){\
           var completion=eval(\"var err='inner';err;\");\
           escaped=eval(\"var err;()=>err\");\
           observed=err+'|'+completion;\
         }\
         err+'|'+observed+'|'+escaped();",
        |value| assert_eq!(string(value), "outer|inner|inner|inner"),
    );
}

#[test]
fn matching_catch_parameter_does_not_hide_an_outer_lexical_eval_conflict() {
    evaluate(
        "function local(){\
           let err='outer lexical';\
           try{throw 'caught';}catch(err){\
             try{eval('var err;');return false;}\
             catch(error){return error.constructor===SyntaxError&&err==='caught';}\
           }\
         }\
         local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_direct_eval_function_declarations_install_beyond_a_matching_catch_parameter() {
    evaluate(
        "var observed;\
         try{throw 'caught';}catch(err){\
           eval(\"function err(){return 'installed';}\");\
           observed=err;\
         }\
         observed+'|'+typeof err+'|'+err();",
        |value| assert_eq!(string(value), "caught|function|installed"),
    );
}

#[test]
fn sloppy_direct_eval_loop_and_destructuring_writes_resolve_to_the_catch_parameter() {
    evaluate(
        "var err='outer',forIn,forOf,destructured;\
         try{throw 'caught';}catch(err){\
           eval(\"for(var err in {key:1}){}\");forIn=err;\
           eval(\"for(var err of ['value']){}\");forOf=err;\
           eval(\"var [err]=['pattern']\");destructured=err;\
         }\
         err+'|'+forIn+'|'+forOf+'|'+destructured;",
        |value| assert_eq!(string(value), "outer|key|value|pattern"),
    );
}

#[test]
fn sloppy_direct_eval_orders_with_objects_around_a_matching_catch_parameter() {
    evaluate(
        "var err='outer',inside,observed;var inner={err:'object'},outer={err:'object'};\
         try{throw 'caught';}catch(err){\
           with(inner){eval(\"var err='inner-with'\");}\
           inside=err;\
         }\
         with(outer){try{throw 'caught';}catch(err){\
           eval(\"var err='inside-catch'\");observed=err;\
         }}\
         err+'|'+inside+'|'+inner.err+'|'+observed+'|'+outer.err;",
        |value| {
            assert_eq!(string(value), "outer|caught|inner-with|inside-catch|object");
        },
    );
}

#[test]
fn direct_eval_resolves_outer_closures_before_realm_globals() {
    evaluate(
        "var value=1;function outer(value){return function(){return eval('value+1');};}outer(41)();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_unmatched_names_fall_back_to_the_realm_global() {
    evaluate(
        "var realmValue=41;function local(){return eval('realmValue+1');}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_global_direct_eval_publishes_configurable_vars_and_functions() {
    evaluate(
        "eval('var evalVar=40;function evalFunction(){return 2;}');let descriptor=Object.getOwnPropertyDescriptor(globalThis,'evalVar');evalVar+evalFunction()+'|'+descriptor.configurable;",
        |value| assert_eq!(string(value), "42|true"),
    );
}

#[test]
fn nested_sloppy_global_direct_eval_inherits_the_global_variable_environment() {
    evaluate(
        "eval(\"eval('var nestedEvalVar=42;')\");nestedEvalVar;",
        |value| {
            assert!(number(value).strict_equals(JsNumber::from_i32(42)));
        },
    );
}

#[test]
fn source_strict_global_direct_eval_keeps_var_declarations_local() {
    evaluate(
        "eval(\"'use strict';var strictDirectEvalVar=1;\");typeof strictDirectEvalVar;",
        |value| assert_eq!(string(value), "undefined"),
    );
}

#[test]
fn sloppy_global_direct_eval_var_statement_has_empty_completion() {
    evaluate("eval('var evalOnly;');", |value| {
        assert_eq!(value.kind(), Ok(ValueKind::Undefined));
    });
}

#[test]
fn sloppy_global_direct_eval_rejects_active_block_lexical_collisions() {
    evaluate(
        "{let collision;try{eval('var collision;');false;}catch(error){error.constructor===SyntaxError;}}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_global_direct_eval_rejects_global_lexical_collisions() {
    evaluate(
        "let collision;try{eval('var collision;');false;}catch(error){error.constructor===SyntaxError;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_global_direct_eval_rejected_properties_throw_type_error() {
    evaluate(
        "Object.preventExtensions(globalThis);try{eval('var unavailable;');false;}catch(error){error.constructor===TypeError;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_global_direct_eval_function_preflight_is_atomic() {
    evaluate(
        "Object.defineProperty(globalThis,'blocked',{value:1,writable:false,enumerable:false,configurable:false});try{eval('var unpublished;function blocked(){}');}catch(error){}typeof unpublished==='undefined'&&blocked===1;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn failed_direct_eval_install_rolls_back_promoted_and_created_cells() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_installed_code(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let error = evaluate_script(
        &mut context,
        "function local(){let value=1;return eval('value;var created=2;created;');}local();",
        "direct-eval-rollback.js",
        ScriptLimits::default(),
    )
    .expect_err("the nested direct-eval installation exceeds the pinned limit");

    assert!(matches!(
        error,
        ScriptEvaluationError::Runtime(GlobalScriptError::Execution(
            ExecutionError::DynamicFunctionInstallation(InstallError::LimitExceeded {
                resource: RuntimeResource::InstalledCode,
                ..
            })
        ))
    ));
    assert_eq!(context.runtime_usage().binding_cells(), 0);
}

#[test]
fn indirect_eval_without_an_argument_returns_undefined() {
    evaluate("(0, eval)();", |value| {
        assert_eq!(value.kind(), Ok(ValueKind::Undefined));
    });
}

#[test]
fn indirect_eval_returns_the_script_completion() {
    evaluate("(0, eval)(\"1; 40 + 2;\");", |value| {
        assert!(number(value).strict_equals(JsNumber::from_i32(42)));
    });
}

#[test]
fn indirect_eval_returns_primitive_expression_completions() {
    evaluate(
        "var x; (0, eval)(\"x = 1\") + '|' + (0, eval)(\"1\") + '|' + (0, eval)(\"'1'\") + '|' + (x = 1, (0, eval)(\"++x\"));",
        |value| assert_eq!(string(value), "1|1|1|2"),
    );
}

#[test]
fn indirect_eval_resolves_against_the_realm_global_environment() {
    evaluate(
        "var marker = 1; function local() { let marker = 2; return (0, eval)(\"marker\"); } local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(1))),
    );
}

#[test]
fn direct_eval_inherits_new_target_from_function_code() {
    evaluate(
        "function target(){return eval('new.target;');}target()===undefined&&new target()===target;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn new_target_defaults_survive_nested_declarations_and_direct_eval() {
    evaluate(
        "let matches=0;\
         function check(expected,actual=new.target){if(actual===expected)matches++;}\
         new check(check);check(undefined);\
         let evald=eval('('+check.toString()+')');new evald(evald);evald(undefined);\
         function outer(){\
           function nested(expected,actual=new.target){if(actual===expected)matches++;}\
           new nested(nested);nested(undefined);\
           let evaldNested=eval('('+nested.toString()+')');\
           new evaldNested(evaldNested);evaldNested(undefined);\
         }\
         outer();new outer();matches;",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(12))),
    );
}

#[test]
fn direct_eval_in_arrow_function_code_rejects_new_target() {
    evaluate(
        "let caught;let arrow=()=>eval('new.target;');try{arrow();}catch(error){caught=error;}typeof caught==='object'&&caught.constructor===SyntaxError;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_in_an_arrow_inherits_the_outer_function_environment() {
    evaluate(
        "function target(){return (()=>eval('new.target;'))();}target()===undefined&&new target()===target;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_super_call_initializes_the_derived_this_environment() {
    evaluate(
        "class Base{constructor(value){this.value=value;}}class Derived extends Base{constructor(){eval('super(42);');}}new Derived().value;",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_super_call_initializes_public_and_private_instance_elements() {
    evaluate(
        "let order=[];\
         class Base{constructor(...values){order.push('base:'+values.join(','));}}\
         class Derived extends Base{\
           answer=order.push('public');\
           #secret=order.push('private');\
           constructor(){let result=eval('super(...[1,2]);');order.push(result===this?'same':'different');}\
           secret(){return this.#secret;}\
         }\
         let value=new Derived();order.join('|')+'|'+value.answer+'|'+value.secret();",
        |value| assert_eq!(string(value), "base:1,2|public|private|same|2|3"),
    );
}

#[test]
fn nested_and_arrow_direct_eval_super_calls_initialize_instance_elements() {
    evaluate(
        "let count=0;class Base{}\
         class Nested extends Base{field=++count;constructor(){eval(\"eval('super()')\");}}\
         class EvalArrow extends Base{field=++count;constructor(){eval('(()=>super())()');}}\
         class OuterArrow extends Base{field=++count;constructor(){(()=>eval('super()'))();}}\
         let nested=new Nested();let evalArrow=new EvalArrow();let outerArrow=new OuterArrow();\
         nested.field+'|'+evalArrow.field+'|'+outerArrow.field+'|'+count;",
        |value| assert_eq!(string(value), "1|2|3|3"),
    );
}

#[test]
fn direct_eval_super_rejects_rebinding_without_reinitializing_elements() {
    evaluate(
        "let initializations=0;class Base{}class Derived extends Base{\
           field=++initializations;\
           constructor(){\
             eval('super()');let repeated=false;\
             try{eval('super()');}catch(error){repeated=error instanceof ReferenceError;}\
             this.repeated=repeated;\
           }\
         }\
         let value=new Derived();value.field+'|'+value.repeated+'|'+initializations;",
        |value| assert_eq!(string(value), "1|true|1"),
    );
}

#[test]
fn abrupt_eval_super_instance_initialization_leaves_this_bound() {
    evaluate(
        "let initializations=0;class Base{}class Derived extends Base{\
           field=(()=>{initializations++;throw 17;})();\
           constructor(){\
             let caught=false;try{eval('super()');}catch(error){caught=error===17;}\
             this.caught=caught;\
           }\
         }\
         let value=new Derived();value.caught+'|'+('field' in value)+'|'+initializations;",
        |value| assert_eq!(string(value), "true|false|1"),
    );
}

#[test]
fn nested_direct_eval_inherits_the_derived_constructor_environment() {
    evaluate(
        "class Base{constructor(value){this.value=value;}}class Derived extends Base{constructor(){eval(\"eval('super(42);')\");}}new Derived().value;",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn arrow_direct_eval_inherits_the_derived_constructor_environment() {
    evaluate(
        "class Base{constructor(value){this.value=value;}}class Derived extends Base{constructor(){(()=>eval('super(42);'))();}}new Derived().value;",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_inherits_the_method_home_object_for_super_properties() {
    evaluate(
        "let object={method(){return eval('super.answer;');}};Object.setPrototypeOf(object,{answer:42});object.method();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_indirect_eval_publishes_vars_but_not_lexicals() {
    evaluate(
        "(0, eval)(\"var evalVar = 1; let evalLexical = 2; evalVar + evalLexical;\"); evalVar + '|' + typeof evalLexical;",
        |value| assert_eq!(string(value), "1|undefined"),
    );
}

#[test]
fn strict_indirect_eval_keeps_var_declarations_local() {
    evaluate(
        "(0, eval)(\"'use strict'; var strictEvalVar = 1; strictEvalVar;\"); typeof strictEvalVar;",
        |value| assert_eq!(string(value), "undefined"),
    );
}

#[test]
fn sloppy_indirect_eval_publishes_function_declarations() {
    evaluate(
        "(0, eval)(\"function answer() { return 42; }\"); answer();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn indirect_eval_closures_keep_eval_lexicals_alive() {
    evaluate(
        "let closure = (0, eval)(\"let captured = 42; () => captured;\"); closure();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn indirect_eval_syntax_errors_are_catchable_javascript_exceptions() {
    evaluate(
        "try { (0, eval)(\"let = ;\"); false; } catch (error) { error instanceof SyntaxError; }",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn indirect_eval_global_lexical_collisions_throw_syntax_error() {
    evaluate(
        "let collision; try { (0, eval)(\"var collision;\"); false; } catch (error) { error.constructor === SyntaxError; }",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn indirect_eval_rejected_global_properties_throw_type_error() {
    evaluate(
        "Object.preventExtensions(globalThis); try { (0, eval)(\"var unavailable;\"); false; } catch (error) { error.constructor === TypeError; }",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}
