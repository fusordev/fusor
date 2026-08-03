//! `Reflect.apply` and `Reflect.construct`, plus the `Reflect` namespace
//! object's own shape.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'function Custom() {}\
//!     Custom.prototype = {marker: "custom"};\
//!     const e = Reflect.construct(Error, ["x"], Custom);\
//!     console.log(Object.getPrototypeOf(e) === Custom.prototype, e.marker, e.message);'
//! true custom x
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Reflect.apply(f, {t:9}, [1,2]) => receiver and list forwarded
//! Reflect.apply(5, null, []) !! TypeError: not a function
//! Reflect.apply(f, null, null) !! TypeError: not a object (no nullish special case)
//! Reflect.construct(Error, ["x"]) => message "x", Error.prototype
//! Reflect.construct(Error, ["x"], Custom) => Custom.prototype is selected
//! Reflect.construct(TypeError, ["x"], Custom.prototype=1) => TypeError.prototype fallback
//! Reflect.construct(Error, ["x"], 5) !! TypeError: not a constructor
//! Reflect.construct(Error, ["x"], Array.prototype.map) !! TypeError: map is not a constructor
//! Reflect.construct(5, ["x"]) !! TypeError: not a function (after the list is read)
//! Reflect.construct(Error, 5) !! TypeError: not a object
//! lengths: apply 3, construct 2; Reflect[Symbol.toStringTag] => "Reflect"
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
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Reflect>"))
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

/// `Reflect.apply` forwards the receiver and the argument list.
#[test]
fn reflect_apply_forwards_receiver_and_arguments() {
    assert_all(&[
        (
            "Reflect.apply(function(a,b){return a+','+b+','+this.t;},{t:9},[1,2])",
            "1,2,9",
        ),
        (
            "Reflect.apply(function(){return this===undefined;},null,[])",
            "false",
        ),
        // An array-like argument list is read element by element.
        (
            "Reflect.apply(function(a,b){return a+','+b;},null,{length:2,0:'x',1:'y'})",
            "x,y",
        ),
        // A missing index applies `undefined`.
        (
            "Reflect.apply(function(a){return String(a);},null,{length:1})",
            "undefined",
        ),
    ]);
}

/// `Reflect.apply` validates its target first and rejects a nullish list,
/// which `Function.prototype.apply` would treat as empty
/// (`quickjs.c:41103-41107` with magic 2).
#[test]
fn reflect_apply_rejects_bad_targets_and_lists() {
    assert_throws(
        "return Reflect.apply(5,null,[]);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return Reflect.apply(function(){},null,null);",
        ExceptionKind::TypeError,
        "not a object",
    );
    assert_throws(
        "return Reflect.apply(function(){},null,5);",
        ExceptionKind::TypeError,
        "not a object",
    );
}

/// `Reflect.construct` builds with `newTarget`, defaulting to the target.
#[test]
fn reflect_construct_builds_with_new_target() {
    assert_all(&[
        // Without `newTarget`, the target itself is used.
        (
            "(function(){\
                const e=Reflect.construct(Error,['x']);\
                return e.message+'|'+(Object.getPrototypeOf(e)===Error.prototype);\
            })()",
            "x|true",
        ),
        // A custom `newTarget` selects its `prototype`.
        (
            "(function(){\
                function Custom(){}\
                Custom.prototype={marker:'custom'};\
                const e=Reflect.construct(Error,['x'],Custom);\
                return (Object.getPrototypeOf(e)===Custom.prototype)+'|'+e.marker+'|'+e.message+'|'+Error.isError(e);\
            })()",
            "true|custom|x|true",
        ),
        // A non-object `newTarget.prototype` falls back to the family
        // intrinsic, not the generic `Error.prototype`.
        (
            "(function(){\
                function Custom(){}\
                Custom.prototype=1;\
                const e=Reflect.construct(TypeError,['x'],Custom);\
                return (Object.getPrototypeOf(e)===TypeError.prototype)+'|'+e.name;\
            })()",
            "true|TypeError",
        ),
        // `AggregateError` collects its list under a custom prototype.
        (
            "(function(){\
                function Custom(){}\
                Custom.prototype={name:'CustomAggregate',marker:'custom'};\
                const e=Reflect.construct(AggregateError,[[1],'many'],Custom);\
                return (Object.getPrototypeOf(e)===Custom.prototype)+'|'+e.errors.length+':'+e.errors[0]+'|'+Error.prototype.toString.call(e)+'|'+e.marker;\
            })()",
            "true|1:1|CustomAggregate: many|custom",
        ),
    ]);
}

/// `Reflect.construct` validation order is pinned: `newTarget` first, the
/// argument list second, and the target last (`quickjs.c:50195-50206`).
#[test]
fn reflect_construct_validates_in_the_pinned_order() {
    assert_throws(
        "return Reflect.construct(Error,['x'],5);",
        ExceptionKind::TypeError,
        "not a constructor",
    );
    // A non-constructor function as `newTarget` reports with its name.
    assert_throws(
        "return Reflect.construct(Error,['x'],Array.prototype.map);",
        ExceptionKind::TypeError,
        "map is not a constructor",
    );
    assert_throws(
        "return Reflect.construct(Error,5);",
        ExceptionKind::TypeError,
        "not a object",
    );
    assert_throws(
        "return Reflect.construct(Error);",
        ExceptionKind::TypeError,
        "not a object",
    );
    // The target is checked only after the argument list is read: the length
    // getter runs before the `not a function` report.
    assert_throws(
        "(function(){\
            const list={get length(){return 0;}};\
            Reflect.construct(5,list);\
        })()",
        ExceptionKind::TypeError,
        "not a function",
    );
}

/// Every argument-list read can enter an accessor, in order.
#[test]
fn the_argument_list_reads_enter_accessors_in_order() {
    assert_all(&[
        (
            "(function(){\
                let log='';\
                const list={get length(){log+='len|';return 1;}};\
                Object.defineProperty(list,0,{get(){log+='e0|';return 'z';},configurable:true});\
                const e=Reflect.construct(Error,list);\
                return log+'|'+e.message;\
            })()",
            "len|e0||z",
        ),
        (
            "(function(){\
                let log='';\
                const list={get length(){log+='len|';return 1;}};\
                Object.defineProperty(list,0,{get(){log+='e0|';return 'z';},configurable:true});\
                Reflect.apply(function(a){log+='call:'+a;},null,list);\
                return log;\
            })()",
            "len|e0|call:z",
        ),
    ]);
}

/// The `Reflect` namespace object carries the pinned shape.
#[test]
fn the_reflect_namespace_has_the_pinned_shape() {
    assert_all(&[
        ("typeof Reflect", "object"),
        ("Reflect[Symbol.toStringTag]", "Reflect"),
        ("Object.getPrototypeOf(Reflect)===Object.prototype", "true"),
        ("Reflect.apply.length", "3"),
        ("Reflect.construct.length", "2"),
        ("Reflect.apply.name", "apply"),
        ("Reflect.construct.name", "construct"),
        (
            "Object.getOwnPropertyDescriptor(Reflect,'construct').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Reflect,'construct').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Reflect,'construct').configurable",
            "true",
        ),
        // The `Reflect` global property exists and is a plain object.
        ("Reflect instanceof Object", "true"),
    ]);
}
