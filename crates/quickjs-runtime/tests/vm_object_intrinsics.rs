//! `Object` constructor and reflection statics, pinned to the
//! `QuickJS` 2026-06-04 oracle.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object() no args => [object]
//! Object(obj) identity => [true]
//! Object.length => [1]
//! Object.name => [Object]
//! getPrototypeOf({}) => [true]
//! getPrototypeOf(5) => [true]
//! getPrototypeOf(null) !! TypeError: not an object
//! getPrototypeOf null-proto => [null]
//! setPrototypeOf returns target => [true]
//! setPrototypeOf primitive target => [5]
//! setPrototypeOf bad proto !! TypeError: not an object
//! setPrototypeOf nonextensible same => [true]
//! setPrototypeOf nonextensible diff !! TypeError: object is not extensible
//! preventExtensions returns => [true]
//! isExtensible fresh => [true]
//! isExtensible prevented => [false]
//! isExtensible primitive => [false]
//! seal keeps writable => [2]
//! seal blocks add => [undefined]
//! seal blocks delete => [false]
//! freeze blocks write => [1]
//! freeze delete => [false]
//! isSealed empty => [false]
//! isSealed prevented empty => [true]
//! isSealed sealed => [true]
//! isFrozen sealed => [false]
//! isFrozen frozen => [true]
//! isFrozen primitive => [true]
//! isSealed primitive => [true]
//! keys order => [0,2,b,a]
//! keys skips symbols => [a]
//! keys of string => [0,1,2]
//! keys true => [0]
//! keys undefined => [TypeError: cannot convert to object]
//! gOPN of array => [0,1,length]
//! keys after delete => [b,c,a]
//! keys independent => [true]
//! Object statics shape => [true]
//! Object.is SameValue => [true]
//! Object.hasOwn ordering => [true]
//! gOPS symbol phase => [true]
//! gOPDs materializes descriptors => [true]
//! proto ctor => [true]
//! ```

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsNumber, JsString, JsValue, OrdinaryDynamicFunctionCompiler,
    OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
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
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Object>"))
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

/// Evaluates `body` as a dynamic `Function` body and projects its result while
/// the owning runtime is still alive.
fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
}

fn boolean(body: &str) -> bool {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_boolean()
            .expect("live value")
            .expect("Boolean")
    })
}

/// Asserts the body evaluates to the exact Number `expected`.
fn assert_number(body: &str, expected: i32) {
    evaluate(body, |result| {
        let actual = result
            .expect("completed")
            .as_number()
            .expect("live value")
            .expect("Number");
        assert!(
            actual.strict_equals(JsNumber::from_i32(expected)),
            "{body} produced {actual:?}, expected {expected}"
        );
    });
}

fn text(body: &str) -> String {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn type_error_message(body: &str) -> String {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn assert_exception_kind(body: &str, expected: ExceptionKind) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        assert_eq!(exception.kind(), Some(expected));
    });
}

/// Joins an array's elements with commas.
///
/// `Array.prototype.join` is not part of the current profile, so the fixtures
/// build the comparison string explicitly rather than depending on it.
const JOIN: &str = "function join(a){var out=\"\";for(var i=0;i<a.length;i++){if(i>0){out+=\",\";}out+=a[i];}return out;}";

/// Oracle: `Object.length => [1]`, `Object.name => [Object]`,
/// `proto ctor => [true]`.
#[test]
fn the_object_constructor_has_the_pinned_identity() {
    assert_number("return Object.length;", 1);
    assert_eq!(text("return Object.name;"), "Object");
    assert!(boolean("return Object.prototype.constructor===Object;"));
}

/// Oracle: `Object(obj) identity => [true]`, `Object() no args => [object]`,
/// `Object(null) fresh => [true]`.
#[test]
fn the_object_constructor_coerces_with_to_object() {
    assert!(boolean("var o={};return Object(o)===o;"));
    assert!(boolean("var a=Object(null);var b=Object();return a!==b;"));
    assert!(boolean("return Object(5) instanceof Number;"));
}

/// Oracle: `getPrototypeOf({}) => [true]`, `getPrototypeOf(5) => [true]`,
/// `getPrototypeOf null-proto => [null]`,
/// `getPrototypeOf(null) !! TypeError: not an object`.
#[test]
fn get_prototype_of_reports_the_prototype_chain() {
    assert!(boolean(
        "return Object.getPrototypeOf({})===Object.prototype;"
    ));
    assert!(boolean(
        "return Object.getPrototypeOf(5)===Number.prototype;"
    ));
    assert!(boolean(
        "return Object.getPrototypeOf(\"s\")===String.prototype;"
    ));
    assert!(boolean(
        "return Object.getPrototypeOf(Object.create(null))===null;"
    ));
    assert_eq!(
        type_error_message("return Object.getPrototypeOf(null);"),
        "not an object"
    );
}

/// Oracle: `setPrototypeOf returns target => [true]`,
/// `setPrototypeOf primitive target => [5]`,
/// `setPrototypeOf bad proto !! TypeError: not an object`.
#[test]
fn set_prototype_of_returns_its_target() {
    assert!(boolean(
        "var o={};return Object.setPrototypeOf(o,null)===o;"
    ));
    assert!(boolean(
        "var base={m:1};var o={};Object.setPrototypeOf(o,base);return o.m===1;"
    ));
    assert_number("return Object.setPrototypeOf(5,null);", 5);
    assert_eq!(
        type_error_message("return Object.setPrototypeOf({},5);"),
        "not an object"
    );
}

/// Oracle: `setPrototypeOf nonextensible same => [true]` and
/// `setPrototypeOf nonextensible diff !! TypeError: object is not extensible`.
///
/// The same-value case succeeds before the extensibility test, which is the
/// early return at `quickjs.c:7940`.
#[test]
fn set_prototype_of_permits_a_same_value_write_on_a_non_extensible_object() {
    assert!(boolean(
        "var o=Object.preventExtensions({});\
         return Object.setPrototypeOf(o,Object.prototype)===o;"
    ));
    assert_eq!(
        type_error_message(
            "var o=Object.preventExtensions({});return Object.setPrototypeOf(o,null);"
        ),
        "object is not extensible"
    );
}

#[test]
fn object_prototype_has_immutable_prototype_exotic_semantics() {
    assert_eq!(
        text(
            "var same=Object.setPrototypeOf(Object.prototype,null)===Object.prototype;\
             var reflected=Reflect.setPrototypeOf(Object.prototype,{});\
             var threw=false;try{Object.setPrototypeOf(Object.prototype,{});}\
             catch(error){threw=error instanceof TypeError;}\
             return same+'|'+reflected+'|'+threw+'|'+\
               (Object.getPrototypeOf(Object.prototype)===null);"
        ),
        "true|false|true|true"
    );
}

#[test]
fn annex_b_object_prototype_extensions_are_absent() {
    assert_eq!(
        text(
            "let proto=Object.prototype;\
             let names=['__proto__','__defineGetter__','__defineSetter__','__lookupGetter__','__lookupSetter__'];\
             return Object.getOwnPropertyNames(proto).join('|')+'#'+\
                 names.every(function(name){return proto[name]===undefined&&\
                   Object.getOwnPropertyDescriptor(proto,name)===undefined;});"
        ),
        "toString|toLocaleString|valueOf|hasOwnProperty|isPrototypeOf|propertyIsEnumerable|constructor#true"
    );
}

#[test]
fn proto_named_object_literal_property_is_an_ordinary_own_data_property() {
    assert!(boolean(
        "let prototype={marker:1};let literal={__proto__:prototype};\
         return Object.getPrototypeOf(literal)===Object.prototype&&\
           Object.hasOwn(literal,'__proto__')&&literal.__proto__===prototype;"
    ));
}

/// Oracle: `preventExtensions returns => [true]`,
/// `isExtensible fresh => [true]`, `isExtensible prevented => [false]`,
/// `isExtensible primitive => [false]`,
/// `sloppy add nonext => [true]`.
#[test]
fn prevent_extensions_blocks_new_properties() {
    assert!(boolean("var o={};return Object.preventExtensions(o)===o;"));
    assert!(boolean("return Object.isExtensible({});"));
    assert!(!boolean(
        "return Object.isExtensible(Object.preventExtensions({}));"
    ));
    assert!(!boolean("return Object.isExtensible(5);"));
    assert!(boolean(
        "var o=Object.preventExtensions({});o.added=1;return o.added===undefined;"
    ));
    // An existing property stays writable after extensions are prevented.
    assert_number(
        "var o={x:1};Object.preventExtensions(o);o.x=2;return o.x;",
        2,
    );
}

/// Oracle: `seal keeps writable => [2]`, `seal blocks add => [undefined]`,
/// `seal blocks delete => [false]`.
#[test]
fn seal_keeps_values_writable_but_blocks_add_and_delete() {
    assert_number("var o={x:1};Object.seal(o);o.x=2;return o.x;", 2);
    assert!(boolean(
        "var o=Object.seal({});o.y=1;return o.y===undefined;"
    ));
    assert!(!boolean("var o=Object.seal({x:1});return delete o.x;"));
    assert!(boolean("var o={};return Object.seal(o)===o;"));
}

/// Oracle: `freeze blocks write => [1]`, `freeze delete => [false]`.
#[test]
fn freeze_blocks_writes_and_deletes() {
    assert_number("var o=Object.freeze({x:1});o.x=2;return o.x;", 1);
    assert!(!boolean("var o=Object.freeze({x:1});return delete o.x;"));
}

/// Oracle: `isSealed empty => [false]`, `isSealed prevented empty => [true]`,
/// `isSealed sealed => [true]`, `isFrozen sealed => [false]`,
/// `isFrozen frozen => [true]`.
///
/// An extensible object is neither sealed nor frozen even with no own
/// properties, so the extensibility bit is tested first.
#[test]
fn integrity_level_tests_require_non_extensibility_first() {
    assert!(!boolean("return Object.isSealed({});"));
    assert!(!boolean("return Object.isFrozen({});"));
    assert!(boolean(
        "return Object.isSealed(Object.preventExtensions({}));"
    ));
    assert!(boolean(
        "return Object.isFrozen(Object.preventExtensions({}));"
    ));
    assert!(boolean("return Object.isSealed(Object.seal({x:1}));"));
    // A sealed data property is still writable, so it is not frozen.
    assert!(!boolean("return Object.isFrozen(Object.seal({x:1}));"));
    assert!(boolean("return Object.isFrozen(Object.freeze({x:1}));"));
    assert!(boolean("return Object.isSealed(Object.freeze({x:1}));"));
}

