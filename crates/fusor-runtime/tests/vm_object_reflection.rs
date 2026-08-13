//! `Object.prototype.hasOwnProperty`, `isPrototypeOf`, and
//! `propertyIsEnumerable`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log([1,,3].hasOwnProperty(1),\
//!     Object.create({a:1}).hasOwnProperty("a"), ({}).isPrototypeOf({}));'
//! false false false
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! ({a:1}).hasOwnProperty("a") => true      hasOwnProperty("b") => false
//! Object.create({a:1}).hasOwnProperty("a") => false
//! [1,2].hasOwnProperty(0) => true          [1,,3].hasOwnProperty(1) => false
//! [1].hasOwnProperty("length") => true     ({0:1}).hasOwnProperty(0) => true
//! ({x:1}).hasOwnProperty({toString(){return "x"}}) => true
//! hasOwnProperty.call("ab",0) => true      hasOwnProperty.call("ab",5) => false
//! hasOwnProperty.call("ab","length") => true
//! ({undefined:1}).hasOwnProperty() => true
//! hasOwnProperty.call(1,"a") => false
//! hasOwnProperty.call(null,"a") !! TypeError: cannot convert to object
//! p.isPrototypeOf(Object.create(p)) => true (transitively too)
//! ({}).isPrototypeOf({}) => false          p.isPrototypeOf(p) => false
//! ({}).isPrototypeOf(1) => false           ({}).isPrototypeOf(null) => false
//! Object.prototype.isPrototypeOf({}) => true
//! Array.prototype.isPrototypeOf([]) => true
//! ({a:1}).propertyIsEnumerable("a") => true
//! Object.create({a:1}).propertyIsEnumerable("a") => false
//! non-enumerable own property => false     absent property => false
//! [1].propertyIsEnumerable(0) => true      [1].propertyIsEnumerable("length") => false
//! each length => 1
//! ```

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits,
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
                    Arc::from("<runtime Object reflection>"),
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

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
}

/// Evaluates `expression` and renders the result with `String()`.
fn rendered(expression: &str) -> String {
    evaluate(&format!("return String({expression});"), |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

/// Returns the thrown exception's kind and message.
fn thrown(body: &str) -> (ExceptionKind, String) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        let kind = exception.kind().expect("engine exception kind");
        let message = exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8");
        (kind, message)
    })
}

fn assert_throws(body: &str, kind: ExceptionKind, message: &str) {
    assert_eq!((kind, message.to_owned()), thrown(body), "{body}");
}

/// Asserts a table of `expression => rendered result` pairs.
fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

/// `hasOwnProperty` tests the own property only, never an inherited one.
#[test]
fn has_own_property_ignores_the_prototype_chain() {
    assert_all(&[
        ("({a:1}).hasOwnProperty('a')", "true"),
        ("({a:1}).hasOwnProperty('b')", "false"),
        // An inherited property is not an own one. `Object.create` is not in
        // this profile, so the same shape is built with `setPrototypeOf`.
        (
            "Object.setPrototypeOf({}, {a:1}).hasOwnProperty('a')",
            "false",
        ),
        ("({}).hasOwnProperty('toString')", "false"),
        ("Object.prototype.hasOwnProperty('toString')", "true"),
    ]);
}

/// A hole is absent rather than `undefined`, which is what `indexOf` relies on.
#[test]
fn has_own_property_distinguishes_a_hole_from_undefined() {
    assert_all(&[
        ("[1,2].hasOwnProperty(0)", "true"),
        ("[1,,3].hasOwnProperty(1)", "false"),
        ("[1,,3].hasOwnProperty(2)", "true"),
        ("[1].hasOwnProperty('length')", "true"),
        ("[1].hasOwnProperty(5)", "false"),
    ]);
}

/// The key is converted with `ToPropertyKey`, so a Number or object works.
#[test]
fn has_own_property_converts_its_key() {
    assert_all(&[
        ("({0:1}).hasOwnProperty(0)", "true"),
        ("({x:1}).hasOwnProperty({toString(){return 'x';}})", "true"),
        // An absent argument becomes the string `"undefined"`.
        ("({undefined:1}).hasOwnProperty()", "true"),
        (
            "(function(){const k=Symbol('k');const o={};o[k]=1;return o.hasOwnProperty(k);})()",
            "true",
        ),
    ]);
}

