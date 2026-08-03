//! The `Reflect` namespace: `apply`, `construct`, the eleven object-reflection
//! mirrors, and the namespace object's own shape.
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
//!
//! Oracle transcript for the object-reflection mirrors:
//!
//! ```text
//! Reflect.get(1, key) !! TypeError: not an object (key.toString never runs)
//! Reflect.get({get a(){return this.m}}, "a", {m:7}) => 7
//! Reflect.has([1,,3], "1") => false (a hole is absent)
//! Reflect.deleteProperty(Object.freeze({a:1}), "a") => false (no throw)
//! Reflect.ownKeys({b:1,2:1,0:1,a:1}) => ["0","2","b","a"]
//! Reflect.ownKeys(o with a symbol key) => the Symbol itself, after the strings
//! Reflect.ownKeys(Reflect).length => 14
//! Reflect.setPrototypeOf(Object.freeze({}), null) => false
//! Reflect.setPrototypeOf(Object.preventExtensions(Object.create(null)), null) => true
//! Reflect.preventExtensions({}) => true (a boolean, not the target)
//! Reflect.defineProperty(Object.freeze({}), "a", {value:1}) => false
//! Reflect.defineProperty({}, "a", {get:1}) !! TypeError: invalid getter
//! Reflect.set(Object.freeze({a:1}), "a", 2) => false
//! Reflect.set({a:1}, "a", 5, receiver) => true, receiver.a === 5, target.a === 1
//! Reflect.set({a:1}, "a", 5, Object.freeze({a:9})) => false
//! Reflect.set({set a(v){this.seen=v}}, "a", 5, receiver) => receiver.seen === 5
//! Reflect.set({}, "0", 5, []) => true, the array receiver's length becomes 1
//! Reflect.set([1,2,3], "length", -1) !! RangeError: invalid array length
//! lengths: get 2, set 3, has 2, deleteProperty 2, ownKeys 1, getPrototypeOf 1,
//!   setPrototypeOf 2, isExtensible 1, preventExtensions 1, defineProperty 3,
//!   getOwnPropertyDescriptor 2
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

/// Every `Reflect` method rejects a non-object target, before any key
/// conversion can run a user `toString`.
#[test]
fn every_method_rejects_a_non_object_target() {
    // Oracle: each of these reports `TypeError: not an object`, and the key's
    // `toString` never runs because the target check precedes `ToPropertyKey`.
    for body in [
        "return Reflect.get(1,'a');",
        "return Reflect.set(1,'a',1);",
        "return Reflect.has(1,'a');",
        "return Reflect.deleteProperty(1,'a');",
        "return Reflect.defineProperty(1,'a',{});",
        "return Reflect.getOwnPropertyDescriptor(1,'a');",
        "return Reflect.ownKeys(1);",
        "return Reflect.getPrototypeOf(1);",
        "return Reflect.isExtensible(1);",
        "return Reflect.preventExtensions(1);",
        "return Reflect.setPrototypeOf(1,null);",
        "return Reflect.getPrototypeOf();",
        "return Reflect.ownKeys(null);",
        "return Reflect.get('str','length');",
    ] {
        assert_throws(body, ExceptionKind::TypeError, "not an object");
    }
    // The target check runs first, so a throwing key getter is never reached.
    assert_throws(
        "(function(){\
            const key={toString(){throw new Error('key');}};\
            Reflect.get(1,key);\
        })()",
        ExceptionKind::TypeError,
        "not an object",
    );
    // `setPrototypeOf` checks its target before its prototype, and rejects a
    // non-object prototype rather than answering `false`.
    assert_throws(
        "return Reflect.setPrototypeOf({},1);",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Reflect.setPrototypeOf({},undefined);",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Reflect.setPrototypeOf();",
        ExceptionKind::TypeError,
        "not an object",
    );
}