#[test]
fn proxy_integrity_levels_follow_internal_method_order() {
    assert_eq!(
        text(
            "var log='';var target={x:1};var proxy=new Proxy(target,{\
               preventExtensions(t){log=log+'p,';return Reflect.preventExtensions(t);},\
               ownKeys(t){log=log+'k,';return Reflect.ownKeys(t);},\
               defineProperty(t,k,d){log=log+'d:'+k+':'+d.configurable+':'+\
                 ('writable' in d)+',';return Reflect.defineProperty(t,k,d);}});\
             var result=Object.seal(proxy);return (result===proxy)+'|'+log+'|'+\
               Object.isSealed(target)+'|'+target.x;"
        ),
        "true|p,k,d:x:false:false,|true|1"
    );
    assert_eq!(
        text(
            "var log='';var target={x:1};var proxy=new Proxy(target,{\
               preventExtensions(t){log=log+'p,';return Reflect.preventExtensions(t);},\
               ownKeys(t){log=log+'k,';return Reflect.ownKeys(t);},\
               getOwnPropertyDescriptor(t,k){log=log+'g:'+k+',';\
                 return Reflect.getOwnPropertyDescriptor(t,k);},\
               defineProperty(t,k,d){log=log+'d:'+k+':'+d.configurable+':'+\
                 d.writable+',';return Reflect.defineProperty(t,k,d);}});\
             Object.freeze(proxy);return log+'|'+Object.isFrozen(target);"
        ),
        "p,k,g:x,d:x:false:false,|true"
    );
    assert_eq!(
        text(
            "var log='';var target=Object.freeze({x:1});var proxy=new Proxy(target,{\
               isExtensible(t){log=log+'e,';return Reflect.isExtensible(t);},\
               ownKeys(t){log=log+'k,';return Reflect.ownKeys(t);},\
               getOwnPropertyDescriptor(t,k){log=log+'g:'+k+',';\
                 return Reflect.getOwnPropertyDescriptor(t,k);}});\
             return Object.isFrozen(proxy)+'|'+log;"
        ),
        "true|e,k,g:x,"
    );
    assert_eq!(
        text(
            "var log='';var proxy=new Proxy({},{\
               preventExtensions(){log=log+'p,';return false;},\
               ownKeys(){log=log+'k,';return [];}});\
             try{Object.freeze(proxy);}catch(error){\
               return (error instanceof TypeError)+'|'+log;}return 'missed';"
        ),
        "true|p,"
    );
    assert_eq!(
        text(
            "var log='';var proxy=new Proxy({},{\
               isExtensible(t){log=log+'e,';return Reflect.isExtensible(t);},\
               ownKeys(){log=log+'k,';return [];}});\
             return Object.isFrozen(proxy)+'|'+log;"
        ),
        "false|e,"
    );
}

#[test]
fn for_in_uses_proxy_enumeration_internal_methods() {
    assert_eq!(
        text(
            "var log='';var proto=new Proxy({p:1},{\
               ownKeys(t){log+='P';return ['p'];},\
               getOwnPropertyDescriptor(t,k){log+='Q'+k;\
                 return {value:1,writable:true,enumerable:true,configurable:true};},\
               getPrototypeOf(){log+='H';return null;}});\
             var target={a:1,b:2};var source=new Proxy(target,{\
               ownKeys(t){log+='K';return ['a','b'];},\
               getOwnPropertyDescriptor(t,k){log+='D'+k;\
                 if(k==='a'){delete t.b;return {value:1,writable:true,\
                   enumerable:true,configurable:true};}return;},\
               getPrototypeOf(){log+='G';return proto;}});\
             var keys='';for(var key in source)keys+=key;\
             var hidden=new Proxy({x:1},{\
               getOwnPropertyDescriptor(){return {value:1,writable:true,\
                 enumerable:false,configurable:true};},\
               getPrototypeOf(){return {x:2};}});\
             var suppressed='';for(var hiddenKey in hidden)suppressed+=hiddenKey;\
             return keys+'|'+log+'|'+suppressed;"
        ),
        "ap|KDaDbGPQpH|"
    );
}

/// Oracle: `isFrozen primitive => [true]`, `isSealed primitive => [true]`.
/// A primitive has no own properties to reconfigure.
#[test]
fn a_primitive_is_vacuously_sealed_and_frozen() {
    assert!(boolean("return Object.isSealed(5);"));
    assert!(boolean("return Object.isFrozen(5);"));
    assert!(boolean("return Object.isSealed(true);"));
}

/// Oracle: `keys order => [0,2,b,a]`. `[[OwnPropertyKeys]]` reports array
/// indices in ascending numeric order before string keys in creation order.
#[test]
fn object_keys_uses_the_pinned_own_key_order() {
    assert_eq!(
        text(&format!(
            "{JOIN}var o={{b:1}};o[2]=1;o.a=1;o[0]=1;return join(Object.keys(o));"
        )),
        "0,2,b,a"
    );
}

/// Oracle: `keys after delete => [b,c,a]`. Deleting compacts the key order, so
/// a re-added key moves to the end.
#[test]
fn object_keys_reflects_deletion_and_re_addition_order() {
    assert_eq!(
        text(&format!(
            "{JOIN}var o={{a:1,b:2,c:3}};delete o.a;o.a=4;return join(Object.keys(o));"
        )),
        "b,c,a"
    );
}

/// Oracle: `keys skips symbols => [a]`. `Object.keys` reports only
/// string-keyed properties.
#[test]
fn object_keys_omits_symbol_keys() {
    assert_eq!(
        text(&format!(
            "{JOIN}var o={{}};o[Symbol(\"s\")]=1;o.a=2;return join(Object.keys(o));"
        )),
        "a"
    );
}

/// Oracle: `keys of string => [0,1,2]` and `keys true => [0]`.
///
/// A primitive string is boxed so its index keys are reported; every other
/// primitive has no own keys.
#[test]
fn object_keys_boxes_a_primitive_string_and_ignores_other_primitives() {
    assert_eq!(
        text(&format!("{JOIN}return join(Object.keys(\"abc\"));")),
        "0,1,2"
    );
    assert_number("return Object.keys(5).length;", 0);
    assert_number("return Object.keys(true).length;", 0);
}

/// Oracle: `keys undefined => [TypeError: cannot convert to object]`.
///
/// The `keys` family reports the `ToObject` failure, unlike `getPrototypeOf`,
/// which reports `not an object`.
#[test]
fn object_keys_reports_the_to_object_failure_for_a_nullish_argument() {
    assert_eq!(
        type_error_message("return Object.keys(null);"),
        "cannot convert to object"
    );
    assert_eq!(
        type_error_message("return Object.keys(undefined);"),
        "cannot convert to object"
    );
}

/// Oracle: `gOPN of array => [0,1,length]`. Unlike `keys`, this listing
/// includes non-enumerable own properties.
#[test]
fn get_own_property_names_includes_non_enumerable_properties() {
    assert_eq!(
        text(&format!(
            "{JOIN}return join(Object.getOwnPropertyNames([1,2]));"
        )),
        "0,1,length"
    );
    assert_eq!(
        text(&format!(
            "{JOIN}return join(Object.getOwnPropertyNames(\"ab\"));"
        )),
        "0,1,length"
    );
    // `Object.keys` omits the same array `length`.
    assert_eq!(
        text(&format!("{JOIN}return join(Object.keys([1,2]));")),
        "0,1"
    );
}

/// Oracle: `keys independent => [true]`. Each call returns a fresh ordinary
/// array, so mutating one result cannot disturb the object or another result.
#[test]
fn object_keys_returns_an_independent_array() {
    assert!(boolean(
        "var o={a:1};\
         var first=Object.keys(o);\
         var second=Object.keys(o);\
         first[0]=\"changed\";\
         return first!==second&&second[0]===\"a\"&&o.a===1;"
    ));
}

/// ECMA-262 20.1.2 installs these `Object` statics with ordinary built-in
/// identities. Their relative own-property order follows the pinned `QuickJS`
/// constructor table and the complete ECMA-262 2025 static surface is present.
#[test]
fn the_extended_object_reflection_statics_have_the_specification_shape() {
    assert!(boolean(
        "var specs=[['is',2],['hasOwn',2],['getOwnPropertySymbols',1],['groupBy',2],\
          ['getOwnPropertyDescriptors',1],['values',1],['entries',1],['assign',2],\
          ['defineProperties',2],['fromEntries',1]];\
         for(var i=0;i<specs.length;i++){\
           var name=specs[i][0],method=Object[name];\
           var descriptor=Object.getOwnPropertyDescriptor(Object,name);\
           if(typeof method!=='function'||method.name!==name||\
              method.length!==specs[i][1]||!descriptor.writable||\
              descriptor.enumerable||!descriptor.configurable){return false;}\
         }return true;"
    ));
    assert_eq!(
        text("return Object.getOwnPropertyNames(Object).join(',');"),
        "length,name,create,getPrototypeOf,setPrototypeOf,defineProperty,defineProperties,\
getOwnPropertyNames,getOwnPropertySymbols,groupBy,keys,values,entries,isExtensible,preventExtensions,\
getOwnPropertyDescriptor,getOwnPropertyDescriptors,is,assign,seal,freeze,isSealed,\
isFrozen,fromEntries,hasOwn,prototype"
    );
}

/// `Object.defineProperties` converts every selected descriptor before it
/// applies the first definition. A later invalid descriptor therefore leaves
/// the target untouched even though all preceding descriptor getters ran.
#[test]
fn object_define_properties_collects_before_applying() {
    assert_eq!(
        text(
            "var log='',target={},properties={};\
             function readA(){log=log+'A';return {value:1};}\
             function readB(){log=log+'B';return {get:1};}\
             Object.defineProperty(properties,'a',{get:readA,enumerable:true});\
             Object.defineProperty(properties,'b',{get:readB,enumerable:true});\
             try{Object.defineProperties(target,properties);}catch(error){\
               return log+'|'+error.name+'|'+Object.hasOwn(target,'a');}\
             return 'missing error';"
        ),
        "AB|TypeError|false"
    );
    assert_eq!(
        text(
            "var log='',properties={};function read(){log=log+'g';return {value:1};}\
             Object.defineProperty(properties,'x',{get:read,enumerable:true});\
             try{Object.defineProperties(1,properties);}catch(error){}return log;"
        ),
        ""
    );
    assert_eq!(
        type_error_message("return Object.defineProperties({},null);"),
        "cannot convert to object"
    );
}

/// The own-key list is snapshotted once while descriptor enumerability and
/// values are re-read per key after getter re-entry. String and Symbol keys
/// retain `[[OwnPropertyKeys]]` order in the application phase.
#[test]
fn object_define_properties_snapshots_keys_and_rechecks_descriptors() {
    assert!(boolean(
        "var symbol=Symbol('s'),properties={},target={};\
         function first(){delete properties.deleted;properties.added={value:4};\
           Object.defineProperty(properties,'hidden',{enumerable:true});return {value:1};}\
         Object.defineProperty(properties,'a',{get:first,enumerable:true});\
         Object.defineProperty(properties,'hidden',{value:{value:2},configurable:true});\
         properties.deleted={value:3};properties[symbol]={value:5,enumerable:true};\
         Object.defineProperties(target,properties);\
         return target.a===1&&target.hidden===2&&!Object.hasOwn(target,'deleted')&&\
           !Object.hasOwn(target,'added')&&target[symbol]===5&&\
           Reflect.ownKeys(target).map(function(key){return typeof key==='symbol'?'s':key;}).join(',')==='a,hidden,s';"
    ));
}

