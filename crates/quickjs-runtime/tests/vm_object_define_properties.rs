//! `Object.defineProperties` and `Object.create`'s descriptors argument.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const o = Object.defineProperties({},\
//!     {a: {value: 1}, b: {get(){ return 2 }}});\
//!     console.log(o.a, o.b, Object.getOwnPropertyDescriptor(o, "a").writable);'
//! 1 2 false
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object.defineProperties({}, {a:{value:1}}) => a is present, all flags false
//! the target itself is returned
//! the descriptors object's enumerable keys are visited, string and symbol
//! a non-enumerable key on the descriptors object is skipped
//! per key: the descriptor is read, then its fields in ToPropertyDescriptor
//!   order, so `ra`, `va`, `rb`, `vb`
//! a non-object descriptor !! TypeError: not an object
//! a nullish descriptors object !! TypeError: cannot convert to object
//! a non-object target !! TypeError: not an object
//! a frozen target !! TypeError: object is not extensible
//! Object.create(null, {a:{value:1}}) creates it with a and a null prototype
//! Object.create(p, undefined) creates it with no own property
//! lengths: defineProperties 2, create 2
//! ```
//!
//! One expectation below is the specification's rather than the oracle's:
//! ECMAScript reads and validates every descriptor before applying any, so a
//! later read that throws leaves the target untouched, while the oracle keeps
//! the earlier definitions. See PORTING.md.
use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
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
                    Arc::from("<runtime Object defineProperties>"),
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

/// `Object.defineProperties` defines every descriptor and returns the target.
#[test]
fn object_define_properties_defines_each_descriptor() {
    assert_all(&[
        (
            "(function(){\
                const o=Object.defineProperties({},{a:{value:1},b:{get(){return 2;}}});\
                return o.a+'|'+o.b;\
            })()",
            "1|2",
        ),
        // The target itself is returned, not a copy.
        (
            "(function(){\
                const target={};\
                return Object.defineProperties(target,{})===target;\
            })()",
            "true",
        ),
        // An omitted attribute defaults to `false`, unlike an assignment.
        (
            "(function(){\
                const o=Object.defineProperties({},{a:{value:1}});\
                const d=Object.getOwnPropertyDescriptor(o,'a');\
                return d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "false|false|false",
        ),
        // Only the descriptors object's own enumerable keys are visited.
        (
            "(function(){\
                const props={};\
                Object.defineProperty(props,'h',{value:{value:1}});\
                props.v={value:2};\
                const o=Object.defineProperties({},props);\
                return String(o.h)+'|'+o.v;\
            })()",
            "undefined|2",
        ),
        // A symbol key on the descriptors object defines a symbol-keyed
        // property.
        (
            "(function(){\
                const s=Symbol('q');\
                const props={};\
                props[s]={value:1};\
                const o=Object.defineProperties({},props);\
                return o[s];\
            })()",
            "1",
        ),
        // An existing property is redefined when it is configurable.
        (
            "(function(){\
                const o={a:1};\
                Object.defineProperties(o,{a:{value:2}});\
                const d=Object.getOwnPropertyDescriptor(o,'a');\
                return d.value+'|'+d.writable;\
            })()",
            "2|true",
        ),
    ]);
    assert_throws(
        "return Object.defineProperties(null,{});",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Object.defineProperties(1,{});",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Object.defineProperties({},null);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.defineProperties({},{a:1});",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Object.defineProperties(Object.freeze({}),{a:{value:1}});",
        ExceptionKind::TypeError,
        "object is not extensible",
    );
}

