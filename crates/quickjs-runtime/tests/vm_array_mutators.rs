//! `Array.prototype.push`, `pop`, `shift`, `unshift`, `reverse`, `fill`, and
//! `copyWithin`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const a=[1,,3]; a.reverse();\
//!     console.log(a.join(), Object.prototype.hasOwnProperty.call(a,1));'
//! 3,,1 false
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [1,2,3].push(4) => 4, array "1,2,3,4"     [].push() => 0
//! [1,2,3].pop() => 3, array "1,2"           [].pop() => undefined
//! [1,2,3].shift() => 1, array "2,3"         [].shift() => undefined
//! [2,3].unshift(1) => 3, array "1,2,3"      [1].unshift() => 1
//! [1,2,3].reverse() => "3,2,1"              [].reverse().length => 0
//! [1,2,3].fill(0,1,2) => "1,0,3"            [1,2,3].fill(0,2,1) => "1,2,3"
//! [1,2,3].fill(0,0,-1) => "0,0,3"           [1,2].fill()[0] => undefined
//! holes: [,2].shift() leaves index 0 present
//!        [1,,3].reverse() keeps index 1 absent
//!        [,2].unshift(0) keeps index 1 absent
//! array-likes: push on {length:1,0:"a"} => length 2, [1] === "b"
//!              pop on {length:-3} sets length 0
//!              reverse on {length:3,0:"a",2:"c"} => {"0":"c","2":"a"}
//! order: push logs getlen|set1:x|setlen:2
//!        pop  logs getlen|get1|setlen:1
//!        fill logs len|start|end, and never coerces its value
//! copyWithin: [1,2,3,4,5].copyWithin(1,0,4) => "1,1,2,3,4"
//!             overlap reads/writes in reverse: get1|set2:y|get0|set1:x
//!             an absent source deletes its destination
//! push past 2^53-1 !! TypeError: Array loo long
//! lengths: push 1, pop 0, shift 0, unshift 1, reverse 0, fill 1, copyWithin 2
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
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime Array mutators>"),
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

/// `push` appends its arguments and returns the new length.
#[test]
fn push_appends_and_returns_the_new_length() {
    assert_all(&[
        (
            "(function(){const a=[1,2,3];const r=a.push(4);return r+'|'+a.join()+'|'+a.length;})()",
            "4|1,2,3,4|4",
        ),
        (
            "(function(){const a=[1];return a.push(2,3)+'|'+a.join();})()",
            "3|1,2,3",
        ),
        // No arguments still reports the length.
        (
            "(function(){const a=[];return a.push()+'|'+a.length;})()",
            "0|0",
        ),
    ]);
}

/// `pop` returns the removed element and deletes it.
#[test]
fn pop_removes_the_last_element() {
    assert_all(&[
        (
            "(function(){const a=[1,2,3];const r=a.pop();return r+'|'+a.join()+'|'+a.length;})()",
            "3|1,2|2",
        ),
        (
            "(function(){const a=[];return String(a.pop())+'|'+a.length;})()",
            "undefined|0",
        ),
        // The index is deleted rather than left as `undefined`.
        (
            "(function(){const a=[1,2];a.pop();return Object.prototype.hasOwnProperty.call(a,1);})()",
            "false",
        ),
        // Popping a hole answers `undefined` and still shrinks the length.
        (
            "(function(){const a=[1,,];const r=a.pop();return String(r)+'|'+a.length;})()",
            "undefined|1",
        ),
    ]);
}

/// `shift` and `unshift` slide the remaining elements.
#[test]
fn shift_and_unshift_slide_the_elements() {
    assert_all(&[
        (
            "(function(){const a=[1,2,3];const r=a.shift();return r+'|'+a.join()+'|'+a.length;})()",
            "1|2,3|2",
        ),
        (
            "(function(){const a=[];return String(a.shift())+'|'+a.length;})()",
            "undefined|0",
        ),
        (
            "(function(){const a=[2,3];const r=a.unshift(1);return r+'|'+a.join();})()",
            "3|1,2,3",
        ),
        (
            "(function(){const a=[3];return a.unshift(1,2)+'|'+a.join();})()",
            "3|1,2,3",
        ),
        // No arguments leaves the array alone but still reports the length.
        (
            "(function(){const a=[1];return a.unshift()+'|'+a.join();})()",
            "1|1",
        ),
    ]);
}