/// Once collection succeeds, definitions are applied in key order. An abrupt
/// later definition preserves earlier completed properties and Array length
/// definitions reuse the resumable `ArraySetLength` conversion.
#[test]
fn object_define_properties_applies_in_order_with_partial_completion() {
    assert!(boolean(
        "var target={};Object.defineProperty(target,'fixed',{value:1});\
         try{Object.defineProperties(target,{before:{value:2},fixed:{value:3},after:{value:4}});}\
         catch(error){return error instanceof TypeError&&target.before===2&&\
           target.fixed===1&&!Object.hasOwn(target,'after');}return false;"
    ));
    assert_eq!(
        text(
            "var log='';function lengthValue(){log=log+'v';return 2;}\
             var array=[1,2,3];Object.defineProperties(array,{\
               length:{value:{valueOf:lengthValue},writable:false}});\
             return array.length+'|'+Object.getOwnPropertyDescriptor(array,'length').writable+'|'+log;"
        ),
        "2|false|vv"
    );
}

/// `ObjectDefineProperties` uses the source and target internal methods in the
/// normative collect-then-apply order, including when either side is a Proxy.
#[test]
fn object_define_properties_routes_proxy_internal_methods() {
    assert_eq!(
        text(
            "var log='';var sourceTarget={a:{value:1},b:{value:2}};\
             var source=new Proxy(sourceTarget,{\
               ownKeys(t){log=log+'K';return ['a','b'];},\
               getOwnPropertyDescriptor(t,k){log=log+'D'+k;return {\
                 value:t[k],writable:true,enumerable:true,configurable:true};},\
               get(t,k,r){log=log+'G'+k;return Reflect.get(t,k,r);}});\
             var targetValue={};var target=new Proxy(targetValue,{\
               defineProperty(t,k,d){log=log+'F'+k+d.value;return Reflect.defineProperty(t,k,d);}});\
             var result=Object.defineProperties(target,source);\
             return (result===target)+'|'+targetValue.a+'|'+targetValue.b+'|'+log;"
        ),
        "true|1|2|KDaGaDbGbFa1Fb2"
    );
    assert_exception_kind(
        "return Object.defineProperties(new Proxy({},{defineProperty(){return false;}}),\
           {x:{value:1}});",
        ExceptionKind::TypeError,
    );
}

/// `Object.fromEntries` creates one fresh ordinary object, converts every key
/// with `ToPropertyKey`, accepts Symbols, and overwrites duplicate values
/// through fully writable, enumerable, configurable data properties.
#[test]
fn object_from_entries_creates_data_properties() {
    assert!(boolean(
        "var symbol=Symbol('s');var result=Object.fromEntries([\
           ['a',1],[symbol,2],[{toString:function key(){return 'a';}},3],\
           ['__proto__',4]]);\
         var descriptor=Object.getOwnPropertyDescriptor(result,'a');\
         return Object.getPrototypeOf(result)===Object.prototype&&result.a===3&&\
           result[symbol]===2&&Object.hasOwn(result,'__proto__')&&result.__proto__===4&&\
           descriptor.writable&&descriptor.enumerable&&descriptor.configurable;"
    ));
}

/// Entry index `0` and `1` are read before `ToPropertyKey` runs, preserving
/// the `AddEntriesFromIterable` order across getter and conversion re-entry.
#[test]
fn object_from_entries_preserves_entry_and_key_conversion_order() {
    assert_eq!(
        text(
            "var log='',step=0;var key={toString:function keyString(){log=log+'k';return 'x';}};\
             var entry={};function readKey(){log=log+'0';return key;}\
             function readValue(){log=log+'1';return 7;}\
             Object.defineProperty(entry,'0',{get:readKey});\
             Object.defineProperty(entry,'1',{get:readValue});\
             var iterator={next:function next(){\
               return step++===0?{done:false,value:entry}:{done:true};}};\
             var iterable={};iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             var result=Object.fromEntries(iterable);return log+'|'+result.x;"
        ),
        "01k|7"
    );
}

/// Every abrupt completion after iterator acquisition performs
/// `IteratorClose`, while a throwing close preserves the original exception.
#[test]
fn object_from_entries_closes_on_abrupt_completion() {
    assert!(boolean(
        "var original={},secondary={},closed=false,step=0;var entry={};\
         function readKey(){throw original;}\
         Object.defineProperty(entry,'0',{get:readKey});entry[1]=1;\
         var iterator={next:function next(){return step++===0?\
           {done:false,value:entry}:{done:true};},\
           return:function close(){closed=true;throw secondary;}};\
         var iterable={};iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
         try{Object.fromEntries(iterable);}catch(error){return error===original&&closed;}\
         return false;"
    ));
    assert!(boolean(
        "var closed=false,step=0;var iterator={\
           next:function next(){return step++===0?{done:false,value:1}:{done:true};},\
           return:function close(){closed=true;return {};}};\
         var iterable={};iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
         try{Object.fromEntries(iterable);}catch(error){\
           return error instanceof TypeError&&closed;}return false;"
    ));
    assert!(boolean(
        "var original={},closed=false,step=0;\
         var key={toString:function keyString(){throw original;}};\
         var iterator={next:function next(){return step++===0?\
           {done:false,value:[key,1]}:{done:true};},\
           return:function close(){closed=true;return {};}};\
         var iterable={};iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
         try{Object.fromEntries(iterable);}catch(error){return error===original&&closed;}\
         return false;"
    ));
}

/// Abrupt completions produced while obtaining the next iterator value are
/// propagated by `IteratorStepValue` itself. They happen before
/// `AddEntriesFromIterable` reaches an operation guarded by
/// `IfAbruptCloseIterator`, so they must not invoke the iterator's `return`.
#[test]
fn object_from_entries_does_not_close_iterator_step_failures() {
    assert_eq!(
        text(
            "var closed=false;var iterator={next:function next(){throw 'next';},\
             return:function close(){closed=true;return {};}};var iterable={};\
             iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             try{Object.fromEntries(iterable);}catch(error){}return ''+closed;"
        ),
        "false"
    );
    assert_eq!(
        text(
            "var closed=false;var iterator={next:function next(){return 1;},\
             return:function close(){closed=true;return {};}};var iterable={};\
             iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             try{Object.fromEntries(iterable);}catch(error){}return ''+closed;"
        ),
        "false"
    );
    assert_eq!(
        text(
            "var closed=false;var iterator={next:function next(){\
               return {get done(){throw 'done';}};},\
             return:function close(){closed=true;return {};}};var iterable={};\
             iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             try{Object.fromEntries(iterable);}catch(error){}return ''+closed;"
        ),
        "false"
    );
    assert_eq!(
        text(
            "var closed=false;var iterator={next:function next(){\
               return {done:false,get value(){throw 'value';}};},\
             return:function close(){closed=true;return {};}};var iterable={};\
             iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             try{Object.fromEntries(iterable);}catch(error){}return ''+closed;"
        ),
        "false"
    );
}

/// `Object.groupBy` calls its callback with `(value, index)` and `undefined`
/// as `this`, converts each answer with `ToPropertyKey`, and materializes
/// first-seen groups as realm arrays on a null-prototype ordinary object.
#[test]
fn object_group_by_creates_null_prototype_array_groups() {
    assert!(boolean(
        "var symbol=Symbol('s'),log='';\
         function callback(value,index){'use strict';\
           log=log+value+index+(this===undefined?'u':'x');\
           if(value===4){return symbol;}\
           if(value===3){return {toString:function key(){log=log+'k';return 'odd';}};}\
           return value===2?'__proto__':'odd';}\
         var result=Object.groupBy([1,2,3,4],callback);\
         var odd=Object.getOwnPropertyDescriptor(result,'odd');\
         return Object.getPrototypeOf(result)===null&&log==='10u21u32uk43u'&&\
           result.odd.join(',')==='1,3'&&result.__proto__.join(',')==='2'&&\
           result[symbol].join(',')==='4'&&Array.isArray(result.odd)&&\
           odd.writable&&odd.enumerable&&odd.configurable;"
    ));
}

/// Callback execution and `ToPropertyKey` alternate once per yielded value;
/// equal converted keys append to the same group without disturbing element
/// order or the order in which group properties are created.
#[test]
fn object_group_by_preserves_callback_and_key_conversion_order() {
    assert_eq!(
        text(
            "var log='',step=0;var iterator={next:function next(){\
               return step<3?{done:false,value:++step}:{done:true};}};\
             var iterable={};iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             function callback(value,index){log=log+'c'+value+index;return {\
               toString:function key(){log=log+'k'+value;return value===2?'b':'a';}};}\
             var result=Object.groupBy(iterable,callback);\
             return log+'|'+Object.keys(result).join(',')+'|'+result.a.join(',')+'|'+result.b.join(',');"
        ),
        "c10k1c21k2c32k3|a,b|1,3|2"
    );
}

/// Callback and property-key conversion failures are the two abrupt `GroupBy`
/// operations guarded by `IfAbruptCloseIterator`. A throwing `return` still
/// preserves the original throw completion.
#[test]
fn object_group_by_closes_post_yield_abrupt_completions() {
    assert!(boolean(
        "var original={},secondary={},closed=false,step=0;var iterator={\
           next:function next(){return step++===0?{done:false,value:1}:{done:true};},\
           return:function close(){closed=true;throw secondary;}};var iterable={};\
         iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
         function callback(){throw original;}\
         try{Object.groupBy(iterable,callback);}catch(error){return error===original&&closed;}\
         return false;"
    ));
    assert!(boolean(
        "var original={},closed=false,step=0;var iterator={\
           next:function next(){return step++===0?{done:false,value:1}:{done:true};},\
           return:function close(){closed=true;return {};}};var iterable={};\
         iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
         function callback(){return {toString:function key(){throw original;}};}\
         try{Object.groupBy(iterable,callback);}catch(error){return error===original&&closed;}\
         return false;"
    ));
}

/// Callback validation precedes iterator acquisition. Once an Iterator Record
/// exists, `IteratorStepValue` failures still propagate without closing it.
#[test]
fn object_group_by_validates_callback_and_does_not_close_step_failures() {
    assert_eq!(
        text(
            "var touched=false,iterable={};Object.defineProperty(iterable,Symbol.iterator,{\
               get:function iteratorGetter(){touched=true;return function(){};}});\
             try{Object.groupBy(iterable,null);}catch(error){}return ''+touched;"
        ),
        "false"
    );
    assert_eq!(
        text(
            "var closed=false;var iterator={next:function next(){throw 'next';},\
             return:function close(){closed=true;return {};}};var iterable={};\
             iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             try{Object.groupBy(iterable,function(){});}catch(error){}return ''+closed;"
        ),
        "false"
    );
    assert_eq!(
        text(
            "var closed=false;var iterator={next:function next(){return {\
               done:false,get value(){throw 'value';}};},\
             return:function close(){closed=true;return {};}};var iterable={};\
             iterable[Symbol.iterator]=function iteratorMethod(){return iterator;};\
             try{Object.groupBy(iterable,function(){});}catch(error){}return ''+closed;"
        ),
        "false"
    );
}

