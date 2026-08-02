//! `Object.defineProperty` and `Object.getOwnPropertyDescriptor`, pinned to the
//! `QuickJS` 2026-06-04 oracle.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! returns target => [true]
//! defaults => [data v=1 w=false e=false c=false]
//! all flags => [data v=1 w=true e=true c=true]
//! accessor => [acc g=function s=undefined e=false c=false read=7]
//! setter only => [acc g=undefined s=function e=false c=false seen=5]
//! nonconfig reject !! TypeError: property is not configurable
//! nonconfig same value => [data v=1 w=false e=false c=false]
//! writable change ok => [data v=2 w=true e=false c=false]
//! mixed desc !! TypeError: cannot have setter/getter and value or writable
//! bad getter !! TypeError: invalid getter
//! bad setter !! TypeError: invalid setter
//! primitive target !! TypeError: not an object
//! null target !! TypeError: not an object
//! nonobject desc !! TypeError: not an object
//! field read order => [ecvw]
//! key then value order => [kv]
//! nonextensible !! TypeError: object is not extensible
//! nonext redefine existing => [data v=2 w=false e=false c=true]
//! symbol key => [9]
//! array index => [len=6 a5=9]
//! gopd absent => [undefined]
//! gopd inherited => [undefined]
//! gopd string index => [data v=a w=false e=true c=false]
//! gopd string length => [data v=2 w=false e=false c=false]
//! gopd array length => [data v=2 w=true e=false c=false]
//! gopd primitive num => [undefined]
//! gopd null !! TypeError: cannot convert to object
//! gopd result independent => [1|1]
//! gopd value field desc => [data v=1 w=true e=true c=true]
//! defineProperty length => [3]
//! gopd length => [2]
//! accessor keeps other half => [getterKept=true hasSet=true]
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

/// Renders one own property descriptor as a comparable string.
///
/// Every fixture is prefixed with this so the assertions read as the shape of
/// the descriptor rather than as a sequence of field reads.
const DESCRIBE: &str = concat!(
    "function descriptor(o,k){",
    "var x=Object.getOwnPropertyDescriptor(o,k);",
    "if(!x){return 'absent';}",
    "if(typeof x.get==='undefined'&&typeof x.set==='undefined'){",
    "return 'data v='+x.value+' w='+x.writable+' e='+x.enumerable+' c='+x.configurable;}",
    "return 'acc g='+(typeof x.get)+' s='+(typeof x.set)",
    "+' e='+x.enumerable+' c='+x.configurable;}"
);

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
                    Arc::from("<runtime descriptor>"),
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

/// Evaluates `body` with the descriptor helper in scope.
fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let source = [DESCRIBE, body].concat();
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &source);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
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

fn boolean(body: &str) -> bool {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_boolean()
            .expect("live value")
            .expect("Boolean")
    })
}

fn assert_throws(body: &str, kind: ExceptionKind, message: &str) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        assert_eq!(exception.kind(), Some(kind), "{body}");
        assert_eq!(
            exception
                .message()
                .expect("engine message")
                .to_utf8_lossy()
                .expect("UTF-8"),
            message,
            "{body}"
        );
    });
}

/// Oracle: `returns target => [true]`, `defaults =>
/// [data v=1 w=false e=false c=false]`, and `all flags =>
/// [data v=1 w=true e=true c=true]`.
///
/// An absent attribute defaults to `false`, so a bare `{value: 1}` produces a
/// fully locked-down property.
#[test]
fn define_property_returns_its_target_and_defaults_attributes_to_false() {
    assert!(boolean(
        "var o={};return Object.defineProperty(o,'x',{value:1})===o;"
    ));
    assert_eq!(
        text("var o={};Object.defineProperty(o,'x',{value:1});return descriptor(o,'x');"),
        "data v=1 w=false e=false c=false"
    );
    assert_eq!(
        text(
            "var o={};\
             Object.defineProperty(o,'x',\
             {value:1,writable:true,enumerable:true,configurable:true});\
             return descriptor(o,'x');"
        ),
        "data v=1 w=true e=true c=true"
    );
}

/// Oracle: `accessor => [acc g=function s=undefined e=false c=false read=7]` and
/// `setter only => [acc g=undefined s=function e=false c=false seen=5]`.
#[test]
fn define_property_installs_accessors_that_run() {
    assert_eq!(
        text(
            "var o={};\
             Object.defineProperty(o,'x',{get(){return 7;}});\
             return descriptor(o,'x')+' read='+o.x;"
        ),
        "acc g=function s=undefined e=false c=false read=7"
    );
    assert_eq!(
        text(
            "var o={};var seen;\
             Object.defineProperty(o,'x',{set(v){seen=v;}});\
             o.x=5;\
             return descriptor(o,'x')+' seen='+seen;"
        ),
        "acc g=undefined s=function e=false c=false seen=5"
    );
}

