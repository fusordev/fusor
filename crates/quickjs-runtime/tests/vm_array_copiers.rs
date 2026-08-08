//! `Array.prototype.slice`, `concat`, `at`, `toReversed`, `toSpliced`, and
//! `with`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const o={length:2,0:"a"};\
//!     const r=[1].concat(o); console.log(r.length, r[1]===o);'
//! 2 true
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [1,2,3].slice(1) => "2,3"      slice(1,2) => "2"     slice(-2) => "2,3"
//! [1,2,3].slice(0,-1) => "1,2"   slice(2,1).length => 0
//! [1,,3].slice(0) => length 3, index 1 absent
//! Array.prototype.slice.call("abc",1) => "b,c"
//! [1,2,3].at(-1) => 3            at(3) => undefined    at(-4) => undefined
//! [1,2,3].at() => 1              at(1.9) => 2          [1,,3].at(1) => undefined
//! [1,2].concat([3,4]) => "1,2,3,4"
//! [1].concat([[2]]) => length 2, index 1 is an Array
//! [1].concat({length:2,0:"a"}) => length 2, index 1 is the object itself
//! Array.prototype.concat.call({length:2,0:"a"},9) => Array, length 2
//! slice reads length before any element: "len|g0|g1"
//! lengths: slice 2, concat 1, at 1
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
                    Arc::from("<runtime Array copiers>"),
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

/// `slice` copies a resolved range into a fresh Array.
#[test]
fn slice_copies_a_resolved_range() {
    assert_all(&[
        ("[1,2,3].slice(1).join()", "2,3"),
        ("[1,2,3].slice(1,2).join()", "2"),
        // Negative endpoints count from the end.
        ("[1,2,3].slice(-2).join()", "2,3"),
        ("[1,2,3].slice(0,-1).join()", "1,2"),
        // Crossed endpoints yield an empty Array rather than swapping.
        ("[1,2,3].slice(2,1).length", "0"),
        ("[1,2,3].slice().join()", "1,2,3"),
        ("[1,2,3].slice(-99,99).join()", "1,2,3"),
        // An absent or `undefined` end runs to the length.
        ("[1,2,3].slice(1,undefined).join()", "2,3"),
        // The result is a real Array, not an array-like.
        ("Array.isArray([1].slice())", "true"),
    ]);
}

/// `slice` performs `ArraySpeciesCreate` with the resolved count before it
/// reads an indexed source property. Its custom result is returned as-is: in
/// particular, `slice` does not finish by assigning a `length` property.
#[test]
fn slice_honors_species_construction_before_copying() {
    assert_all(&[(
        "(function(){\
            let log='';\
            const target={};\
            Object.defineProperty(target,'length',{set:function(){log+='set|';}});\
            function Species(length){log+='ctor:'+length+'|';return target;}\
            const source=[1];\
            source.constructor={};\
            source.constructor[Symbol.species]=Species;\
            Object.defineProperty(source,'0',{get:function(){log+='get|';return 1;}});\
            const result=source.slice(0,1);\
            return log+(result===target)+'|'+result[0];\
        })()",
        "ctor:1|get|true|1",
    )]);

    let (kind, _) = thrown(
        "(function(){\
            function Species(){Object.preventExtensions(this);}\
            const source=[1];\
            source.constructor={};\
            source.constructor[Symbol.species]=Species;\
            return source.slice();\
        })()",
    );
    assert_eq!(kind, ExceptionKind::TypeError);
}

/// `ArraySpeciesCreate(O, count)` rejects a count outside the Array length
/// domain before `slice` can start a potentially unbounded indexed loop.
#[test]
fn slice_rejects_an_impossible_result_length_before_index_reads() {
    let (kind, _) = thrown(
        "return Array.prototype.slice.call({length:4294967296,get 0(){throw new Error('read');}});",
    );
    assert_eq!(kind, ExceptionKind::RangeError);
}

/// `at` answers a single element and accepts a negative index.
#[test]
fn at_answers_one_element() {
    assert_all(&[
        ("[1,2,3].at(0)", "1"),
        ("[1,2,3].at(-1)", "3"),
        ("[1,2,3].at(1.9)", "2"),
        // An absent index is `0`.
        ("[1,2,3].at()", "1"),
        // Out of range answers `undefined` rather than throwing.
        ("String([1,2,3].at(3))", "undefined"),
        ("String([1,2,3].at(-4))", "undefined"),
        ("String([].at(0))", "undefined"),
        // A hole reads as `undefined`.
        ("String([1,,3].at(1))", "undefined"),
    ]);
}