/// `reverse` exchanges each pair in place and returns the same object.
#[test]
fn reverse_exchanges_pairs_in_place() {
    assert_all(&[
        ("[1,2,3].reverse().join()", "3,2,1"),
        ("[1,2,3,4].reverse().join()", "4,3,2,1"),
        ("[].reverse().length", "0"),
        ("[1].reverse().join()", "1"),
        // The receiver itself is returned, not a copy.
        (
            "(function(){const a=[1,2];return a.reverse()===a;})()",
            "true",
        ),
    ]);
}

/// `fill` writes one value across a resolved range.
#[test]
fn fill_writes_across_its_resolved_range() {
    assert_all(&[
        ("[1,2,3].fill(0).join()", "0,0,0"),
        ("[1,2,3].fill(0,1).join()", "1,0,0"),
        ("[1,2,3].fill(0,1,2).join()", "1,0,3"),
        // Negative bounds count from the end.
        ("[1,2,3].fill(0,-2).join()", "1,0,0"),
        ("[1,2,3].fill(0,0,-1).join()", "0,0,3"),
        // Crossed bounds fill nothing.
        ("[1,2,3].fill(0,2,1).join()", "1,2,3"),
        ("(function(){const a=[1];return a.fill(0)===a;})()", "true"),
        // An absent value fills with `undefined`.
        ("String([1,2].fill()[0])", "undefined"),
    ]);
}

/// `copyWithin` copies a resolved range in place and returns its receiver.
#[test]
fn copy_within_copies_in_place() {
    assert_all(&[
        ("[1,2,3,4,5].copyWithin(0,3).join()", "4,5,3,4,5"),
        // Overlap requires a backward walk so unread sources survive.
        ("[1,2,3,4,5].copyWithin(1,0,4).join()", "1,1,2,3,4"),
        // Negative bounds are relative to the snapshotted length.
        ("[1,2,3,4,5].copyWithin(-2,-4,-1).join()", "1,2,3,2,3"),
        // Explicit `undefined`, like an absent end, copies through the length.
        ("[1,2,3,4].copyWithin(0,2,undefined).join()", "3,4,3,4"),
        ("[1,2,3].copyWithin(0,3,1).join()", "1,2,3"),
        (
            "(function(){const a=[1,2];return a.copyWithin(0,1)===a;})()",
            "true",
        ),
        // `ToObject` returns a wrapper rather than the original primitive.
        (
            "Object.prototype.toString.call(Array.prototype.copyWithin.call(3,0,0))",
            "[object Number]",
        ),
    ]);
}

/// Holes survive a move rather than becoming `undefined`.
///
/// An absent source is deleted at its destination, so a sparse array stays
/// sparse. This is the same distinction `hasOwnProperty` and `indexOf` rely on.
#[test]
fn holes_are_preserved_across_moves() {
    assert_all(&[
        // The element that slides into index 0 is present, so index 0 stays
        // present even though it began as a hole.
        (
            "(function(){\
                const a=[,2];\
                const r=a.shift();\
                return String(r)+'|'+a.join()+'|'+Object.prototype.hasOwnProperty.call(a,0);\
            })()",
            "undefined|2|true",
        ),
        // Reversing keeps the middle hole absent.
        (
            "(function(){\
                const a=[1,,3];\
                a.reverse();\
                return a.join()+'|'+Object.prototype.hasOwnProperty.call(a,1);\
            })()",
            "3,,1|false",
        ),
        // The hole moves up with everything else.
        (
            "(function(){\
                const a=[,2];\
                a.unshift(0);\
                return a.join()+'|'+Object.prototype.hasOwnProperty.call(a,1);\
            })()",
            "0,,2|false",
        ),
        // A missing source removes an existing destination rather than writing
        // `undefined`; the present source still moves normally.
        (
            "(function(){\
                const a=[,1,,3];\
                a.copyWithin(1,0,3);\
                return a.join()+'|'\
                    +Object.prototype.hasOwnProperty.call(a,0)+'|'\
                    +Object.prototype.hasOwnProperty.call(a,1)+'|'\
                    +Object.prototype.hasOwnProperty.call(a,2)+'|'\
                    +Object.prototype.hasOwnProperty.call(a,3);\
            })()",
            ",,1,|false|false|true|false",
        ),
    ]);
}