/// `Object.is` is exactly ECMA-262 `SameValue`: unlike strict equality it
/// equates NaNs and distinguishes the two signed zeros while retaining object
/// and Symbol identity.
#[test]
fn object_is_uses_same_value() {
    assert!(boolean(
        "var object={};var symbol=Symbol('s');\
         return Object.is(NaN,NaN)&&!Object.is(0,-0)&&Object.is(-0,-0)&&\
           Object.is(object,object)&&!Object.is({}, {})&&\
           Object.is(symbol,symbol)&&!Object.is(Symbol('s'),symbol)&&\
           Object.is()&&!Object.is(undefined,null);"
    ));
}

/// ECMA-262 `Object.hasOwn` applies `ToObject(O)` before `ToPropertyKey(P)`.
/// This differs from `Object.prototype.hasOwnProperty`, whose key conversion
/// precedes the receiver conversion.
#[test]
fn object_has_own_boxes_primitives_and_preserves_conversion_order() {
    assert!(boolean(
        "var inherited={x:1};var object=Object.create(inherited);object.y=2;\
         return Object.hasOwn(object,'y')&&!Object.hasOwn(object,'x')&&\
           Object.hasOwn('ab',0)&&Object.hasOwn('ab','length')&&\
           !Object.hasOwn(5,'valueOf');"
    ));
    assert_eq!(
        text(
            "var log='';function keyToString(){log=log+'k';return 'x';}\
             var key={toString:keyToString};\
             try{Object.hasOwn(null,key);}catch(error){log=log+'e';}\
             var object={x:1};var found=Object.hasOwn(object,key);\
             return log+'|'+found;"
        ),
        "ek|true"
    );
    assert_eq!(
        type_error_message("return Object.hasOwn(undefined,'x');"),
        "cannot convert to object"
    );
}

/// `Object.getOwnPropertySymbols` projects only the symbol phase of
/// `[[OwnPropertyKeys]]`, retaining creation order and returning a fresh Array.
#[test]
fn get_own_property_symbols_reports_only_symbol_keys_in_order() {
    assert!(boolean(
        "var first=Symbol('first'),second=Symbol('second');var object={x:1};\
         object[second]=2;object[first]=1;delete object[second];object[second]=3;\
         var a=Object.getOwnPropertySymbols(object);\
         var b=Object.getOwnPropertySymbols(object);\
         return a!==b&&a.length===2&&a[0]===first&&a[1]===second&&\
           b[0]===first&&Object.getOwnPropertySymbols('ab').length===0;"
    ));
    assert_eq!(
        type_error_message("return Object.getOwnPropertySymbols(null);"),
        "cannot convert to object"
    );
}

/// `Object.getOwnPropertyDescriptors` snapshots all own keys, materializes one
/// fresh descriptor per still-present property, and defines those descriptor
/// values as ordinary mutable properties on a fresh ordinary result object.
#[test]
fn get_own_property_descriptors_materializes_all_descriptor_kinds() {
    assert!(boolean(
        "var symbol=Symbol('s');function getter(){return 7;}function setter(v){}\
         var object={};Object.defineProperty(object,'data',{value:3,writable:false,\
           enumerable:true,configurable:false});\
         Object.defineProperty(object,'accessor',{get:getter,set:setter,\
           enumerable:false,configurable:true});object[symbol]=9;\
         var descriptors=Object.getOwnPropertyDescriptors(object);\
         var keys=Reflect.ownKeys(descriptors);\
         var data=descriptors.data,accessor=descriptors.accessor;\
         var resultDescriptor=Object.getOwnPropertyDescriptor(descriptors,'data');\
         data.value=11;\
         return Object.getPrototypeOf(descriptors)===Object.prototype&&\
           keys.length===3&&keys[0]==='data'&&keys[1]==='accessor'&&keys[2]===symbol&&\
           data.value===11&&object.data===3&&!data.writable&&data.enumerable&&\
           !data.configurable&&accessor.get===getter&&accessor.set===setter&&\
           !accessor.enumerable&&accessor.configurable&&\
           resultDescriptor.writable&&resultDescriptor.enumerable&&\
           resultDescriptor.configurable&&descriptors[symbol].value===9;"
    ));
}

/// Primitive Strings expose virtual index and `length` own properties to
/// `ToObject`; other non-nullish primitives produce an empty descriptor map.
#[test]
fn get_own_property_descriptors_observes_primitive_string_exotics() {
    assert!(boolean(
        "var descriptors=Object.getOwnPropertyDescriptors('ab');\
         var zero=descriptors[0],length=descriptors.length;\
         return Reflect.ownKeys(descriptors).join(',')==='0,1,length'&&\
           zero.value==='a'&&!zero.writable&&zero.enumerable&&!zero.configurable&&\
           length.value===2&&!length.writable&&!length.enumerable&&!length.configurable&&\
           Reflect.ownKeys(Object.getOwnPropertyDescriptors(5)).length===0;"
    ));
    assert_eq!(
        type_error_message("return Object.getOwnPropertyDescriptors(null);"),
        "cannot convert to object"
    );
}

/// The key list is captured through `[[OwnPropertyKeys]]`, then each current
/// descriptor is obtained through `[[GetOwnProperty]]` in list order.
#[test]
fn get_own_property_descriptors_routes_proxy_internal_methods() {
    assert_eq!(
        text(
            "var log='';var target={a:1,b:2};var proxy=new Proxy(target,{\
               ownKeys(){log=log+'K';return ['a','b'];},\
               getOwnPropertyDescriptor(t,k){log=log+'D'+k;\
                 if(k==='a'){delete t.b;return {value:1,writable:false,\
                   enumerable:true,configurable:true};}return undefined;}});\
             var descriptors=Object.getOwnPropertyDescriptors(proxy);\
             var descriptor=descriptors.a;\
             return Reflect.ownKeys(descriptors).join(',')+'|'+descriptor.value+'|'+\
               descriptor.writable+'|'+descriptor.enumerable+'|'+\
               descriptor.configurable+'|'+log;"
        ),
        "a|1|false|true|true|KDaDb"
    );
}

/// `Object.values` and `Object.entries` use the string-key phase of
/// `[[OwnPropertyKeys]]`, filter on the current descriptor's enumerability, and
/// read values from left to right while omitting symbols and hidden properties.
#[test]
fn object_values_and_entries_follow_enumerable_own_property_order() {
    assert!(boolean(
        "var symbol=Symbol('s'),object={b:2};object[2]='two';object.a=1;\
         object[0]='zero';Object.defineProperty(object,'hidden',{value:9});\
         object[symbol]=3;var values=Object.values(object);\
         var entries=Object.entries(object);\
         return values.join(',')==='zero,two,2,1'&&entries.length===4&&\
           entries[0][0]==='0'&&entries[0][1]==='zero'&&\
           entries[1][0]==='2'&&entries[1][1]==='two'&&\
           entries[2][0]==='b'&&entries[2][1]===2&&\
           entries[3][0]==='a'&&entries[3][1]===1;"
    ));
}

/// The own-key list is fixed before the first getter, but each later key's
/// descriptor is re-read. A getter can therefore expose or delete a snapshotted
/// key, while a newly added key is not visited.
#[test]
fn enumerable_own_properties_rechecks_descriptors_after_each_getter() {
    assert!(boolean(
        "var log='';var object={};\
         function first(){log=log+'a';\
           Object.defineProperty(object,'hidden',{enumerable:true});\
           delete object.deleted;object.added=4;return 1;}\
         Object.defineProperty(object,'a',{get:first,enumerable:true});\
         Object.defineProperty(object,'hidden',{value:2,configurable:true});\
         object.deleted=3;var values=Object.values(object);\
         return values.length===2&&values[0]===1&&values[1]===2&&log==='a';"
    ));
}

/// A non-enumerable accessor is never invoked. Enumerable getters receive the
/// original object as `this`, and abrupt completion is propagated unchanged.
#[test]
fn enumerable_own_properties_runs_only_selected_getters_and_propagates_throws() {
    assert!(boolean(
        "var log='';var object={};var marker={};\
         function hidden(){log=log+'h';return 1;}\
         function visible(){log=log+'v';return this===object?7:0;}\
         Object.defineProperty(object,'hidden',{get:hidden});\
         Object.defineProperty(object,'visible',{get:visible,enumerable:true});\
         var values=Object.values(object);if(values[0]!==7||log!=='v'){return false;}\
         function boom(){throw marker;}\
         Object.defineProperty(object,'boom',{get:boom,enumerable:true});\
         try{Object.entries(object);}catch(error){return error===marker;}return false;"
    ));
}

/// `ToObject` exposes primitive String index properties; other non-nullish
/// primitives have no enumerable own properties and nullish inputs throw.
#[test]
fn object_values_and_entries_box_primitive_strings() {
    assert!(boolean(
        "var values=Object.values('ab'),entries=Object.entries('ab');\
         return values.length===2&&values[0]==='a'&&values[1]==='b'&&\
           entries.length===2&&entries[0][0]==='0'&&entries[0][1]==='a'&&\
           entries[1][0]==='1'&&entries[1][1]==='b'&&\
           Object.values(5).length===0&&Object.entries(true).length===0;"
    ));
    assert_eq!(
        type_error_message("return Object.values(null);"),
        "cannot convert to object"
    );
    assert_eq!(
        type_error_message("return Object.entries(undefined);"),
        "cannot convert to object"
    );
}

/// `Object.assign` has the standard built-in identity and descriptor.
#[test]
fn object_assign_has_the_specification_shape() {
    assert!(boolean(
        "var descriptor=Object.getOwnPropertyDescriptor(Object,'assign');\
         return typeof Object.assign==='function'&&Object.assign.name==='assign'&&\
           Object.assign.length===2&&descriptor.writable&&!descriptor.enumerable&&\
           descriptor.configurable;"
    ));
}

/// Sources are visited left to right. Each source snapshots all own keys,
/// filters current enumerable descriptors, reads getters, and copies both
/// String and Symbol keys with strict `Set` semantics.
#[test]
fn object_assign_copies_sources_strings_and_symbols_in_order() {
    assert!(boolean(
        "var symbol=Symbol('s'),log='';var first={};\
         function readA(){log=log+'a';return 1;}\
         Object.defineProperty(first,'a',{get:readA,enumerable:true});first[symbol]=2;\
         var target=Object.assign({},first,null,undefined,'bc',{a:4});\
         return log==='a'&&target.a===4&&target[symbol]===2&&\
           target[0]==='b'&&target[1]==='c'&&\
           Object.getOwnPropertySymbols(target)[0]===symbol;"
    ));
}

