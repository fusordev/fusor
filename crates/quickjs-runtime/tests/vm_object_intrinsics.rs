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
