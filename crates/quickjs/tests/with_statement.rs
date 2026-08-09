use quickjs::{ScriptLimits, evaluate_script};
use quickjs_runtime::{JsNumber, Runtime, RuntimeLimits};

fn evaluate<T>(source: &str, inspect: impl FnOnce(&quickjs_runtime::JsValue) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let value = evaluate_script(
        &mut context,
        source,
        "with-statement.js",
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
fn with_reads_object_properties_and_falls_back_to_lexical_bindings() {
    evaluate(
        "let value=1;function read(object){with(object){return value;}}read({value:2})*10+read({});",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(21))),
    );
}

#[test]
fn with_honors_inherited_properties_accessors_and_symbol_unscopables() {
    evaluate(
        "let value=1;function read(object){with(object){return value;}}let inherited=Object.create({value:2});let receiver={marker:3,get value(){return this.marker;}};let blocked={value:4,[Symbol.unscopables]:{value:true}};''+read(inherited)+read(receiver)+read(blocked);",
        |value| assert_eq!(string(value), "231"),
    );
}

#[test]
fn with_lookup_preserves_observable_has_get_order() {
    evaluate(
        "let log=[];let target={value:42};let object=new Proxy(target,{has(t,k){log.push('h:'+(k===Symbol.unscopables?'u':k));return Reflect.has(t,k);},get(t,k,r){log.push('g:'+(k===Symbol.unscopables?'u':k));return Reflect.get(t,k,r);}});function read(object){with(object){return value;}}let result=read(object);result+'|'+log.join(',');",
        |value| assert_eq!(string(value), "42|h:value,g:u,h:value,g:value"),
    );
}

#[test]
fn closures_capture_fresh_with_objects_per_activation() {
    evaluate(
        "function read(object){with(object){return ()=>value;}}let one=read({value:1});let two=read({value:2});one()*10+two();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(12))),
    );
}

#[test]
fn nested_with_environments_are_consulted_from_inner_to_outer() {
    evaluate(
        "let value=1;function read(outer,inner){with(outer){with(inner){return value;}}}read({value:4},{})*10+read({value:4},{value:2});",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn captured_with_reads_preserve_strict_get_binding_value_semantics() {
    evaluate(
        "let count=0;let target={value:1};let object=new Proxy(target,{has(target,key){if(key==='value'&&++count===2)return false;return Reflect.has(target,key);}});function make(){with(object){return function(){'use strict';return value;};}}let read=make();try{read();false;}catch(error){error.constructor===ReferenceError&&count===2;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn typeof_uses_with_lookup_before_its_unresolvable_reference_fallback() {
    evaluate(
        "function kind(object){with(object){return typeof value;}}kind({value:1})+'|'+kind({});",
        |value| assert_eq!(string(value), "number|undefined"),
    );
}

#[test]
fn with_identifier_calls_use_the_object_environment_as_receiver() {
    evaluate(
        "function call(object){with(object){return method();}}call({marker:42,method(){return this.marker;}});",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn with_identifier_call_fallback_preserves_an_undefined_receiver() {
    evaluate(
        "let fallback=function(){'use strict';return this===undefined;};function call(object){with(object){return fallback();}}call({});",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn captured_strict_with_calls_preserve_get_binding_value_errors() {
    evaluate(
        "let count=0;let target={method(){return 1;}};let object=new Proxy(target,{has(target,key){if(key==='method'&&++count===2)return false;return Reflect.has(target,key);}});function make(){with(object){return function(){'use strict';return method();};}}let call=make();try{call();false;}catch(error){error.constructor===ReferenceError&&count===2;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn with_calls_and_tags_preserve_lookup_order_and_receiver() {
    evaluate(
        "let log=[];let target={marker:37,method(a,b){return this.marker+a+b;},tag(strings,value){return this.marker+strings[0]+value+strings[1];}};let object=new Proxy(target,{has(t,k){log.push('h:'+(k===Symbol.unscopables?'u':k));return Reflect.has(t,k);},get(t,k,r){log.push('g:'+(k===Symbol.unscopables?'u':k));return Reflect.get(t,k,r);}});function spread(object){with(object){return method(...[2,3]);}}function tagged(object){with(object){return tag`x${2}y`;}}spread(object)+'|'+tagged(object)+'|'+log.join(',');",
        |value| {
            assert_eq!(
                string(value),
                "42|37x2y|h:method,g:u,h:method,g:method,g:marker,h:tag,g:u,h:tag,g:tag,g:marker"
            );
        },
    );
}

#[test]
fn with_delete_resolves_own_inherited_blocked_and_missing_bindings() {
    evaluate(
        "let value=1;function remove(object){with(object){return delete value;}}let own={value:2};let inherited=Object.create({value:3});let blocked={value:4,[Symbol.unscopables]:{value:true}};let result=[remove(own),!('value'in own),remove(inherited),inherited.value,remove(blocked),blocked.value];result.join('|');",
        |value| assert_eq!(string(value), "true|true|true|3|false|4"),
    );
}

#[test]
fn with_delete_preserves_observable_has_unscopables_delete_order() {
    evaluate(
        "let log=[];let target={value:42};let object=new Proxy(target,{has(t,k){log.push('h:'+(k===Symbol.unscopables?'u':k));return Reflect.has(t,k);},get(t,k,r){log.push('g:'+(k===Symbol.unscopables?'u':k));return Reflect.get(t,k,r);},deleteProperty(t,k){log.push('d:'+k);return Reflect.deleteProperty(t,k);}});function remove(object){with(object){return delete value;}}remove(object)+'|'+('value'in target)+'|'+log.join(',');",
        |value| assert_eq!(string(value), "true|false|h:value,g:u,d:value"),
    );
}
