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
fn closed_direct_eval_returns_the_script_completion() {
    evaluate(
        "function local(){return eval('let answer=40+2;answer;');} local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
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
fn direct_eval_super_call_with_instance_fields_fails_closed() {
    evaluate(
        "class Base{}class Derived extends Base{answer=42;constructor(){eval('super();');}}let caught;try{new Derived();}catch(error){caught=error;}caught.constructor===SyntaxError;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
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