/// `concat` honors `@@isConcatSpreadable` before falling back to `IsArray`.
///
/// An array-like becomes a single element, and nesting is not flattened, so the
/// two cases below differ even though both arguments are objects.
#[test]
fn concat_uses_is_concat_spreadable_then_is_array() {
    assert_all(&[
        ("[1,2].concat([3,4]).join()", "1,2,3,4"),
        ("[1,2].concat().join()", "1,2"),
        ("Array.isArray([1].concat())", "true"),
        // A nested Array is appended whole, not flattened.
        (
            "(function(){const r=[1].concat([[2]]);return r.length+'|'+Array.isArray(r[1]);})()",
            "2|true",
        ),
        (
            "(function(){const r=[1].concat(2,[3,[4]]);return r.length+'|'+r[3].length;})()",
            "4|1",
        ),
        // An array-like is one element, and it is the same object.
        (
            "(function(){\
                const o={length:2,0:'a'};\
                const r=[1].concat(o);\
                return r.length+'|'+(r[1]===o);\
            })()",
            "2|true",
        ),
        // An array-like receiver is spread, because the receiver is always
        // treated as the first source.
        (
            "(function(){\
                const r=Array.prototype.concat.call({length:2,0:'a'},9);\
                return Array.isArray(r)+'|'+r.length+'|'+(typeof r[0]);\
            })()",
            "true|2|object",
        ),
        (
            "(function(){let log='';const proxy=new Proxy([2,3],{\
                has(target,key){log+='h'+key+',';return Reflect.has(target,key);},\
                get(target,key,receiver){log+='g'+(typeof key==='symbol'?'@':key)+',';\
                  return Reflect.get(target,key,receiver);}\
              });const result=[1].concat(proxy);return result.join()+'|'+log;})()",
            "1,2,3|g@,glength,h0,g0,h1,g1,",
        ),
        (
            "(function(){const value={0:'a',1:'b',length:2};\
              value[Symbol.isConcatSpreadable]=true;return [1].concat(value).join();})()",
            "1,a,b",
        ),
        (
            "(function(){const value=[2,3];value[Symbol.isConcatSpreadable]=false;\
              const result=[1].concat(value);return result.length+'|'+(result[1]===value);})()",
            "2|true",
        ),
    ]);
}

/// `concat` validates a spread source's complete length before probing its
/// first indexed property, as the result cannot exceed `2^53 - 1`.
#[test]
fn concat_rejects_an_impossible_spread_length_before_index_reads() {
    let (kind, _) = thrown(
        "(function(){\
            const source={length:Number.MAX_SAFE_INTEGER};\
            source[Symbol.isConcatSpreadable]=true;\
            Object.defineProperty(source,'0',{get:function(){throw new Error('read');}});\
            return [1].concat(source);\
        })()",
    );
    assert_eq!(kind, ExceptionKind::TypeError);
}

/// `concat` obtains its `ArraySpeciesCreate` destination before observing the
/// first source's `@@isConcatSpreadable` property.
#[test]
fn concat_honors_species_construction_before_spreadability() {
    assert_all(&[
        (
            "(function(){\
                let order='';\
                const source=[];\
                Object.defineProperty(source,'constructor',{get:function(){\
                    order+='constructor|';return Array;}});\
                Object.defineProperty(source,Symbol.isConcatSpreadable,{get:function(){\
                    order+='spread|';return true;}});\
                source.concat();return order;\
            })()",
            "constructor|spread|",
        ),
        (
            "(function(){\
                let calls='';\
                function Species(length){calls+='ctor:'+length+'|';}\
                const source=[];\
                source.constructor={};\
                source.constructor[Symbol.species]=Species;\
                const result=source.concat(1);\
                return calls+(result instanceof Species)+'|'+result.length+'|'+result[0];\
            })()",
            "ctor:0|true|1|1",
        ),
    ]);

    let (kind, _) = thrown(
        "(function(){\
            function Species(){Object.preventExtensions(this);}\
            const source=[];\
            source.constructor={};\
            source.constructor[Symbol.species]=Species;\
            return source.concat(1);\
        })()",
    );
    assert_eq!(kind, ExceptionKind::TypeError);
}