/// `Reflect.get` reads through the prototype chain with an optional receiver.
#[test]
fn reflect_get_reads_with_the_requested_receiver() {
    assert_all(&[
        ("Reflect.get({a:1},'a')", "1"),
        ("Reflect.get({a:1},'b')", "undefined"),
        (
            "Reflect.get({},'toString')===Object.prototype.toString",
            "true",
        ),
        // An accessor runs with the supplied receiver as its `this`.
        ("Reflect.get({get a(){return this.m}},'a',{m:7})", "7"),
        // An omitted receiver defaults to the target rather than `undefined`.
        ("Reflect.get({m:3,get a(){return this.m}},'a')", "3"),
        ("Reflect.get([1,2],'length')", "2"),
        // A boxed `String`'s exotic index is readable.
        ("Reflect.get(Object('ab'),'1')", "b"),
        // The key converts with `ToPropertyKey`, which can run a `toString`.
        (
            "(function(){\
                let log='';\
                const key={toString(){log+='k';return 'a';}};\
                const value=Reflect.get({a:1},key);\
                return log+'|'+value;\
            })()",
            "k|1",
        ),
    ]);
}

/// `Reflect.has` is the `in` operator, and `Reflect.deleteProperty` is a
/// non-throwing `delete`.
#[test]
fn reflect_has_and_delete_answer_with_booleans() {
    assert_all(&[
        ("Reflect.has({a:1},'a')", "true"),
        ("Reflect.has({},'toString')", "true"),
        // A hole is absent, unlike an explicit `undefined`.
        ("Reflect.has([1,,3],'1')", "false"),
        ("Reflect.has(Object('ab'),'1')", "true"),
        ("Reflect.has(Object('ab'),'5')", "false"),
        ("Reflect.deleteProperty({a:1},'a')", "true"),
        // A non-configurable property refuses, which is `false` rather than the
        // strict-mode `delete`'s `TypeError`.
        ("Reflect.deleteProperty(Object.freeze({a:1}),'a')", "false"),
        // An absent property reports success.
        ("Reflect.deleteProperty({},'a')", "true"),
        ("Reflect.deleteProperty([1,2],'length')", "false"),
    ]);
}

/// `Reflect.ownKeys` reports string *and* symbol keys, in the
/// `[[OwnPropertyKeys]]` order.
#[test]
fn reflect_own_keys_reports_both_key_phases() {
    assert_all(&[
        // Ascending indices, then string keys in creation order.
        ("Reflect.ownKeys({b:1,2:1,0:1,a:1}).join(',')", "0,2,b,a"),
        ("Reflect.ownKeys([1,2]).join(',')", "0,1,length"),
        // A boxed `String` reports its virtual indices ahead of `length`.
        ("Reflect.ownKeys(Object('ab')).join(',')", "0,1,length"),
        (
            "Reflect.ownKeys(function f(a){}).join(',')",
            "length,name,prototype",
        ),
        // A symbol key is reported as the Symbol itself, after every string
        // key, and compares identical to the original.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={a:1};\
                o[s]=2;\
                const keys=Reflect.ownKeys(o);\
                return keys.length+'|'+String(keys[0])+'|'+String(keys[1])+'|'+(keys[1]===s);\
            })()",
            "2|a|Symbol(q)|true",
        ),
        // The namespace's own listing: `apply`, `construct`, the eleven
        // alphabetical methods, then the symbol tag.
        ("Reflect.ownKeys(Reflect).length", "14"),
        (
            "Reflect.ownKeys(Reflect).slice(0,4).join(',')",
            "apply,construct,defineProperty,deleteProperty",
        ),
        (
            "String(Reflect.ownKeys(Reflect)[13])",
            "Symbol(Symbol.toStringTag)",
        ),
    ]);
}