/// The target is coerced once with `ToObject`, so non-nullish primitives return
/// their mutable wrapper while a nullish target fails before any source Get.
#[test]
fn object_assign_boxes_its_target_and_validates_it_before_sources() {
    assert!(boolean(
        "var boxed=Object.assign(1,{a:2});\
         return typeof boxed==='object'&&boxed.valueOf()===1&&boxed.a===2;"
    ));
    assert_eq!(
        text(
            "var log='';function read(){log=log+'g';return 1;}\
             var source={};Object.defineProperty(source,'x',{get:read,enumerable:true});\
             try{Object.assign(null,source);}catch(error){}return log;"
        ),
        ""
    );
    assert_eq!(
        type_error_message("return Object.assign(undefined,{x:1});"),
        "cannot convert to object"
    );
}

/// The source key list is fixed, but current descriptors are re-read after
/// earlier getters and target setters. Snapshotted properties can become
/// enumerable or disappear; newly added keys remain outside the traversal.
#[test]
fn object_assign_rechecks_source_descriptors_after_reentry() {
    assert!(boolean(
        "var source={},target={};\
         function first(){Object.defineProperty(source,'hidden',{enumerable:true});\
           delete source.deleted;source.added=4;return 1;}\
         Object.defineProperty(source,'a',{get:first,enumerable:true});\
         Object.defineProperty(source,'hidden',{value:2,configurable:true});\
         source.deleted=3;Object.assign(target,source);\
         return target.a===1&&target.hidden===2&&!Object.hasOwn(target,'deleted')&&\
           !Object.hasOwn(target,'added');"
    ));
    assert!(boolean(
        "var source={a:1};var target={};\
         Object.defineProperty(source,'hidden',{value:2,enumerable:false,configurable:true});\
         function setA(value){Object.defineProperty(source,'hidden',{enumerable:true});}\
         Object.defineProperty(target,'a',{set:setA});Object.assign(target,source);\
         return target.hidden===2;"
    ));
}

/// Setters run with the coerced target as receiver. A rejected strict write or
/// a throwing setter aborts the operation after preserving earlier writes.
#[test]
fn object_assign_uses_strict_resumable_set_semantics() {
    assert!(boolean(
        "var log='',target={};function setX(value){log=log+(this===target?'t':'f')+value;}\
         Object.defineProperty(target,'x',{set:setX});Object.assign(target,{x:3});\
         if(log!=='t3'){return false;}\
         Object.defineProperty(target,'fixed',{value:1});\
         try{Object.assign(target,{before:2,fixed:3,after:4});}catch(error){\
           return error instanceof TypeError&&target.before===2&&target.fixed===1&&\
             !Object.hasOwn(target,'after');}return false;"
    ));
    assert!(boolean(
        "var marker={},target={};function thrower(value){throw marker;}\
         Object.defineProperty(target,'x',{set:thrower});\
         try{Object.assign(target,{x:1});}catch(error){return error===marker;}return false;"
    ));
    assert!(boolean(
        "var marker={},source={};function thrower(){throw marker;}\
         Object.defineProperty(source,'x',{get:thrower,enumerable:true});\
         try{Object.assign({},source);}catch(error){return error===marker;}return false;"
    ));
}

/// Assigning an enumerable `length` key to an Array uses the existing
/// resumable Array length write path, including the two observable numeric
/// conversions required for an object-valued length.
#[test]
fn object_assign_routes_array_length_through_array_set_length() {
    assert_eq!(
        text(
            "var log='';function lengthValue(){log=log+'v';return 2;}\
             var length={valueOf:lengthValue};\
             var array=[];Object.assign(array,{length:length});\
             return array.length+'|'+log;"
        ),
        "2|vv"
    );
}

#[test]
fn object_assign_routes_proxy_sources_and_targets_through_internal_methods() {
    assert_eq!(
        text(
            "var log='';var source=new Proxy({},{\
               ownKeys(){log=log+'o';return ['x'];},\
               getOwnPropertyDescriptor(){log=log+'d';return {enumerable:true,configurable:true};},\
               get(){log=log+'g';return 7;}});\
             var backing={};var target=new Proxy(backing,{set(t,k,v,r){\
               log=log+'s';t[k]=v;return true;}});\
             var result=Object.assign(target,source);\
             return (result===target)+'|'+backing.x+'|'+log;"
        ),
        "true|7|odgs"
    );
    assert_eq!(
        text(
            "var log='';var source=new Proxy({},{\
               ownKeys(){log=log+'o';return ['x'];},\
               getOwnPropertyDescriptor(){log=log+'d';return {enumerable:true,configurable:true};},\
               get(){log=log+'g';return 8;}});\
             var copy;({...copy}=source);return copy.x+'|'+log;"
        ),
        "8|odg"
    );
}

/// ECMA-262 28.1 defines `%Reflect%` as a non-callable ordinary object whose
/// prototype is `%Object.prototype%`; all thirteen methods have their exact
/// identity and the object has the specification @@toStringTag descriptor.
#[test]
fn reflect_construct_has_the_specification_shape() {
    assert_eq!(
        text(&format!(
            "{JOIN}return join([typeof Reflect,\
             Object.getPrototypeOf(Reflect)===Object.prototype,\
             Object.prototype.toString.call(Reflect),\
             Reflect.construct.length,Reflect.construct.name,\
             Object.getOwnPropertyNames(Reflect).join(',')]);"
        )),
        "object,true,[object Reflect],2,construct,apply,construct,defineProperty,deleteProperty,get,getOwnPropertyDescriptor,getPrototypeOf,has,isExtensible,ownKeys,preventExtensions,set,setPrototypeOf"
    );
    assert_eq!(
        text(
            "var d=Object.getOwnPropertyDescriptor(Reflect,Symbol.toStringTag);\
             return d.value+'|'+d.writable+'|'+d.enumerable+'|'+d.configurable;"
        ),
        "Reflect|false|false|true"
    );
    assert_eq!(type_error_message("return Reflect();"), "not a function");
    assert!(boolean(
        "var specs=[['apply',3],['construct',2],['defineProperty',3],\
          ['deleteProperty',2],['get',2],['getOwnPropertyDescriptor',2],\
          ['getPrototypeOf',1],['has',2],['isExtensible',1],['ownKeys',1],\
          ['preventExtensions',1],['set',3],['setPrototypeOf',2]];\
         for(var i=0;i<specs.length;i++){\
           var name=specs[i][0],method=Reflect[name];\
           var descriptor=Object.getOwnPropertyDescriptor(Reflect,name);\
           if(typeof method!=='function'||method.name!==name||\
              method.length!==specs[i][1]||!descriptor.writable||\
              descriptor.enumerable||!descriptor.configurable){return false;}\
         }return true;"
    ));
}

/// `Reflect.construct` validates `target` and the explicit `newTarget` before
/// touching `argumentsList`, then reads `length` once and indexed values from
/// left to right through ordinary `Get`.
#[test]
fn reflect_construct_preserves_specification_validation_and_collection_order() {
    assert_eq!(
        text(
            "var log='';\
             var args={get length(){log=log+'l';return 1;},\
                       get 0(){log=log+'0';return 'message';}};\
             var error=Reflect.construct(Error,args);\
             return error.message+'|'+log+'|'+(Object.getPrototypeOf(error)===Error.prototype);"
        ),
        "message|l0|true"
    );
    assert_eq!(
        text(
            "var log='';var args={get length(){log=log+'l';return 0;}};\
             try{Reflect.construct(Error.prototype.toString,args);}catch(error){}\
             try{Reflect.construct(Error,args,Error.prototype.toString);}catch(error){}\
             return log;"
        ),
        ""
    );
    assert_eq!(
        type_error_message("return Reflect.construct(Error,null);"),
        "not an object"
    );
    assert_eq!(
        type_error_message("return Reflect.construct(Error,[],undefined);"),
        "not a constructor"
    );
}

/// The selected `newTarget.prototype` controls allocation while `target`
/// controls constructor execution and the returned object completion.
#[test]
fn reflect_construct_keeps_target_and_new_target_distinct() {
    assert!(boolean(
        "function Target(){return new.target;}\
         function NewTarget(){}\
         return Reflect.construct(Target,[],NewTarget)===NewTarget;"
    ));
    assert_eq!(
        text(
            "function Target(value){this.value=value;}\
             function NewTarget(){}\
             NewTarget.prototype={marker:'custom'};\
             var value=Reflect.construct(Target,[7],NewTarget);\
             return value.value+'|'+value.marker+'|'+\
                    (Object.getPrototypeOf(value)===NewTarget.prototype);"
        ),
        "7|custom|true"
    );
    assert_eq!(
        text(
            "function Target(){return {replacement:true};}\
             var value=Reflect.construct(Target,[]);\
             return value.replacement+'|'+(Object.getPrototypeOf(value)===Target.prototype);"
        ),
        "true|false"
    );
}

/// The ordinary reflection surface delegates calls, preserves an explicit
/// receiver for accessors, includes inherited properties in `has`, and emits
/// `[[OwnPropertyKeys]]`'s numeric/string/symbol phase order.
#[test]
fn reflect_reads_calls_and_lists_keys_with_internal_method_semantics() {
    assert_eq!(
        text(
            "var symbol=Symbol('s');var object={2:'two',a:1};\
             Object.defineProperty(object,'hidden',{value:3});object[symbol]=4;\
             function add(a,b){return this.base+a+b;}\
             var getter={get x(){return this.value;}};var receiver={value:9};\
             var keys=Reflect.ownKeys(object).map(function(key){\
               return typeof key==='symbol'?'symbol':key;});\
             return Reflect.apply(add,{base:5},[2,3])+'|'+\
                    Reflect.get(getter,'x',receiver)+'|'+\
                    Reflect.has(Object.create(object),'a')+'|'+\
                    Reflect.getOwnPropertyDescriptor(object,'hidden').enumerable+'|'+\
                    keys.join(',');"
        ),
        "10|9|true|false|2,a,hidden,symbol"
    );
}

/// Reflect mutation methods expose internal-method rejection as `false`
/// instead of throwing, while successful definitions and writes retain their
/// exact descriptor and receiver behavior.
#[test]
fn reflect_mutations_return_booleans_and_honor_the_explicit_receiver() {
    assert_eq!(
        text(
            "var object={};\
             var defined=Reflect.defineProperty(object,'x',\
               {value:1,writable:false,configurable:false});\
             var redefined=Reflect.defineProperty(object,'x',{value:2});\
             var deleted=Reflect.deleteProperty(object,'x');\
             var fixed={};Object.defineProperty(fixed,'x',{value:1,writable:false});\
             var receiver={};var rejected=Reflect.set(fixed,'x',2,receiver);\
             var base={open:1};var child={};\
             var written=Reflect.set(base,'open',7,child);\
             var locked={};Object.preventExtensions(locked);\
             var prototype=Reflect.setPrototypeOf(locked,{});\
             var prevented=Reflect.preventExtensions(receiver);\
             return [defined,redefined,deleted,rejected,written,child.open,\
                     prototype,prevented,Reflect.isExtensible(receiver),\
                     Reflect.getPrototypeOf(child)===Object.prototype].join('|');"
        ),
        "true|false|false|false|true|7|false|true|false|true"
    );
    assert_eq!(
        text(
            "var target={set x(value){this.seen=value;}};var receiver={};\
             return Reflect.set(target,'x',11,receiver)+'|'+receiver.seen;"
        ),
        "true|11"
    );
}