/// The mutators accept any array-like receiver.
#[test]
fn the_mutators_accept_an_array_like_receiver() {
    assert_all(&[
        (
            "(function(){\
                const o={length:1,0:'a'};\
                Array.prototype.push.call(o,'b');\
                return o.length+'|'+o[1];\
            })()",
            "2|b",
        ),
        (
            "(function(){\
                const o={length:2,0:'a',1:'b'};\
                const r=Array.prototype.pop.call(o);\
                return r+'|'+o.length;\
            })()",
            "b|1",
        ),
        // A length that needs coercion is converted once with `ToLength`.
        (
            "(function(){\
                const o={length:'2',1:'b'};\
                const r=Array.prototype.pop.call(o);\
                return r+'|'+o.length;\
            })()",
            "b|1",
        ),
        // Object-valued lengths perform a resumable `ToPrimitive` before
        // `ToLength` rather than being rejected by the native driver.
        (
            "(function(){\
                const o={length:{valueOf(){return 2;}},1:'b'};\
                const r=Array.prototype.pop.call(o);\
                return r+'|'+o.length;\
            })()",
            "b|1",
        ),
        // `push` with no arguments still writes the length back unchanged.
        (
            "(function(){const o={length:5};Array.prototype.push.call(o);return o.length;})()",
            "5",
        ),
        // A negative length clamps to zero, and `pop` writes that back.
        (
            "(function(){const o={length:-3};Array.prototype.pop.call(o);return o.length;})()",
            "0",
        ),
        // `JSON` is not in this profile, so the shape is read back directly.
        // Reversing an array-like moves `0` to `2` and leaves the absent middle
        // index absent.
        (
            "(function(){\
                const o={length:3,0:'a',2:'c'};\
                Array.prototype.reverse.call(o);\
                return o[0]+'|'+o[2]+'|'+Object.prototype.hasOwnProperty.call(o,1);\
            })()",
            "c|a|false",
        ),
        (
            "(function(){\
                const o={length:3};\
                Array.prototype.fill.call(o,7,1);\
                return String(o[0])+'|'+o[1]+'|'+o[2]+'|'+o.length;\
            })()",
            "undefined|7|7|3",
        ),
        (
            "(function(){\
                const o={length:4,0:'a',2:'c'};\
                const r=Array.prototype.copyWithin.call(o,1,0,3);\
                return (r===o)+'|'+o[1]+'|'\
                    +Object.prototype.hasOwnProperty.call(o,2)+'|'+o[3]+'|'+o.length;\
            })()",
            "true|a|false|c|4",
        ),
        // Integer indices above the Array-index domain are ordinary String
        // keys and remain reachable under `LengthOfArrayLike`.
        (
            "(function(){\
                const o={length:4294967296,0:'head','4294967295':'tail'};\
                Array.prototype.copyWithin.call(o,0,4294967295);\
                Array.prototype.copyWithin.call(o,4294967295,0,1);\
                return o[0]+'|'+o['4294967295']+'|'+o.length;\
            })()",
            "tail|tail|4294967296",
        ),
    ]);
}

/// The observable order is length read, element steps, then length write.
#[test]
fn the_observable_order_matches_the_oracle() {
    assert_all(&[
        // `push` reads the length, writes the element, then writes the length.
        (
            "(function(){\
                let log='';\
                const o={get length(){log+='getlen|';return 1;},set length(v){log+='setlen:'+v;}};\
                Object.defineProperty(o,1,{set(v){log+='set1:'+v+'|';},configurable:true});\
                Array.prototype.push.call(o,'x');\
                return log;\
            })()",
            "getlen|set1:x|setlen:2",
        ),
        // `pop` reads the length, reads the element, then shrinks the length.
        (
            "(function(){\
                let log='';\
                const o={get length(){log+='getlen|';return 2;},set length(v){log+='setlen:'+v;}};\
                Object.defineProperty(o,1,{get(){log+='get1|';return 'v';},configurable:true});\
                Array.prototype.pop.call(o);\
                return log;\
            })()",
            "getlen|get1|setlen:1",
        ),
        // `fill` converts its bounds after the length and before any element.
        (
            "(function(){\
                let log='';\
                const o={get length(){log+='len|';return 2;}};\
                Array.prototype.fill.call(\
                    o,\
                    {valueOf(){log+='val|';return 1;}},\
                    {valueOf(){log+='start|';return 0;}},\
                    {valueOf(){log+='end';return 2;}});\
                return log;\
            })()",
            "len|start|end",
        ),
        // `fill`'s value is stored without conversion, so its `valueOf` never
        // runs and the stored element stays an object.
        (
            "(function(){\
                let n=0;\
                const a=[1,2];\
                a.fill({valueOf(){n=n+1;return 9;}});\
                return n+'|'+typeof a[0];\
            })()",
            "0|object",
        ),
        // Length, target, start, and end are converted in that order. The
        // overlapping element range is then copied from the end.
        (
            "(function(){\
                let log='';\
                const o={\
                    get length(){log+='length|';return {valueOf(){log+='lengthValue|';return 3;}};},\
                    get 0(){log+='get0|';return 'x';},\
                    set 1(v){log+='set1:'+v;},\
                    get 1(){log+='get1|';return 'y';},\
                    set 2(v){log+='set2:'+v+'|';}\
                };\
                Array.prototype.copyWithin.call(\
                    o,\
                    {valueOf(){log+='target|';return 1;}},\
                    {valueOf(){log+='start|';return 0;}},\
                    {valueOf(){log+='end|';return 2;}});\
                return log;\
            })()",
            "length|lengthValue|target|start|end|get1|set2:y|get0|set1:x",
        ),
        // `copyWithin` never writes `length` back.
        (
            "(function(){\
                let log='';\
                const o={get length(){log+='get';return 1;},set length(v){log+='set';}};\
                Array.prototype.copyWithin.call(o,0,0);\
                return log;\
            })()",
            "get",
        ),
    ]);
}