/// A primitive receiver answers about its exotic own properties.
///
/// A String exposes its indices and `length`; every other primitive has no own
/// property at all.
#[test]
fn has_own_property_accepts_a_primitive_receiver() {
    assert_all(&[
        ("Object.prototype.hasOwnProperty.call('ab',0)", "true"),
        ("Object.prototype.hasOwnProperty.call('ab',5)", "false"),
        (
            "Object.prototype.hasOwnProperty.call('ab','length')",
            "true",
        ),
        ("Object.prototype.hasOwnProperty.call(1,'a')", "false"),
        ("Object.prototype.hasOwnProperty.call(true,'a')", "false"),
    ]);
}

/// `isPrototypeOf` walks the candidate's chain and never matches itself.
#[test]
fn is_prototype_of_walks_the_candidates_chain() {
    assert_all(&[
        (
            "(function(){\
                const p={};\
                return p.isPrototypeOf(Object.setPrototypeOf({}, p));\
            })()",
            "true",
        ),
        // The walk is transitive.
        (
            "(function(){\
                const p={};\
                const m=Object.setPrototypeOf({}, p);\
                return p.isPrototypeOf(Object.setPrototypeOf({}, m));\
            })()",
            "true",
        ),
        ("({}).isPrototypeOf({})", "false"),
        // The walk starts at the candidate's prototype, so nothing precedes
        // itself.
        (
            "(function(){const p={};return p.isPrototypeOf(p);})()",
            "false",
        ),
        // A primitive candidate has no chain of its own.
        ("({}).isPrototypeOf(1)", "false"),
        ("({}).isPrototypeOf(null)", "false"),
        ("({}).isPrototypeOf(undefined)", "false"),
        ("Object.prototype.isPrototypeOf({})", "true"),
        ("Object.prototype.isPrototypeOf([])", "true"),
        ("Array.prototype.isPrototypeOf([])", "true"),
        ("Array.prototype.isPrototypeOf({})", "false"),
        (
            "(function(){let log='';const p={};const candidate=new Proxy({},\
                {getPrototypeOf(){log+='g';return p;}});\
                return p.isPrototypeOf(candidate)+'|'+log;})()",
            "true|g",
        ),
        // The candidate type check precedes ToObject(this).
        ("Object.prototype.isPrototypeOf.call(null,1)", "false"),
        (
            "(function(){let log='';const candidate=new Proxy({},\
                {getPrototypeOf(){log+='g';return null;}});\
                try{Object.prototype.isPrototypeOf.call(null,candidate);}\
                catch(error){return (error instanceof TypeError)+'|'+log;}\
                return 'missed';})()",
            "true|",
        ),
    ]);
}

/// `propertyIsEnumerable` tests the own property's enumerable attribute.
#[test]
fn property_is_enumerable_tests_the_own_attribute() {
    assert_all(&[
        ("({a:1}).propertyIsEnumerable('a')", "true"),
        // An inherited property answers `false` even when enumerable.
        (
            "Object.setPrototypeOf({}, {a:1}).propertyIsEnumerable('a')",
            "false",
        ),
        (
            "(function(){\
                const o={};\
                Object.defineProperty(o,'a',{value:1,enumerable:false});\
                return o.propertyIsEnumerable('a');\
            })()",
            "false",
        ),
        // An absent property is not enumerable.
        ("({}).propertyIsEnumerable('a')", "false"),
        ("[1].propertyIsEnumerable(0)", "true"),
        // An array's `length` is writable but not enumerable.
        ("[1].propertyIsEnumerable('length')", "false"),
    ]);
}