/// Array `length` keeps its resumable numeric conversion and exotic mutation
/// rules when reached through `Reflect.set`, but completes with a Boolean.
#[test]
fn reflect_set_preserves_array_length_semantics() {
    assert_eq!(
        text(
            "var array=[1,2,3];var shortened=Reflect.set(array,'length',1);\
             Object.freeze(array);var blocked=Reflect.set(array,'length',0);\
             return shortened+'|'+array.length+'|'+blocked;"
        ),
        "true|1|false"
    );
    assert_exception_kind(
        "var array=[];return Reflect.set(array,'length',1.5);",
        ExceptionKind::RangeError,
    );
}

/// `ArraySetLength` performs `ToUint32` and `ToNumber` as distinct observable
/// conversions, truncates before clearing writable, and returns the caller's
/// Object/Reflect completion shape.
#[test]
fn array_length_descriptor_definitions_follow_array_set_length() {
    assert_eq!(
        text(
            "var log='';var value={valueOf(){log=log+'v';return 1;}};\
             var array=[1,2,3];\
             var defined=Reflect.defineProperty(array,'length',\
               {value:value,writable:false});\
             var descriptor=Object.getOwnPropertyDescriptor(array,'length');\
             return defined+'|'+array.length+'|'+descriptor.writable+'|'+log;"
        ),
        "true|1|false|vv"
    );
    assert_eq!(
        text(
            "var array=[1,2];\
             var same=Object.defineProperty(array,'length',{writable:false})===array;\
             var descriptor=Object.getOwnPropertyDescriptor(array,'length');\
             return same+'|'+array.length+'|'+descriptor.writable+'|'+\
                    Reflect.defineProperty(array,'length',{value:2})+'|'+\
                    Reflect.defineProperty(array,'length',{value:1});"
        ),
        "true|2|false|true|false"
    );
    assert_exception_kind(
        "var array=[];return Reflect.defineProperty(array,'length',{value:1.5});",
        ExceptionKind::RangeError,
    );
}

/// A non-configurable index stops an Array length shrink at that index plus
/// one. `ArraySetLength` still installs a requested `writable: false` before
/// reporting the failed internal definition.
#[test]
fn array_length_descriptor_blockers_preserve_the_partial_specification_result() {
    assert_eq!(
        text(
            "var array=[];\
             Object.defineProperty(array,'2',{value:3,configurable:false});\
             var defined=Reflect.defineProperty(array,'length',\
               {value:0,writable:false});\
             var descriptor=Object.getOwnPropertyDescriptor(array,'length');\
             return defined+'|'+array.length+'|'+descriptor.writable+'|'+array[2];"
        ),
        "false|3|false|3"
    );
}

/// Dense storage is unobservable: holes remain absent, exceptional indexed
/// descriptors extend `length`, `[[OwnPropertyKeys]]` stays ordered, and
/// freezing applies the ordinary descriptor transitions to every element.
/// The expected transcript also matches the independent Node oracle.
#[test]
fn dense_array_storage_preserves_reflection_and_integrity_semantics() {
    assert_eq!(
        text(
            "var array=[1,2,3];delete array[1];\
             Object.defineProperty(array,'0',{writable:false});\
             Object.defineProperty(array,'4',{\
               get(){return 9;},enumerable:true,configurable:true});\
             var zero=Object.getOwnPropertyDescriptor(array,'0');\
             var four=Object.getOwnPropertyDescriptor(array,'4');\
             var frozen=Object.freeze([4,5]);\
             return array.length+'|'+Reflect.ownKeys(array).join(',')+'|'+\
                    (1 in array)+'|'+zero.writable+'|'+typeof four.get+'|'+\
                    array[4]+'|'+Object.isFrozen(frozen)+'|'+\
                    Reflect.set(frozen,'0',7)+'|'+frozen[0];"
        ),
        "5|0,2,4,length|false|false|function|9|true|false|4"
    );
}

/// Realm-global references use ordinary `[[Get]]` and `[[Set]]`; accessors on
/// the global object therefore suspend into user code instead of surfacing an
/// engine-only diagnostic.
#[test]
fn realm_global_accessor_reads_and_writes_are_resumable() {
    assert_eq!(
        text(
            "var log='';\
             Object.defineProperty(globalThis,'realmAccessor',{\
               configurable:true,\
               get(){log=log+'g';return 4;},\
               set(value){log=log+'s'+value;}\
             });\
             realmAccessor=7;\
             return realmAccessor+'|'+log;"
        ),
        "4|s7g"
    );
}

/// Target type checks precede every observable property-key or array-list
/// conversion required by the corresponding ECMA-262 Reflect algorithm.
#[test]
fn reflect_validates_targets_before_observable_conversions() {
    assert_eq!(
        text(
            "var log='';function keyString(){log=log+'k';return 'x';}\
             var key={toString:keyString};\
             var list={get length(){log=log+'l';return 0;}};\
             try{Reflect.apply(1,null,list);}catch(error){}\
             try{Reflect.defineProperty(1,key,{});}catch(error){}\
             try{Reflect.deleteProperty(1,key);}catch(error){}\
             try{Reflect.get(1,key);}catch(error){}\
             try{Reflect.getOwnPropertyDescriptor(1,key);}catch(error){}\
             try{Reflect.has(1,key);}catch(error){}\
             try{Reflect.set(1,key,0);}catch(error){}\
             return log;"
        ),
        ""
    );
}

/// Proxy `[[Get]]` observes the handler lookup before the trap, supplies the
/// target/key/receiver tuple, and uses the target internal method when the trap
/// is absent. Static, computed, and Reflect access share that dispatcher.
#[test]
fn proxy_get_is_resumable_and_shared_by_language_and_reflect_access() {
    assert_eq!(
        text(
            "var log='';var target={x:3};var receiver={tag:'r'};\
             var handler={get get(){log=log+'l';return function(t,k,r){\
               log=log+k+(r===receiver?'r':'p');return t[k]+1;};}};\
             var proxy=new Proxy(target,handler);var key='x';\
             var fallback=new Proxy(target,{});\
             return proxy.x+'|'+proxy[key]+'|'+\
                    Reflect.get(proxy,'x',receiver)+'|'+fallback.x+'|'+log;"
        ),
        "4|4|4|3|lxplxplxr"
    );
}

#[test]
fn proxy_get_drives_operator_and_property_key_conversion() {
    assert_eq!(
        text(
            "var log='';function label(key){return typeof key==='symbol'?'@':key;}\
             var value=new Proxy({}, {get(target,key,receiver){\
               log=log+label(key)+',';if(typeof key==='symbol')return undefined;\
               if(key==='valueOf')return function(){return 4;};\
               return Reflect.get(target,key,receiver);}});\
             var sum=value+1;var propertyKey=new Proxy({}, {get(target,key,receiver){\
               log=log+label(key)+',';if(typeof key==='symbol')return undefined;\
               if(key==='toString')return function(){return 'x';};\
               return Reflect.get(target,key,receiver);}});\
             var target={};target[propertyKey]=7;return sum+'|'+target.x+'|'+log;"
        ),
        "5|7|@,valueOf,@,toString,"
    );
}

#[test]
fn proxy_get_drives_intrinsic_tags_and_wrapper_new_target_prototypes() {
    assert_eq!(
        text(
            "var log='';var value=new Proxy({}, {get(target,key,receiver){\
               if(key===Symbol.toStringTag){log=log+'t';return 'Tagged';}\
               return Reflect.get(target,key,receiver);}});\
             return Object.prototype.toString.call(value)+'|'+log;"
        ),
        "[object Tagged]|t"
    );
    assert_eq!(
        text(
            "var log='';var proto={marker:1};\
             var newTarget=new Proxy(function(){},{get(target,key,receiver){\
               if(key==='prototype'){log=log+'p';return proto;}\
               return Reflect.get(target,key,receiver);}});\
             var boolean=Reflect.construct(Boolean,[true],newTarget);\
             var number=Reflect.construct(Number,[7],newTarget);\
             var string=Reflect.construct(String,['x'],newTarget);\
             return Boolean.prototype.valueOf.call(boolean)+'|'+\
                    Number.prototype.valueOf.call(number)+'|'+\
                    String.prototype.valueOf.call(string)+'|'+\
                    (Object.getPrototypeOf(boolean)===proto)+'|'+\
                    (Object.getPrototypeOf(number)===proto)+'|'+\
                    (Object.getPrototypeOf(string)===proto)+'|'+log;"
        ),
        "true|7|x|true|true|true|ppp"
    );
    assert_eq!(
        text(
            "var log='';function construct(Target,args){\
               var proto={};var newTarget=new Proxy(function(){},{\
                 get:function(target,key,receiver){\
                   if(key==='prototype'){log=log+'p';return proto;}\
                   return Reflect.get(target,key,receiver);}});\
               var value=Reflect.construct(Target,args,newTarget);\
               return Object.getPrototypeOf(value)===proto;}\
             var target={};\
             return construct(Array,[])+'|'+construct(Map,[])+'|'+\
                    construct(WeakMap,[])+'|'+construct(Set,[])+'|'+\
                    construct(WeakSet,[])+'|'+\
                    construct(Promise,[function(){}])+'|'+\
                    construct(WeakRef,[target])+'|'+\
                    construct(FinalizationRegistry,[function(){}])+'|'+log;"
        ),
        "true|true|true|true|true|true|true|true|pppppppp"
    );
}

/// A `get` trap cannot lie about frozen data or getterless accessor properties.
#[test]
fn proxy_get_enforces_non_configurable_target_invariants() {
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1});\
         return new Proxy(target,{get(){return 2;}}).x;",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{get:undefined});\
         return new Proxy(target,{get(){return 1;}}).x;",
        ExceptionKind::TypeError,
    );
}

/// `%Proxy%` has no ordinary call behavior and `Proxy.revocable` publishes its
/// result record in the specification's `proxy`, `revoke` order.
#[test]
fn proxy_constructor_and_revocable_surface_are_installed() {
    assert_eq!(
        text(
            "var pair=Proxy.revocable({x:1},{});\
             return typeof Proxy+'|'+Proxy.length+'|'+\
                    Object.prototype.hasOwnProperty.call(Proxy,'prototype')+'|'+\
                    Reflect.ownKeys(pair).join(',')+'|'+pair.proxy.x+'|'+\
                    typeof pair.revoke;"
        ),
        "function|2|false|proxy,revoke|1|function"
    );
    assert_exception_kind("return Proxy({},{});", ExceptionKind::TypeError);
    assert_exception_kind("return new Proxy(1,{});", ExceptionKind::TypeError);
    assert_exception_kind("return new Proxy({},1);", ExceptionKind::TypeError);
}