/// Deleting an absent source's destination is the throwing abstract operation.
#[test]
fn copy_within_throws_when_a_destination_cannot_be_deleted() {
    assert_throws(
        "const o={length:2};\
         Object.defineProperty(o,1,{value:'locked',configurable:false,writable:true});\
         return Array.prototype.copyWithin.call(o,1,0,1);",
        ExceptionKind::TypeError,
        "could not delete property",
    );
}

/// Growing past the maximum length reports upstream's misspelled message.
///
/// The typo is upstream's (`quickjs.c:41933`) and the message is observable, so
/// it is reproduced rather than corrected.
#[test]
fn growing_past_the_maximum_length_is_rejected() {
    assert_throws(
        "const o={length:9007199254740991};return Array.prototype.push.call(o,1);",
        ExceptionKind::TypeError,
        "Array loo long",
    );
    assert_throws(
        "const o={length:9007199254740990};return Array.prototype.unshift.call(o,1,2);",
        ExceptionKind::TypeError,
        "Array loo long",
    );
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_receiver_is_rejected() {
    for method in [
        "push",
        "pop",
        "shift",
        "unshift",
        "reverse",
        "fill",
        "copyWithin",
    ] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver});"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}

/// Generic mutators preserve the full Proxy internal-method sequence instead
/// of reading or writing the handler object as ordinary storage.
#[test]
fn reverse_uses_proxy_internal_methods() {
    assert_all(&[(
        "(function(){\
            let log='';const target=[1,2];\
            const proxy=new Proxy(target,{\
                get:function(t,k){log+='g'+k+';';return t[k];},\
                has:function(t,k){log+='h'+k+';';return k in t;},\
                set:function(t,k,v){log+='s'+k+'='+v+';';t[k]=v;return true;},\
                deleteProperty:function(t,k){log+='d'+k+';';return delete t[k];}\
            });\
            Array.prototype.reverse.call(proxy);\
            return log+'|'+target.join();\
        })()",
        "glength;h0;g0;h1;g1;s1=1;s0=2;|2,1",
    )]);
}

/// The installed mutators carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_mutators_have_the_pinned_shape() {
    assert_all(&[
        // `push`, `unshift`, and `fill` report 1; the rest report 0.
        ("Array.prototype.push.length", "1"),
        ("Array.prototype.unshift.length", "1"),
        ("Array.prototype.fill.length", "1"),
        ("Array.prototype.pop.length", "0"),
        ("Array.prototype.shift.length", "0"),
        ("Array.prototype.reverse.length", "0"),
        ("Array.prototype.copyWithin.length", "2"),
        ("Array.prototype.push.name", "push"),
        ("Array.prototype.fill.name", "fill"),
        ("Array.prototype.reverse.name", "reverse"),
        ("Array.prototype.copyWithin.name", "copyWithin"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'push').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'push').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'push').configurable",
            "true",
        ),
        (
            "Object.prototype.hasOwnProperty.call(Array.prototype.copyWithin,'prototype')",
            "false",
        ),
        (
            "(function(){try{new Array.prototype.copyWithin();}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

/// A long `copyWithin` scan is lazy and consumes the shared instruction fuel.
#[test]
fn copy_within_scans_consume_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "return Array.prototype.copyWithin.call({length:1000},1,0,999);",
    );
    let result = context.call(
        &run,
        &[],
        ExecutionLimits::default().with_instruction_fuel(100),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}