/// Oracle: `nonconfig reject !! TypeError: property is not configurable`,
/// `nonconfig same value` succeeding, and `writable change ok =>
/// [data v=2 w=true e=false c=false]`.
///
/// This is the descriptor authority becoming script-reachable: the rules the
/// object-literal path already used now govern an explicit definition too.
#[test]
fn define_property_enforces_the_descriptor_compatibility_rules() {
    assert_throws(
        "var o={};Object.defineProperty(o,'x',{value:1});\
         return Object.defineProperty(o,'x',{value:2});",
        ExceptionKind::TypeError,
        "property is not configurable",
    );
    // A `SameValue` rewrite of a frozen property is a no-op rather than a throw.
    assert_eq!(
        text(
            "var o={};Object.defineProperty(o,'x',{value:1});\
             Object.defineProperty(o,'x',{value:1});\
             return descriptor(o,'x');"
        ),
        "data v=1 w=false e=false c=false"
    );
    assert_eq!(
        text(
            "var o={};Object.defineProperty(o,'x',{value:1,writable:true});\
             Object.defineProperty(o,'x',{value:2});\
             return descriptor(o,'x');"
        ),
        "data v=2 w=true e=false c=false"
    );
}

/// Oracle: `mixed desc`, `bad getter`, and `bad setter` each report their own
/// pinned message.
#[test]
fn define_property_validates_the_descriptor_shape() {
    assert_throws(
        "return Object.defineProperty({},'x',{value:1,get(){return 2;}});",
        ExceptionKind::TypeError,
        "cannot have setter/getter and value or writable",
    );
    assert_throws(
        "return Object.defineProperty({},'x',{get:5});",
        ExceptionKind::TypeError,
        "invalid getter",
    );
    assert_throws(
        "return Object.defineProperty({},'x',{set:5});",
        ExceptionKind::TypeError,
        "invalid setter",
    );
}

/// Oracle: `primitive target`, `null target`, and `nonobject desc` all report
/// `TypeError: not an object`.
#[test]
fn define_property_requires_an_object_target_and_descriptor() {
    for source in [
        "return Object.defineProperty(5,'x',{value:1});",
        "return Object.defineProperty(null,'x',{value:1});",
        "return Object.defineProperty(undefined,'x',{value:1});",
        "return Object.defineProperty({},'x',5);",
    ] {
        assert_throws(source, ExceptionKind::TypeError, "not an object");
    }
}

/// Oracle: `field read order => [ecvw]`.
///
/// `ToPropertyDescriptor` fixes the order as `enumerable`, `configurable`,
/// `value`, `writable`, `get`, `set`, which a descriptor with side-effecting
/// accessors observes.
#[test]
fn define_property_reads_the_descriptor_fields_in_specification_order() {
    assert_eq!(
        text(
            "var log='';\
             var desc={\
             get enumerable(){log+='e';return true;},\
             get configurable(){log+='c';return true;},\
             get value(){log+='v';return 1;},\
             get writable(){log+='w';return true;}};\
             Object.defineProperty({},'x',desc);\
             return log;"
        ),
        "ecvw"
    );
}

/// An invalid accessor is rejected at its field, before later descriptor
/// fields are consulted.
#[test]
fn define_property_validates_accessors_during_descriptor_conversion() {
    assert_eq!(
        text(
            "var log='';var desc={\
             get get(){log+='g';return 1;},\
             get set(){log+='s';return undefined;}};\
             try{Object.defineProperty({},'x',desc);}catch(error){}return log;"
        ),
        "g"
    );
}

/// Oracle: `key then value order => [kv]`. The key is converted before any
/// descriptor field is read.
#[test]
fn define_property_converts_its_key_before_reading_the_descriptor() {
    assert_eq!(
        text(
            "var log='';\
             var key={toString(){log+='k';return 'p';}};\
             var desc={get value(){log+='v';return 1;}};\
             Object.defineProperty({},key,desc);\
             return log;"
        ),
        "kv"
    );
}

/// Oracle: `nonextensible !! TypeError: object is not extensible` and
/// `nonext redefine existing => [data v=2 w=false e=false c=true]`.
#[test]
fn define_property_rejects_a_new_property_on_a_non_extensible_object() {
    assert_throws(
        "var o=Object.preventExtensions({});\
         return Object.defineProperty(o,'x',{value:1});",
        ExceptionKind::TypeError,
        "object is not extensible",
    );
    // Redefining an existing configurable property still succeeds.
    assert_eq!(
        text(
            "var o={};Object.defineProperty(o,'x',{value:1,configurable:true});\
             Object.preventExtensions(o);\
             Object.defineProperty(o,'x',{value:2});\
             return descriptor(o,'x');"
        ),
        "data v=2 w=false e=false c=true"
    );
}