/// A nullish receiver is rejected once an operation reaches `ToObject(this)`.
#[test]
fn a_nullish_receiver_is_rejected() {
    for method in ["hasOwnProperty", "propertyIsEnumerable"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Object.prototype.{method}.call({receiver}, 'a');"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }

    // `isPrototypeOf` first rejects a non-object candidate without coercing
    // its receiver, but an object candidate reaches `ToObject(this)`.
    assert_all(&[
        ("Object.prototype.isPrototypeOf.call(null, 'a')", "false"),
        (
            "Object.prototype.isPrototypeOf.call(undefined, 'a')",
            "false",
        ),
    ]);
    for receiver in ["null", "undefined"] {
        assert_throws(
            &format!("return Object.prototype.isPrototypeOf.call({receiver}, {{}});"),
            ExceptionKind::TypeError,
            "cannot convert to object",
        );
    }
}

/// The installed methods carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_reflection_methods_have_the_pinned_shape() {
    assert_all(&[
        ("Object.prototype.hasOwnProperty.length", "1"),
        ("Object.prototype.isPrototypeOf.length", "1"),
        ("Object.prototype.propertyIsEnumerable.length", "1"),
        ("Object.prototype.hasOwnProperty.name", "hasOwnProperty"),
        ("Object.prototype.isPrototypeOf.name", "isPrototypeOf"),
        (
            "Object.prototype.propertyIsEnumerable.name",
            "propertyIsEnumerable",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object.prototype,'hasOwnProperty').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object.prototype,'hasOwnProperty').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object.prototype,'hasOwnProperty').configurable",
            "true",
        ),
    ]);
}
/// `Object.create` installs the requested prototype, including none at all.
///
/// Oracle:
///
/// ```console
/// $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log(\
///     Object.getPrototypeOf(Object.create(null)), Object.create({a:1}).a);'
/// null 1
/// ```
#[test]
fn object_create_installs_the_requested_prototype() {
    assert_all(&[
        // A null prototype is represented rather than substituted, so the
        // result inherits nothing at all.
        ("Object.getPrototypeOf(Object.create(null))", "null"),
        ("Object.create(null).a", "undefined"),
        (
            "(function(){const p={a:1};return Object.getPrototypeOf(Object.create(p))===p;})()",
            "true",
        ),
        // The new object inherits rather than owning.
        ("Object.create({a:1}).a", "1"),
        (
            "Object.prototype.hasOwnProperty.call(Object.create({a:1}),'a')",
            "false",
        ),
        ("Object.keys(Object.create({a:1})).length", "0"),
        ("typeof Object.create({})", "object"),
        // A function is a valid prototype. A declaration is used because this
        // profile does not yet infer a name for an anonymous function
        // expression bound to a `const`.
        (
            "(function(){\
                function f(){}\
                return Object.getPrototypeOf(Object.create(f))===f;\
            })()",
            "true",
        ),
        ("Object.create(Array.prototype) instanceof Array", "true"),
        ("Object.isExtensible(Object.create(null))", "true"),
    ]);
}

/// Only `null` and an object are prototypes.
///
/// Every other argument, including an absent one, reports
/// `TypeError: not a prototype`.
#[test]
fn object_create_rejects_a_non_prototype() {
    for argument in ["1", "'a'", "true", "undefined", ""] {
        assert_throws(
            &format!("return Object.create({argument});"),
            ExceptionKind::TypeError,
            "not a prototype",
        );
    }
}

/// The optional descriptors argument delegates to the same two-phase
/// `ObjectDefineProperties` operation as `Object.defineProperties`.
#[test]
fn object_create_applies_property_descriptors() {
    assert_all(&[
        (
            "(function(){\
                const p={inherited:1};\
                const o=Object.create(p,{x:{value:2,enumerable:true},\
                    hidden:{value:3,writable:true}});\
                return Object.getPrototypeOf(o)===p&&o.x===2&&o.hidden===3&&\
                    Object.keys(o).join(',')==='x';\
            })()",
            "true",
        ),
        (
            "(function(){\
                const marker={};function read(){throw marker;}\
                const descriptors={};\
                Object.defineProperty(descriptors,'x',{get:read,enumerable:true});\
                try{Object.create({},descriptors);}catch(error){return error===marker;}\
                return false;\
            })()",
            "true",
        ),
    ]);
    assert_throws(
        "return Object.create({}, null);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_all(&[(
        "(function(){\
            let log='';const descriptors={};function read(){log+='g';return {value:1};}\
            Object.defineProperty(descriptors,'x',{get:read,enumerable:true});\
            try{Object.create(1,descriptors);}catch(error){}return log;\
        })()",
        "",
    )]);
}

/// `Object.create` carries the pinned `name`, `length`, and descriptors.
#[test]
fn object_create_has_the_pinned_shape() {
    assert_all(&[
        // Arity 2 covers the prototype and optional descriptor map.
        ("Object.create.length", "2"),
        ("Object.create.name", "create"),
        (
            "Object.getOwnPropertyDescriptor(Object,'create').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object,'create').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object,'create').configurable",
            "true",
        ),
    ]);
}