/// Holes survive into the copied result.
///
/// An absent source index is skipped rather than written, so the destination
/// keeps a hole and still counts it toward the length.
#[test]
fn holes_survive_into_the_result() {
    assert_all(&[
        (
            "(function(){\
                const r=[1,,3].slice(0);\
                return r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);\
            })()",
            "3|false",
        ),
        (
            "(function(){\
                const r=[1,,3].concat([4]);\
                return r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);\
            })()",
            "4|false",
        ),
    ]);
}

/// The copiers accept any array-like or primitive-String receiver.
#[test]
fn the_copiers_accept_an_array_like_receiver() {
    assert_all(&[
        (
            "(function(){\
                const r=Array.prototype.slice.call({length:2,0:'a',1:'b'});\
                return Array.isArray(r)+'|'+r.join();\
            })()",
            "true|a,b",
        ),
        // A primitive String exposes its indices, so it slices by character.
        ("Array.prototype.slice.call('abc',1).join()", "b,c"),
    ]);
}

/// The length is read once, before any element.
#[test]
fn the_length_is_read_before_any_element() {
    assert_all(&[(
        "(function(){\
            let log='';\
            const o={\
                get length(){log+='len|';return 2;},\
                get 0(){log+='g0|';return 1;},\
                get 1(){log+='g1';return 2;}\
            };\
            Array.prototype.slice.call(o);\
            return log;\
        })()",
        "len|g0|g1",
    )]);
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_receiver_is_rejected_by_the_copiers() {
    for method in ["slice", "concat", "at"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver});"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}

/// The installed copiers carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_copiers_have_the_pinned_shape() {
    assert_all(&[
        // Only `slice` reports arity 2.
        ("Array.prototype.slice.length", "2"),
        ("Array.prototype.concat.length", "1"),
        ("Array.prototype.at.length", "1"),
        ("Array.prototype.slice.name", "slice"),
        ("Array.prototype.concat.name", "concat"),
        ("Array.prototype.at.name", "at"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'slice').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'slice').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'slice').configurable",
            "true",
        ),
    ]);
}

/// `toReversed` reads from the end into a fresh ordinary Array.
#[test]
fn to_reversed_copies_in_descending_source_order() {
    assert_all(&[
        ("[1,2,3].toReversed().join()", "3,2,1"),
        ("[].toReversed().length", "0"),
        ("Array.isArray([1].toReversed())", "true"),
        ("Array.prototype.toReversed.call('abc').join('')", "cba"),
        (
            "Array.prototype.toReversed.call({length:3,0:'a',2:'c'}).join()",
            "c,,a",
        ),
        // The source remains untouched.
        (
            "(function(){const a=[1,2];const r=a.toReversed();return a.join()+'|'+r.join();})()",
            "1,2|2,1",
        ),
    ]);
}

/// Change-by-copy methods use `Get`, so holes become own `undefined` values.
#[test]
fn change_by_copy_methods_read_through_holes() {
    assert_all(&[
        (
            "(function(){const r=[1,,3].toReversed();const own=Object.prototype.hasOwnProperty;return r.length+'|'+own.call(r,0)+'|'+own.call(r,1)+'|'+own.call(r,2)+'|'+r.join();})()",
            "3|true|true|true|3,,1",
        ),
        (
            "(function(){const r=[,,].with(0,7);const own=Object.prototype.hasOwnProperty;return r.length+'|'+own.call(r,0)+'|'+own.call(r,1)+'|'+r.join();})()",
            "2|true|true|7,",
        ),
        // Inherited indexed values are observed by ordinary `Get`.
        (
            "(function(){const p={1:'p'};const o=Object.create(p);o.length=3;o[0]='a';o[2]='c';return Array.prototype.toReversed.call(o).join();})()",
            "c,p,a",
        ),
    ]);
}

/// `with` validates one relative index and replaces only that output slot.
#[test]
fn with_replaces_one_relative_index_without_mutating_the_source() {
    assert_all(&[
        ("[1,2,3].with(1,9).join()", "1,9,3"),
        ("[1,2,3].with(-1,9).join()", "1,2,9"),
        ("[1,2,3].with(1.9,9).join()", "1,9,3"),
        ("[1,2,3].with(undefined,9).join()", "9,2,3"),
        (
            "Array.prototype.with.call({length:3,0:'a',2:'c'},-2,'b').join()",
            "a,b,c",
        ),
        (
            "(function(){const a=[1,2];const r=a.with(0,9);return a.join()+'|'+r.join();})()",
            "1,2|9,2",
        ),
        // The replacement value is stored without coercion.
        (
            "(function(){const v={};return [1].with(0,v)[0]===v;})()",
            "true",
        ),
    ]);
}