/// Oracle: `symbol key => [9]` and `array index => [len=6 a5=9]`.
#[test]
fn define_property_accepts_symbol_and_array_index_keys() {
    assert_eq!(
        text(
            "var s=Symbol('k');var o={};\
             Object.defineProperty(o,s,{value:9});\
             return String(o[s]);"
        ),
        "9"
    );
    // An array index extends the cached length.
    assert_eq!(
        text(
            "var a=[1,2];\
             Object.defineProperty(a,'5',\
             {value:9,enumerable:true,writable:true,configurable:true});\
             return 'len='+a.length+' a5='+a[5];"
        ),
        "len=6 a5=9"
    );
}

/// Oracle: `gopd absent => [undefined]` and `gopd inherited => [undefined]`.
#[test]
fn get_own_property_descriptor_reports_only_own_properties() {
    assert_eq!(
        text("return typeof Object.getOwnPropertyDescriptor({},'x');"),
        "undefined"
    );
    assert_eq!(
        text(
            "var base={x:1};var o={__proto__:base};\
             return typeof Object.getOwnPropertyDescriptor(o,'x');"
        ),
        "undefined"
    );
}

/// Oracle: `gopd string index => [data v=a w=false e=true c=false]`,
/// `gopd string length => [data v=2 w=false e=false c=false]`, and
/// `gopd array length => [data v=2 w=true e=false c=false]`.
#[test]
fn get_own_property_descriptor_reports_the_pinned_exotic_attributes() {
    assert_eq!(
        text("return descriptor('ab',0);"),
        "data v=a w=false e=true c=false"
    );
    assert_eq!(
        text("return descriptor('ab','length');"),
        "data v=2 w=false e=false c=false"
    );
    assert_eq!(
        text("return descriptor([1,2],'length');"),
        "data v=2 w=true e=false c=false"
    );
}

/// Oracle: `gopd primitive num => [undefined]` and
/// `gopd null !! TypeError: cannot convert to object`.
#[test]
fn get_own_property_descriptor_handles_primitive_targets() {
    assert_eq!(
        text("return typeof Object.getOwnPropertyDescriptor(5,'x');"),
        "undefined"
    );
    assert_throws(
        "return Object.getOwnPropertyDescriptor(null,'x');",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// Oracle: `gopd result independent => [1|1]` and
/// `gopd value field desc => [data v=1 w=true e=true c=true]`.
///
/// The result is a fresh ordinary object, so mutating it cannot disturb the
/// property it described.
#[test]
fn get_own_property_descriptor_returns_an_independent_mutable_object() {
    assert_eq!(
        text(
            "var o={x:1};\
             var first=Object.getOwnPropertyDescriptor(o,'x');\
             first.value=99;\
             return String(o.x)+'|'\
             +String(Object.getOwnPropertyDescriptor(o,'x').value);"
        ),
        "1|1"
    );
    assert_eq!(
        text(
            "var o={x:1};\
             var first=Object.getOwnPropertyDescriptor(o,'x');\
             return descriptor(first,'value');"
        ),
        "data v=1 w=true e=true c=true"
    );
}

/// Oracle: `defineProperty length => [3]` and `gopd length => [2]`.
#[test]
fn the_descriptor_statics_report_the_pinned_arities() {
    assert_eq!(text("return String(Object.defineProperty.length);"), "3");
    assert_eq!(
        text("return String(Object.getOwnPropertyDescriptor.length);"),
        "2"
    );
}

/// Oracle: `accessor keeps other half => [getterKept=true hasSet=true]`.
///
/// An absent accessor field keeps the current function rather than clearing it.
#[test]
fn redefining_one_accessor_half_keeps_the_other() {
    assert_eq!(
        text(
            "var o={};function g(){return 1;}\
             Object.defineProperty(o,'x',{get:g,configurable:true});\
             Object.defineProperty(o,'x',{set(v){}});\
             var x=Object.getOwnPropertyDescriptor(o,'x');\
             return 'getterKept='+(x.get===g)+' hasSet='+(typeof x.set==='function');"
        ),
        "getterKept=true hasSet=true"
    );
}

/// A round trip through both operations reproduces the descriptor exactly.
#[test]
fn a_descriptor_round_trip_preserves_every_attribute() {
    assert_eq!(
        text(
            "var source={};\
             Object.defineProperty(source,'x',\
             {value:7,writable:true,enumerable:false,configurable:true});\
             var copy={};\
             Object.defineProperty(copy,'x',\
             Object.getOwnPropertyDescriptor(source,'x'));\
             return descriptor(copy,'x');"
        ),
        "data v=7 w=true e=false c=true"
    );
}
