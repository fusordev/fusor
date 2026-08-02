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
        "return Object.getPrototypeOf({__proto__:null})===null;"
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
/// constructor table while methods outside the admitted profile remain absent.
#[test]
fn the_extended_object_reflection_statics_have_the_specification_shape() {
    assert!(boolean(
        "var specs=[['is',2],['hasOwn',2],['getOwnPropertySymbols',1],\
          ['getOwnPropertyDescriptors',1],['values',1],['entries',1],['assign',2],\
          ['defineProperties',2]];\
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
getOwnPropertyNames,getOwnPropertySymbols,keys,values,entries,isExtensible,preventExtensions,\
getOwnPropertyDescriptor,getOwnPropertyDescriptors,is,assign,seal,freeze,isSealed,\
isFrozen,hasOwn,prototype"
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