/// Each descriptor is read, then its fields, so the walk has two nested
/// suspension points per key.
#[test]
fn each_descriptor_and_its_fields_read_in_order() {
    assert_all(&[
        // The descriptors object's own accessor runs first, then the
        // descriptor's own fields.
        (
            "(function(){\
                let log='';\
                const props={};\
                Object.defineProperty(props,'a',{\
                    get(){log+='ra';return {get value(){log+='va';return 1;}};},\
                    enumerable:true\
                });\
                Object.defineProperty(props,'b',{\
                    get(){log+='rb';return {get value(){log+='vb';return 2;}};},\
                    enumerable:true\
                });\
                const o=Object.defineProperties({},props);\
                return log+'|'+o.a+','+o.b;\
            })()",
            "ravarbvb|1,2",
        ),
        // The keys are visited in `[[OwnPropertyKeys]]` order.
        (
            "(function(){\
                let log='';\
                const props={};\
                for (const key of ['a','b']) {\
                    Object.defineProperty(props,key,{get(){log+=key;return {value:1};},enumerable:true});\
                }\
                Object.defineProperties({},props);\
                return log;\
            })()",
            "ab",
        ),
        // The fields are read in `ToPropertyDescriptor` order.
        (
            "(function(){\
                let log='';\
                const descriptor={};\
                for (const field of ['writable','value','configurable','enumerable']) {\
                    Object.defineProperty(descriptor,field,{get(){log+=field+'|';return undefined;},configurable:true});\
                }\
                Object.defineProperties({},{a:descriptor});\
                return log;\
            })()",
            "enumerable|configurable|value|writable|",
        ),
    ]);
}

/// Every descriptor is validated before any is applied.
///
/// This is the specification's two-phase order. The pinned oracle interleaves
/// the phases and so keeps the earlier definitions; see PORTING.md.
#[test]
fn every_descriptor_is_validated_before_any_is_applied() {
    assert_all(&[
        // A later read that throws leaves the target untouched.
        (
            "(function(){\
                const target={};\
                try {\
                    Object.defineProperties(target,{a:{value:1},get b(){throw new TypeError('x');}});\
                } catch (thrown) {\
                    return String(target.a);\
                }\
                return 'not thrown';\
            })()",
            "undefined",
        ),
        // A later descriptor that fails validation does too.
        (
            "(function(){\
                const target={};\
                try {\
                    Object.defineProperties(target,{a:{value:1},b:{get(){},value:2}});\
                } catch (thrown) {\
                    return String(target.a);\
                }\
                return 'not thrown';\
            })()",
            "undefined",
        ),
        // A later *definition* that is refused still leaves the earlier ones,
        // because by then the validation phase is over.
        (
            "(function(){\
                const target={};\
                Object.defineProperty(target,'b',{value:0,configurable:false});\
                try {\
                    Object.defineProperties(target,{a:{value:1},b:{value:2}});\
                } catch (thrown) {\
                    return target.a+'|'+target.b;\
                }\
                return 'not thrown';\
            })()",
            "1|0",
        ),
    ]);
    // A malformed descriptor is still a `TypeError`, reported during validation.
    assert_throws(
        "return Object.defineProperties({},{a:{get:1}});",
        ExceptionKind::TypeError,
        "invalid getter",
    );
    assert_throws(
        "return Object.defineProperties({},{a:{get(){},value:1}});",
        ExceptionKind::TypeError,
        "cannot have setter/getter and value or writable",
    );
}

/// `Object.create` runs the same operation on the object it creates.
#[test]
fn object_create_admits_its_descriptors_argument() {
    assert_all(&[
        (
            "(function(){\
                const o=Object.create(null,{a:{value:1,enumerable:true}});\
                return o.a+'|'+(Object.getPrototypeOf(o)===null);\
            })()",
            "1|true",
        ),
        (
            "(function(){\
                const parent={z:9};\
                const o=Object.create(parent,{a:{value:1}});\
                return o.a+'|'+o.z;\
            })()",
            "1|9",
        ),
        // An absent or `undefined` argument creates the object bare.
        (
            "Object.getPrototypeOf(Object.create(null,undefined))",
            "null",
        ),
        ("Reflect.ownKeys(Object.create(null)).length", "0"),
        // A descriptor accessor runs against the created object.
        (
            "(function(){\
                let log='';\
                const o=Object.create(null,{a:{get value(){log+='v';return 1;}}});\
                return log+'|'+o.a;\
            })()",
            "v|1",
        ),
    ]);
    assert_throws(
        "return Object.create(null,{a:1});",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Object.create(1,{});",
        ExceptionKind::TypeError,
        "not a prototype",
    );
}

/// Both statics carry the pinned `name` and `length`.
#[test]
fn both_statics_carry_the_pinned_identity() {
    assert_all(&[
        ("Object.defineProperties.length", "2"),
        ("Object.defineProperties.name", "defineProperties"),
        ("Object.create.length", "2"),
        (
            "Object.getOwnPropertyDescriptor(Object,'defineProperties').enumerable",
            "false",
        ),
    ]);
}