/// The prototype and extensibility mirrors answer with booleans rather than the
/// target or a `TypeError`.
#[test]
fn the_prototype_and_extensibility_mirrors_answer_with_booleans() {
    assert_all(&[
        ("Reflect.getPrototypeOf({})===Object.prototype", "true"),
        ("Reflect.getPrototypeOf(Object.create(null))", "null"),
        ("Reflect.getPrototypeOf([])===Array.prototype", "true"),
        ("Reflect.setPrototypeOf({},null)", "true"),
        (
            "(function(){\
                const o={};\
                const ok=Reflect.setPrototypeOf(o,Array.prototype);\
                return ok+'|'+(Reflect.getPrototypeOf(o)===Array.prototype);\
            })()",
            "true|true",
        ),
        // A non-extensible object refuses a changed prototype but accepts an
        // unchanged one, because the comparison precedes the check.
        ("Reflect.setPrototypeOf(Object.freeze({}),null)", "false"),
        (
            "Reflect.setPrototypeOf(Object.preventExtensions(Object.create(null)),null)",
            "true",
        ),
        // A cycle refuses rather than throwing.
        (
            "(function(){\
                const a={};\
                const b=Object.create(a);\
                return Reflect.setPrototypeOf(a,b);\
            })()",
            "false",
        ),
        // A function is a valid prototype, since it is an object.
        (
            "(function(){\
                const o={};\
                const ok=Reflect.setPrototypeOf(o,Array.prototype.map);\
                return ok+'|'+(Reflect.getPrototypeOf(o)===Array.prototype.map);\
            })()",
            "true|true",
        ),
        ("Reflect.isExtensible({})", "true"),
        (
            "Reflect.isExtensible(Object.preventExtensions({}))",
            "false",
        ),
        // `preventExtensions` answers `true` rather than the target.
        ("Reflect.preventExtensions({})", "true"),
        (
            "(function(){\
                const o={};\
                const ok=Reflect.preventExtensions(o);\
                return ok+'|'+Reflect.isExtensible(o);\
            })()",
            "true|false",
        ),
    ]);
}

/// `Reflect.defineProperty` shares `Object.defineProperty`'s descriptor read
/// and differs only in reporting a rejection as `false`.
#[test]
fn reflect_define_property_answers_with_a_boolean() {
    assert_all(&[
        ("Reflect.defineProperty({},'a',{value:1})", "true"),
        // A rejection is `false`, where `Object.defineProperty` throws.
        (
            "Reflect.defineProperty(Object.freeze({}),'a',{value:1})",
            "false",
        ),
        (
            "Reflect.defineProperty(Object.freeze({a:1}),'a',{value:2})",
            "false",
        ),
        // An omitted attribute defaults to `false`, unlike an assignment.
        (
            "(function(){\
                const o={};\
                Reflect.defineProperty(o,'a',{value:1});\
                const d=Reflect.getOwnPropertyDescriptor(o,'a');\
                return d.value+'|'+d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "1|false|false|false",
        ),
        // An array index routes through the exotic define, extending the
        // cached length.
        (
            "(function(){\
                const a=[1];\
                const ok=Reflect.defineProperty(a,'3',{value:9});\
                return ok+'|'+a.length+'|'+a[3];\
            })()",
            "true|4|9",
        ),
        // The descriptor fields are read in `ToPropertyDescriptor` order and
        // each read can enter a getter.
        (
            "(function(){\
                let log='';\
                const d={};\
                for (const field of ['writable','value','configurable','enumerable']) {\
                    Object.defineProperty(d,field,{get(){log+=field+'|';return undefined;},configurable:true});\
                }\
                Reflect.defineProperty({},'a',d);\
                return log;\
            })()",
            "enumerable|configurable|value|writable|",
        ),
    ]);
    // A malformed descriptor is still a `TypeError`: the boolean answer covers
    // the definition's rejection, not the descriptor's validation.
    assert_throws(
        "return Reflect.defineProperty({},'a',1);",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Reflect.defineProperty({},'a',{get:1});",
        ExceptionKind::TypeError,
        "invalid getter",
    );
    assert_throws(
        "return Reflect.defineProperty({},'a',{get(){},value:1});",
        ExceptionKind::TypeError,
        "cannot have setter/getter and value or writable",
    );
}