/// Observable conversions and element reads follow the algorithm's order.
#[test]
fn change_by_copy_conversion_and_getter_order_is_exact() {
    assert_all(&[
        (
            "(function(){let log='';const o={get length(){log+='l';return {valueOf(){log+='v';return 3;}}},get 2(){log+='2';return 'c'},get 1(){log+='1';return 'b'},get 0(){log+='0';return 'a'}};const r=Array.prototype.toReversed.call(o);return log+'|'+r.join('');})()",
            "lv210|cba",
        ),
        (
            "(function(){let log='';const o={get length(){log+='l';return 2},get 0(){log+='0';return 'a'},get 1(){log+='1';return 'b'}};const i={valueOf(){log+='i';return 1}};const r=Array.prototype.with.call(o,i,'x');return log+'|'+r.join('');})()",
            "li0|ax",
        ),
        // An out-of-range index throws before any element is read.
        (
            "(function(){let read=false;const o={length:1,get 0(){read=true;return 1}};try{Array.prototype.with.call(o,1,9);}catch(error){return (error instanceof RangeError)+'|'+read;}})()",
            "true|false",
        ),
        // Getter abrupt completions propagate without touching later indices.
        (
            "(function(){let later=false;const o={length:2,get 1(){throw 41},get 0(){later=true;return 0}};try{Array.prototype.toReversed.call(o);}catch(error){return (error===41)+'|'+later;}})()",
            "true|false",
        ),
    ]);
}

/// Invalid indices and `ArrayCreate` lengths produce realm-owned range errors.
#[test]
fn change_by_copy_preconditions_throw_range_error() {
    for body in [
        "return [1,2].with(2,9);",
        "return [1,2].with(-3,9);",
        "return [1,2].with(Infinity,9);",
        "return [].with(0,9);",
    ] {
        assert_throws(body, ExceptionKind::RangeError, "invalid array index");
    }
    for body in [
        "return Array.prototype.toReversed.call({length:4294967296});",
        "return Array.prototype.with.call({length:4294967296},0,1);",
    ] {
        assert_throws(body, ExceptionKind::RangeError, "invalid array length");
    }
}

/// The new methods are ordinary non-constructors with the pinned identities.
#[test]
fn change_by_copy_methods_have_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.toReversed.name", "toReversed"),
        ("Array.prototype.toReversed.length", "0"),
        ("Array.prototype.with.name", "with"),
        ("Array.prototype.with.length", "2"),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(Array.prototype,'toReversed');return d.writable+','+d.enumerable+','+d.configurable;})()",
            "true,false,true",
        ),
        (
            "(function(){try{new Array.prototype.toReversed();}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
        (
            "(function(){try{new Array.prototype.with(0,1);}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
    for method in ["toReversed", "with"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver});"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}

/// A long change-by-copy scan consumes shared instruction fuel.
#[test]
fn change_by_copy_scans_consume_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "return Array.prototype.toReversed.call({length:1000});",
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

/// `toSpliced` copies a prefix, inserts its items, and then copies the suffix.
#[test]
fn to_spliced_builds_a_fresh_changed_array() {
    assert_all(&[
        ("[1,2,3,4].toSpliced(1,2,'a','b').join()", "1,a,b,4"),
        ("[1,2,3,4].toSpliced(-2,1,'x').join()", "1,2,x,4"),
        ("[1,2,3].toSpliced(Infinity,1,'x').join()", "1,2,3,x"),
        ("[1,2,3].toSpliced(-Infinity,1,'x').join()", "x,2,3"),
        (
            "Array.prototype.toSpliced.call({length:3,0:'a',2:'c'},1,1,'b').join()",
            "a,b,c",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.toSpliced(1,1,9);return a.join()+'|'+r.join()+'|'+Array.isArray(r);})()",
            "1,2,3|1,9,3|true",
        ),
    ]);
}