/// Callable proxies use `apply`/`construct` traps with a fresh argument-list
/// Array, preserve the original `newTarget`, and delegate when a trap is absent.
#[test]
fn proxy_call_and_construct_traps_are_resumable() {
    assert_eq!(
        text(
            "function target(a,b){return this.base+a+b;}\
             var log='';var handler={\
               get apply(){log=log+'l';return function(t,r,args){\
                 return t.apply(r,args)+1;};}};\
             var proxy=new Proxy(target,handler);\
             var fallback=new Proxy(target,{});\
             return proxy.call({base:1},2,3)+'|'+\
                    fallback.call({base:1},2,3)+'|'+log;"
        ),
        "7|6|l"
    );
    assert_eq!(
        text(
            "function Target(value){this.value=value;}\
             function NewTarget(){}NewTarget.prototype={marker:1};\
             var seen=false;var proxy=new Proxy(Target,{construct(t,args,n){\
               seen=n===NewTarget;return Reflect.construct(t,args,n);}});\
             var value=Reflect.construct(proxy,[4],NewTarget);\
             return value.value+'|'+value.marker+'|'+seen;"
        ),
        "4|1|true"
    );
    assert_exception_kind(
        "function Target(){};var proxy=new Proxy(Target,{construct(){return 1;}});\
         return new proxy();",
        ExceptionKind::TypeError,
    );
}

/// The revoker is idempotent and all essential operations reject after it has
/// atomically cleared the Proxy's target and handler slots.
#[test]
fn proxy_revocation_affects_objects_and_callable_proxies() {
    assert_eq!(
        text(
            "var pair=Proxy.revocable({},{});\
             return String(pair.revoke()===undefined&&pair.revoke()===undefined);"
        ),
        "true"
    );
    assert_exception_kind(
        "var pair=Proxy.revocable({x:1},{});pair.revoke();return pair.proxy.x;",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var pair=Proxy.revocable(function(){return 1;},{});\
         pair.revoke();return pair.proxy();",
        ExceptionKind::TypeError,
    );
}

#[test]
fn array_is_array_unwraps_proxies_and_rejects_revoked_proxies() {
    assert!(boolean(
        "var array=[];var proxy=new Proxy(new Proxy(array,{}),{});\
         return Array.isArray(proxy)&&!Array.isArray(new Proxy({},{}));"
    ));
    assert_exception_kind(
        "var pair=Proxy.revocable([],{});pair.revoke();return Array.isArray(pair.proxy);",
        ExceptionKind::TypeError,
    );
    assert_eq!(
        text("return Object.prototype.toString.call(new Proxy([],{}));"),
        "[object Array]"
    );
    assert_exception_kind(
        "var pair=Proxy.revocable([],{});pair.revoke();\
         return Object.prototype.toString.call(pair.proxy);",
        ExceptionKind::TypeError,
    );
}

/// `[[Set]]`, `[[HasProperty]]`, and `[[Delete]]` share resumable trap lookup,
/// exact argument tuples, and their operation-specific Boolean completions.
#[test]
fn proxy_boolean_internal_methods_cover_language_and_reflect_paths() {
    assert_eq!(
        text(
            "var log='';var target={x:1};var receiver={};var handler={\
             set(t,k,v,r){log=log+'s'+k+v+(r===receiver?'r':'p');t[k]=v;return true;},\
             has(t,k){log=log+'h'+k;return k==='x';},\
             deleteProperty(t,k){log=log+'d'+k;return delete t[k];}};\
             var proxy=new Proxy(target,handler);proxy.x=2;proxy['x']=3;\
             var reflected=Reflect.set(proxy,'x',4,receiver);\
             var has=Reflect.has(proxy,'x');var inResult='x' in proxy;\
             var deleted=delete proxy.x;\
             return reflected+'|'+has+'|'+inResult+'|'+deleted+'|'+target.x+'|'+log;"
        ),
        "true|true|true|true|undefined|sx2psx3psx4rhxhxdx"
    );
    assert_eq!(
        text("var proxy=new Proxy({x:1},{set(){return false;}});proxy.x=2;return String(proxy.x);"),
        "1"
    );
    assert_eq!(
        text(
            "var log='';var receiver={};var prototype=new Proxy({},{set(t,k,v,r){\
               log=log+k+v+(r===receiver?'r':'?');return true;}});\
             var target=Object.create(prototype);\
             return Reflect.set(target,'x',5,receiver)+'|'+log;"
        ),
        "true|x5r"
    );
    assert_exception_kind(
        "'use strict';var proxy=new Proxy({x:1},{set(){return false;}});\
         proxy.x=2;return 0;",
        ExceptionKind::TypeError,
    );
}

#[test]
fn ordinary_set_routes_receiver_proxy_internal_methods() {
    assert_eq!(
        text(
            "var log='';var backing={};var receiver=new Proxy(backing,{\
               getOwnPropertyDescriptor(t,k){log=log+'G'+k;\
                 return Reflect.getOwnPropertyDescriptor(t,k);},\
               defineProperty(t,k,d){log=log+'D'+k+d.value;\
                 return Reflect.defineProperty(t,k,d);}});\
             var target={};var reflected=Reflect.set(target,'x',5,receiver);\
             return reflected+'|'+backing.x+'|'+Object.hasOwn(target,'x')+'|'+log;"
        ),
        "true|5|false|GxDx5"
    );
    assert_eq!(
        text(
            "var log='';var backing={};var proxy=new Proxy(backing,{\
               getOwnPropertyDescriptor(t,k){log=log+'G';\
                 return Reflect.getOwnPropertyDescriptor(t,k);},\
               defineProperty(t,k,d){log=log+'D';return Reflect.defineProperty(t,k,d);}});\
             proxy.x=1;return backing.x+'|'+log;"
        ),
        "1|GD"
    );
}

#[test]
fn core_internal_method_prototype_walks_are_iterative() {
    assert!(boolean(
        "var root={x:1};for(var i=0;i<4096;i=i+1)root=Object.create(root);\
         var read=root.x===1;var has='x' in root;root.y=2;\
         return read&&has&&root.y===2;"
    ));
}

/// Boolean traps cannot hide protected properties, delete non-configurable
/// properties, or claim incompatible writes to frozen target descriptors.
#[test]
fn proxy_boolean_traps_enforce_target_invariants() {
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1});\
         return Reflect.has(new Proxy(target,{has(){return false;}}),'x');",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1});\
         return delete new Proxy(target,{deleteProperty(){return true;}}).x;",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1,writable:false});\
         return Reflect.set(new Proxy(target,{set(){return true;}}),'x',2);",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1,writable:false});\
         var inner=new Proxy(target,{getOwnPropertyDescriptor(t,k){\
           return Reflect.getOwnPropertyDescriptor(t,k);}});\
         return Reflect.set(new Proxy(inner,{set(){return true;}}),'x',2);",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={x:1};Object.preventExtensions(target);\
         var inner=new Proxy(target,{getOwnPropertyDescriptor(t,k){\
           return Reflect.getOwnPropertyDescriptor(t,k);},\
           isExtensible(t){return Reflect.isExtensible(t);}});\
         return Reflect.has(new Proxy(inner,{has(){return false;}}),'x');",
        ExceptionKind::TypeError,
    );
}

/// Prototype and extensibility traps preserve their distinct return types and
/// validate the target state after the trap has run.
#[test]
fn proxy_meta_internal_methods_are_resumable_and_spec_ordered() {
    assert_eq!(
        text(
            "var log='';var first={first:1};var second={second:1};\
             var target=Object.create(first);var handler={\
               getPrototypeOf(t){log=log+'g';return second;},\
               setPrototypeOf(t,p){log=log+'s'+p.marker;return true;},\
               isExtensible(t){log=log+'i';return Object.isExtensible(t);},\
               preventExtensions(t){log=log+'p';Object.preventExtensions(t);return true;}};\
             var proxy=new Proxy(target,handler);var requested={marker:'r'};\
             var got=Reflect.getPrototypeOf(proxy)===second;\
             var set=Reflect.setPrototypeOf(proxy,requested);\
             var before=Reflect.isExtensible(proxy);\
             var prevented=Reflect.preventExtensions(proxy);\
             var after=Reflect.isExtensible(proxy);\
             return got+'|'+set+'|'+before+'|'+prevented+'|'+after+'|'+log;"
        ),
        "true|true|true|true|false|gsripi"
    );
    assert_eq!(
        text(
            "var log='';var prototype={p:1};var target={};var handler={\
               getPrototypeOf(t){log=log+'g';return prototype;},\
               setPrototypeOf(t,p){log=log+'s';return true;},\
               isExtensible(t){log=log+'i';return Object.isExtensible(t);},\
               preventExtensions(t){log=log+'p';Object.preventExtensions(t);return true;}};\
             var proxy=new Proxy(target,handler);var requested={};\
             var got=Object.getPrototypeOf(proxy)===prototype;\
             var set=Object.setPrototypeOf(proxy,requested)===proxy;\
             var before=Object.isExtensible(proxy);\
             var prevented=Object.preventExtensions(proxy)===proxy;\
             var after=Object.isExtensible(proxy);\
             return got+'|'+set+'|'+before+'|'+prevented+'|'+after+'|'+log;"
        ),
        "true|true|true|true|false|gsipi"
    );
}

/// Non-extensible targets pin their prototype, `isExtensible` must report the
/// target's exact bit, and a successful `preventExtensions` trap must clear it.
#[test]
fn proxy_meta_traps_enforce_target_invariants() {
    assert_exception_kind(
        "var target={};Object.preventExtensions(target);\
         return Reflect.getPrototypeOf(new Proxy(target,{getPrototypeOf(){return {};}}));",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.preventExtensions(target);\
         return Reflect.setPrototypeOf(new Proxy(target,{setPrototypeOf(){return true;}}),{});",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "return Reflect.isExtensible(new Proxy({},{isExtensible(){return false;}}));",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "return Reflect.preventExtensions(new Proxy({},{preventExtensions(){return true;}}));",
        ExceptionKind::TypeError,
    );
}

/// `[[GetOwnProperty]]` converts a trap descriptor in the normative field
/// order, completes omitted fields, and returns a fresh ordinary descriptor.
#[test]
fn proxy_get_own_property_descriptor_is_resumable_and_complete() {
    assert_eq!(
        text(
            "var log='';var target={};\
             Object.defineProperty(target,'x',{value:1,writable:false,\
               enumerable:false,configurable:false});\
             var descriptor={\
               get enumerable(){log=log+'e';return false;},\
               get configurable(){log=log+'c';return false;},\
               get value(){log=log+'v';return 1;},\
               get writable(){log=log+'w';return false;}};\
             var proxy=new Proxy(target,{getOwnPropertyDescriptor(){return descriptor;}});\
             var result=Reflect.getOwnPropertyDescriptor(proxy,'x');\
             result.value=4;\
             return log+'|'+result.value+'|'+result.writable+'|'+\
                    result.enumerable+'|'+result.configurable+'|'+target.x;"
        ),
        "ecvw|4|false|false|false|1"
    );
    assert_eq!(
        text(
            "var target={x:1};var proxy=new Proxy(target,{});\
             var result=Object.getOwnPropertyDescriptor(proxy,'x');\
             return result.value+'|'+result.writable+'|'+\
                    result.enumerable+'|'+result.configurable;"
        ),
        "1|true|true|true"
    );
    assert_eq!(
        text(
            "var log='';var target={x:1};var proxy=new Proxy(target,{\
               getOwnPropertyDescriptor(t,k){log=log+k;return Reflect.getOwnPropertyDescriptor(t,k);}});\
             return Object.hasOwn(proxy,'x')+'|'+\
                    Object.prototype.hasOwnProperty.call(proxy,'x')+'|'+\
                    Object.prototype.propertyIsEnumerable.call(proxy,'x')+'|'+log;"
        ),
        "true|true|true|xxx"
    );
}