/// `Reflect.getOwnPropertyDescriptor` materializes a fully mutable descriptor.
#[test]
fn reflect_get_own_property_descriptor_materializes_a_descriptor() {
    assert_all(&[
        (
            "(function(){\
                const d=Reflect.getOwnPropertyDescriptor({a:1},'a');\
                return d.value+'|'+d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "1|true|true|true",
        ),
        ("Reflect.getOwnPropertyDescriptor({},'a')", "undefined"),
        // An inherited property is not an own one.
        (
            "Reflect.getOwnPropertyDescriptor({},'toString')",
            "undefined",
        ),
        (
            "(function(){\
                const d=Reflect.getOwnPropertyDescriptor([1],'length');\
                return d.value+'|'+d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "1|true|false|false",
        ),
        // A symbol key resolves the same way a string one does.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={};\
                o[s]=1;\
                const d=Reflect.getOwnPropertyDescriptor(o,s);\
                return d.value+'|'+d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "1|true|true|true",
        ),
    ]);
}

/// `Reflect.set` with the default receiver is the ordinary assignment, reported
/// as a boolean instead of throwing or silently succeeding.
#[test]
fn reflect_set_answers_the_ordinary_assignment_with_a_boolean() {
    assert_all(&[
        ("Reflect.set({},'a',1)", "true"),
        (
            "(function(){\
                const o={};\
                const ok=Reflect.set(o,'a',5);\
                return ok+'|'+o.a;\
            })()",
            "true|5",
        ),
        // A non-writable property refuses instead of throwing.
        ("Reflect.set(Object.freeze({a:1}),'a',2)", "false"),
        // A boxed `String`'s exotic index is non-writable.
        ("Reflect.set(Object('ab'),'0','z')", "false"),
        // An array index extends the cached length.
        (
            "(function(){\
                const a=[];\
                return Reflect.set(a,'5',1)+'|'+a.length;\
            })()",
            "true|6",
        ),
        // An array's `length` converts with `ToNumber` before its range check.
        (
            "(function(){\
                const a=[1,2,3];\
                return Reflect.set(a,'length',1)+'|'+a.length;\
            })()",
            "true|1",
        ),
        (
            "(function(){\
                const a=[1,2,3];\
                return Reflect.set(a,'length',{valueOf(){return 1;}})+'|'+a.length;\
            })()",
            "true|1",
        ),
        // A non-writable `length` refuses as `false`, not as the strict-mode
        // assignment's `'length' is read-only`.
        (
            "(function(){\
                const a=Object.freeze([1,2,3]);\
                return Reflect.set(a,'length',1)+'|'+a.length;\
            })()",
            "false|3",
        ),
    ]);
    // A length outside the array-length domain still reports a `RangeError`,
    // because the boolean answer covers a refusal, not an invalid value.
    assert_throws(
        "return Reflect.set([1,2,3],'length',-1);",
        ExceptionKind::RangeError,
        "invalid array length",
    );
}