/// Argument presence controls `toSpliced` independently from coercion value.
#[test]
fn to_spliced_distinguishes_absent_and_undefined_arguments() {
    assert_all(&[
        // With no start argument, the skip count is zero and the source copies.
        ("[1,2,3].toSpliced().join()", "1,2,3"),
        // A present start with no skip count removes the remaining suffix.
        ("[1,2,3].toSpliced(1).join()", "1"),
        // Present undefined start converts to zero, then the absent skip count
        // removes everything.
        ("[1,2,3].toSpliced(undefined).join()", ""),
        // Present undefined skip count converts to zero instead of removing the
        // suffix.
        ("[1,2,3].toSpliced(1,undefined,'x').join()", "1,x,2,3"),
        ("[1,2,3].toSpliced(1,-4,'x').join()", "1,x,2,3"),
        ("[1,2,3].toSpliced(1,99,'x').join()", "1,x"),
    ]);
}

/// `toSpliced` reads through holes and inherited indexed properties.
#[test]
fn to_spliced_materializes_every_copied_index() {
    assert_all(&[
        (
            "(function(){const r=[1,,3].toSpliced(1,0);const own=Object.prototype.hasOwnProperty;return r.length+'|'+own.call(r,0)+'|'+own.call(r,1)+'|'+own.call(r,2)+'|'+r.join();})()",
            "3|true|true|true|1,,3",
        ),
        (
            "(function(){const p={1:'p'};const o=Object.create(p);o.length=3;o[0]='a';o[2]='c';return Array.prototype.toSpliced.call(o,1,0,'x').join();})()",
            "a,x,p,c",
        ),
    ]);
}

/// Start and skip conversions finish before any source index is observed.
#[test]
fn to_spliced_conversion_and_getter_order_is_exact() {
    assert_all(&[
        (
            "(function(){let log='';const o={get length(){log+='l';return 3},get 0(){log+='0';return'a'},get 1(){log+='1';return'b'},get 2(){log+='2';return'c'}};const s={valueOf(){log+='s';return 1}},d={valueOf(){log+='d';return 1}};const r=Array.prototype.toSpliced.call(o,s,d,'x');return log+'|'+r.join();})()",
            "lsd02|a,x,c",
        ),
        (
            "(function(){let read=false;const o={length:2,get 0(){read=true;return 1}};try{Array.prototype.toSpliced.call(o,{valueOf(){throw 41}},0);}catch(error){return (error===41)+'|'+read;}})()",
            "true|false",
        ),
        (
            "(function(){let read=false;const o={length:2,get 0(){read=true;return 1}};try{Array.prototype.toSpliced.call(o,0,{valueOf(){throw 42}});}catch(error){return (error===42)+'|'+read;}})()",
            "true|false",
        ),
        (
            "(function(){let later=false;const o={length:2,get 0(){throw 43},get 1(){later=true;return 2}};try{Array.prototype.toSpliced.call(o,1,0);}catch(error){return (error===43)+'|'+later;}})()",
            "true|false",
        ),
    ]);
}

/// Result length validation precedes copying and supports large source keys.
#[test]
fn to_spliced_validates_only_the_result_array_length() {
    assert_throws(
        "return Array.prototype.toSpliced.call({length:9007199254740991},0,0,1);",
        ExceptionKind::TypeError,
        "invalid array length",
    );
    assert_throws(
        "return Array.prototype.toSpliced.call({length:4294967296});",
        ExceptionKind::RangeError,
        "invalid array length",
    );
    assert_all(&[
        (
            "Array.prototype.toSpliced.call({length:4294967296},0,4294967296).length",
            "0",
        ),
        (
            "Array.prototype.toSpliced.call({length:4294967296,'4294967295':'tail'},0,4294967295)[0]",
            "tail",
        ),
    ]);
}

/// `toSpliced` is an ordinary non-constructor with the pinned shape.
#[test]
fn to_spliced_has_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.toSpliced.name", "toSpliced"),
        ("Array.prototype.toSpliced.length", "2"),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(Array.prototype,'toSpliced');return d.writable+','+d.enumerable+','+d.configurable;})()",
            "true,false,true",
        ),
        (
            "(function(){try{new Array.prototype.toSpliced();}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
    for receiver in ["null", "undefined"] {
        assert_throws(
            &format!("return Array.prototype.toSpliced.call({receiver});"),
            ExceptionKind::TypeError,
            "cannot convert to object",
        );
    }
}