#[test]
fn proxy_backed_property_descriptors_use_resumable_has_and_get() {
    assert_eq!(
        text(
            "var log='';var descriptorTarget={enumerable:true,configurable:true,\
               value:5,writable:true};var descriptor=new Proxy(descriptorTarget,{\
               has(t,k){log=log+'h:'+k+',';return k in t;},\
               get(t,k,r){log=log+'g:'+k+',';return Reflect.get(t,k,r);}});\
             var target={};Reflect.defineProperty(target,'x',descriptor);\
             return target.x+'|'+log;"
        ),
        "5|h:enumerable,g:enumerable,h:configurable,g:configurable,h:value,g:value,h:writable,g:writable,h:get,h:set,"
    );
    assert_eq!(
        text(
            "var log='';var target={x:1};var descriptor=new Proxy(\
               {value:2,writable:true,enumerable:true,configurable:true},{\
                 has(t,k){log=log+'h'+k[0];return k in t;},\
                 get(t,k,r){log=log+'g'+k[0];return Reflect.get(t,k,r);}});\
             var proxy=new Proxy(target,{getOwnPropertyDescriptor(){return descriptor;}});\
             var result=Reflect.getOwnPropertyDescriptor(proxy,'x');\
             return result.value+'|'+log;"
        ),
        "2|hegehcgchvgvhwgwhghs"
    );
    assert_eq!(
        text(
            "var log='';var marker={};var descriptor=new Proxy({enumerable:true},{\
               has(t,k){log=log+'h:'+k+',';if(k==='configurable')throw marker;return k in t;},\
               get(t,k,r){log=log+'g:'+k+',';return Reflect.get(t,k,r);}});\
             try{Reflect.defineProperty({},'x',descriptor);}catch(error){\
               return (error===marker)+'|'+log;}return 'missed';"
        ),
        "true|h:enumerable,g:enumerable,h:configurable,"
    );
}

/// A descriptor trap cannot hide a protected property or report a descriptor
/// that is incompatible with the target's current extensibility and layout.
#[test]
fn proxy_get_own_property_descriptor_enforces_target_invariants() {
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1});\
         return Reflect.getOwnPropertyDescriptor(\
           new Proxy(target,{getOwnPropertyDescriptor(){return undefined;}}),'x');",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={x:1};Object.preventExtensions(target);\
         return Reflect.getOwnPropertyDescriptor(\
           new Proxy(target,{getOwnPropertyDescriptor(){return undefined;}}),'x');",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "return Reflect.getOwnPropertyDescriptor(\
           new Proxy({},{getOwnPropertyDescriptor(){return {configurable:false};}}),'x');",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "return Reflect.getOwnPropertyDescriptor(\
           new Proxy({},{getOwnPropertyDescriptor(){return 1;}}),'x');",
        ExceptionKind::TypeError,
    );
    assert_eq!(
        text(
            "var log='';var target={x:1};\
             var inner=new Proxy(target,{\
               getOwnPropertyDescriptor(t,k){log=log+'d';return Reflect.getOwnPropertyDescriptor(t,k);},\
               isExtensible(t){log=log+'e';return Reflect.isExtensible(t);}});\
             var outer=new Proxy(inner,{getOwnPropertyDescriptor(t,k){\
               log=log+'o';return {value:2,writable:true,enumerable:true,configurable:true};}});\
             var result=Reflect.getOwnPropertyDescriptor(outer,'x');\
             return result.value+'|'+log;"
        ),
        "2|ode"
    );
}

/// Proxy `[[DefineOwnProperty]]` receives a fresh partial descriptor object,
/// preserves absent fields, and maps its Boolean result to Object/Reflect.
#[test]
fn proxy_define_own_property_is_resumable_and_shared_by_object_and_reflect() {
    assert_eq!(
        text(
            "var log='';var target={};var handler={defineProperty(t,k,d){\
               log=log+(this===handler?'h':'?')+(t===target?'t':'?')+k+'|'+\
                 Reflect.ownKeys(d).join(',');return true;}};\
             var proxy=new Proxy(target,handler);\
             var reflected=Reflect.defineProperty(proxy,'x',{value:3,writable:true,\
               enumerable:true,configurable:true});\
             var returned=Object.defineProperty(proxy,'y',{value:4,configurable:true});\
             return reflected+'|'+(returned===proxy)+'|'+target.x+'|'+target.y+'|'+log;"
        ),
        "true|true|undefined|undefined|htx|enumerable,configurable,value,writablehty|configurable,value"
    );
    assert_eq!(
        text(
            "var target={};var proxy=new Proxy(target,{});\
             var returned=Object.defineProperty(proxy,'x',{value:7});\
             return (returned===proxy)+'|'+target.x+'|'+\
                    Object.getOwnPropertyDescriptor(target,'x').writable;"
        ),
        "true|7|false"
    );
    assert_eq!(
        text(
            "var log='';var value={valueOf(){log=log+'v';return 1;}};\
             var proxy=new Proxy([1,2,3],{});\
             var result=Reflect.defineProperty(proxy,'length',{value:value});\
             return result+'|'+proxy.length+'|'+log;"
        ),
        "true|1|vv"
    );
    assert_eq!(
        text(
            "return String(Reflect.defineProperty(\
               new Proxy({},{defineProperty(){return false;}}),'x',{value:1}));"
        ),
        "false"
    );
    assert_exception_kind(
        "return Object.defineProperty(\
           new Proxy({},{defineProperty(){return false;}}),'x',{value:1});",
        ExceptionKind::TypeError,
    );
}

/// A successful define trap must still describe a definition compatible with
/// the target's current own property and extensibility state.
#[test]
fn proxy_define_own_property_enforces_target_invariants() {
    assert_exception_kind(
        "var target={};Object.preventExtensions(target);\
         return Reflect.defineProperty(new Proxy(target,{defineProperty(){return true;}}),\
           'x',{value:1});",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "return Reflect.defineProperty(new Proxy({},{defineProperty(){return true;}}),\
           'x',{configurable:false});",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1});\
         return Reflect.defineProperty(new Proxy(target,{defineProperty(){return true;}}),\
           'x',{value:2});",
        ExceptionKind::TypeError,
    );
}

/// `[[OwnPropertyKeys]]` performs `CreateListFromArrayLike` through observable
/// length/index reads, preserves trap order, and returns string/Symbol keys.
#[test]
fn proxy_own_keys_is_resumable_and_preserves_the_trap_list() {
    assert_eq!(
        text(
            "var log='';var symbol=Symbol('s');var result={\
               get length(){log=log+'l';return {valueOf(){log=log+'v';return 2;}};},\
               get 0(){log=log+'0';return 'x';},\
               get 1(){log=log+'1';return symbol;}};\
             var target={a:1};var proxy=new Proxy(target,{\
               get ownKeys(){log=log+'g';return function(t){log=log+'t';return result;};}});\
             var keys=Reflect.ownKeys(proxy);\
             return keys[0]+'|'+(keys[1]===symbol)+'|'+log;"
        ),
        "x|true|gtlv01"
    );
    assert_eq!(
        text(
            "var symbol=Symbol('s');var target={2:1,a:2};target[symbol]=3;\
             return Reflect.ownKeys(new Proxy(target,{})).map(function(k){\
               return typeof k==='symbol'?'s':k;}).join(',');"
        ),
        "2,a,s"
    );
    assert_eq!(
        text(
            "var log='';var symbol=Symbol('s');var target={};\
             Object.defineProperty(target,'hidden',{value:1});target[symbol]=2;\
             var proxy=new Proxy(target,{\
               ownKeys(){log=log+'o';return ['virtual','hidden',symbol];},\
               getOwnPropertyDescriptor(t,k){log=log+'d';if(k==='virtual'){\
                 return {value:3,enumerable:true,configurable:true};}\
                 return Reflect.getOwnPropertyDescriptor(t,k);}});\
             var keys=Object.keys(proxy).join(',');\
             var names=Object.getOwnPropertyNames(proxy).join(',');\
             var symbols=Object.getOwnPropertySymbols(proxy);\
             return keys+'|'+names+'|'+(symbols[0]===symbol)+'|'+log;"
        ),
        "virtual|virtual,hidden|true|oddoo"
    );
    assert_eq!(
        text(
            "var log='';var proxy=new Proxy({},{\
               ownKeys(){log=log+'o';return ['x'];},\
               getOwnPropertyDescriptor(){log=log+'d';return {enumerable:true,configurable:true};},\
               get(){log=log+'g';return 9;}});\
             var values=Object.values(proxy);var entries=Object.entries(proxy);\
             return values[0]+'|'+entries[0][0]+'|'+entries[0][1]+'|'+log;"
        ),
        "9|x|9|odgodg"
    );
}

/// Proxy own-key results reject duplicates/non-keys and must contain protected
/// keys; a non-extensible target additionally requires an exact key set.
#[test]
fn proxy_own_keys_enforces_target_invariants() {
    assert_exception_kind(
        "return Reflect.ownKeys(new Proxy({},{ownKeys(){return ['x','x'];}}));",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "return Reflect.ownKeys(new Proxy({},{ownKeys(){return [1];}}));",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={};Object.defineProperty(target,'x',{value:1});\
         return Reflect.ownKeys(new Proxy(target,{ownKeys(){return [];}}));",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={x:1};Object.preventExtensions(target);\
         return Reflect.ownKeys(new Proxy(target,{ownKeys(){return [];}}));",
        ExceptionKind::TypeError,
    );
    assert_exception_kind(
        "var target={x:1};Object.preventExtensions(target);\
         return Reflect.ownKeys(new Proxy(target,{ownKeys(){return ['x','y'];}}));",
        ExceptionKind::TypeError,
    );
}

/// `Proxy.[[OwnPropertyKeys]]` consumes immediate trap-result and descriptor
/// completions iteratively. A large ordinary target must not turn that native
/// state machine into host-stack recursion or quadratic key validation.
#[test]
fn proxy_own_keys_scales_for_a_large_reversed_trap_result() {
    assert_number(
        "var count=2048;var target={};var keys=[];\
         for(var index=0;index<count;index=index+1){\
           var key='key'+index;\
           Object.defineProperty(target,key,{configurable:(index%2)===0,value:0});\
           keys.push(key);\
         }\
         var proxy=new Proxy(target,{ownKeys(){\
           var result=[];\
           for(var index=keys.length;index>0;index=index-1)result.push(keys[index-1]);\
           return result;\
         }});\
         return Object.getOwnPropertyNames(proxy).length;",
        2048,
    );
}