/// A differing `Reflect.set` receiver splits the lookup from the storage.
#[test]
fn reflect_set_stores_on_a_differing_receiver() {
    assert_all(&[
        // The target supplies the lookup; the receiver gains the property.
        (
            "(function(){\
                const target={a:1};\
                const receiver={};\
                const ok=Reflect.set(target,'a',5,receiver);\
                return ok+'|'+target.a+'|'+receiver.a;\
            })()",
            "true|1|5",
        ),
        // The created property is a fully mutable data property.
        (
            "(function(){\
                const receiver={};\
                Reflect.set({a:1},'a',5,receiver);\
                const d=Reflect.getOwnPropertyDescriptor(receiver,'a');\
                return d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "true|true|true",
        ),
        // An existing receiver property is updated in place, keeping its
        // attributes rather than being redefined as fully mutable.
        (
            "(function(){\
                const receiver={};\
                Object.defineProperty(receiver,'a',{value:1,writable:true,enumerable:false,configurable:false});\
                const ok=Reflect.set({a:2},'a',7,receiver);\
                const d=Reflect.getOwnPropertyDescriptor(receiver,'a');\
                return ok+'|'+d.value+'|'+d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "true|7|true|false|false",
        ),
        // A non-writable or accessor own property on the receiver refuses even
        // though the target's was writable.
        (
            "(function(){\
                const receiver=Object.freeze({a:9});\
                return Reflect.set({a:1},'a',5,receiver)+'|'+receiver.a;\
            })()",
            "false|9",
        ),
        (
            "(function(){\
                let log='';\
                const receiver={set a(v){log+=v;}};\
                return Reflect.set({a:1},'a',5,receiver)+'|'+log;\
            })()",
            "false|",
        ),
        // A non-extensible receiver refuses a new property.
        (
            "(function(){\
                const receiver=Object.preventExtensions({});\
                return Reflect.set({a:1},'a',5,receiver)+'|'+receiver.a;\
            })()",
            "false|undefined",
        ),
    ]);
}

/// A differing `Reflect.set` receiver still runs the target's setter, with the
/// receiver as its `this`.
#[test]
fn a_differing_receiver_becomes_the_setter_receiver() {
    assert_all(&[
        // A target setter runs with the receiver as its `this`, wherever on the
        // chain it was found, and the operation answers `true`.
        (
            "(function(){\
                const target={set a(v){this.seen=v;}};\
                const receiver={};\
                return Reflect.set(target,'a',5,receiver)+'|'+receiver.seen+'|'+target.seen;\
            })()",
            "true|5|undefined",
        ),
        (
            "(function(){\
                let log='';\
                const grandparent={set a(v){log+=v+':'+this.tag;}};\
                const parent=Object.create(grandparent);\
                const target=Object.create(parent);\
                return Reflect.set(target,'a',5,{tag:'R'})+'|'+log;\
            })()",
            "true|5:R",
        ),
        // A getter-only target property refuses without touching the receiver.
        (
            "(function(){\
                const receiver={};\
                return Reflect.set({get a(){return 1;}},'a',5,receiver)+'|'+receiver.a;\
            })()",
            "false|undefined",
        ),
        // An inherited data property on the target does not become an own one:
        // the definition lands on the receiver.
        (
            "(function(){\
                const target=Object.create({a:1});\
                const receiver={};\
                const ok=Reflect.set(target,'a',5,receiver);\
                return ok+'|'+receiver.a+'|'+Object.prototype.hasOwnProperty.call(target,'a');\
            })()",
            "true|5|false",
        ),
        // A non-writable inherited property refuses.
        (
            "(function(){\
                const parent={};\
                Object.defineProperty(parent,'a',{value:1,writable:false});\
                const receiver={};\
                return Reflect.set(Object.create(parent),'a',5,receiver)+'|'+receiver.a;\
            })()",
            "false|undefined",
        ),
        // A null-prototype target still stores on the receiver.
        (
            "(function(){\
                const receiver={};\
                return Reflect.set(Object.create(null),'a',5,receiver)+'|'+receiver.a;\
            })()",
            "true|5",
        ),
        // A primitive receiver can never store the result.
        ("Reflect.set({a:1},'a',5,'str')", "false"),
        ("Reflect.set({},'a',5,'str')", "false"),
        // A receiver whose own property does not exist yet is created even when
        // the target has nothing at all.
        (
            "(function(){\
                const receiver={a:1};\
                return Reflect.set({},'a',5,receiver)+'|'+receiver.a;\
            })()",
            "true|5",
        ),
    ]);
}

/// A differing receiver keeps its own exotic behaviors.
#[test]
fn a_differing_receiver_keeps_its_exotics() {
    assert_all(&[
        // An array receiver extends its cached length through the definition.
        (
            "(function(){\
                const receiver=[];\
                return Reflect.set({},'0',5,receiver)+'|'+receiver.length+'|'+receiver[0];\
            })()",
            "true|1|5",
        ),
        (
            "(function(){\
                const receiver=[1];\
                return Reflect.set({},'3',9,receiver)+'|'+receiver.length;\
            })()",
            "true|4",
        ),
        (
            "(function(){\
                const receiver=Object.freeze([1]);\
                return Reflect.set({},'0',9,receiver)+'|'+receiver[0];\
            })()",
            "false|1",
        ),
        // An array receiver's `length` keeps the resumable conversion, so the
        // `RangeError` still outranks the boolean answer.
        (
            "(function(){\
                const receiver=[1,2,3];\
                return Reflect.set({},'length',1,receiver)+'|'+receiver.length;\
            })()",
            "true|1",
        ),
        (
            "(function(){\
                const receiver=[1,2,3];\
                return Reflect.set({},'length',{valueOf(){return 1;}},receiver)+'|'+receiver.length;\
            })()",
            "true|1",
        ),
        (
            "(function(){\
                const receiver=Object.freeze([1,2,3]);\
                return Reflect.set({},'length',1,receiver)+'|'+receiver.length;\
            })()",
            "false|3",
        ),
        // An array *target*'s `length` is only consulted for the lookup, so its
        // value stays put while the receiver gains an ordinary property.
        (
            "(function(){\
                const target=[1,2,3];\
                const receiver={};\
                const ok=Reflect.set(target,'length',1,receiver);\
                return ok+'|'+target.length+'|'+receiver.length;\
            })()",
            "true|3|1",
        ),
        (
            "(function(){\
                const target=Object.freeze([1,2,3]);\
                const receiver={};\
                return Reflect.set(target,'length',1,receiver)+'|'+receiver.length;\
            })()",
            "false|undefined",
        ),
        // A boxed `String` receiver refuses an in-range index and accepts an
        // out-of-range one.
        ("Reflect.set({},'0','z',Object('ab'))", "false"),
        (
            "(function(){\
                const receiver=Object('ab');\
                return Reflect.set({},'5','z',receiver)+'|'+receiver[5];\
            })()",
            "true|z",
        ),
        // A function receiver's non-writable `name` refuses.
        (
            "(function(){\
                const receiver=function f(){};\
                return Reflect.set({},'name','z',receiver)+'|'+receiver.name;\
            })()",
            "false|f",
        ),
    ]);
    assert_throws(
        "return Reflect.set({},'length',-1,[1,2,3]);",
        ExceptionKind::RangeError,
        "invalid array length",
    );
}

/// Every installed method carries the pinned `name` and `length`.
#[test]
fn every_method_carries_the_pinned_identity() {
    assert_all(&[
        ("Reflect.get.length", "2"),
        ("Reflect.set.length", "3"),
        ("Reflect.has.length", "2"),
        ("Reflect.deleteProperty.length", "2"),
        ("Reflect.ownKeys.length", "1"),
        ("Reflect.getPrototypeOf.length", "1"),
        ("Reflect.setPrototypeOf.length", "2"),
        ("Reflect.isExtensible.length", "1"),
        ("Reflect.preventExtensions.length", "1"),
        ("Reflect.defineProperty.length", "3"),
        ("Reflect.getOwnPropertyDescriptor.length", "2"),
        ("Reflect.get.name", "get"),
        ("Reflect.deleteProperty.name", "deleteProperty"),
        (
            "Reflect.getOwnPropertyDescriptor.name",
            "getOwnPropertyDescriptor",
        ),
        // A `Reflect` method is an ordinary function object, not a constructor.
        ("typeof Reflect.get", "function"),
        (
            "Object.getPrototypeOf(Reflect.get)===Function.prototype",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Reflect,'get').enumerable",
            "false",
        ),
    ]);
}
