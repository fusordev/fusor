//! ES-first `RegExp` constructor, accessor, and execution semantics.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime,
    RuntimeLimits,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime RegExp>"))
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

fn evaluate<T>(
    body: &str,
    project: impl FnOnce(Result<quickjs_runtime::JsValue, ExecutionError>) -> T,
) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    project(context.call(&run, &[], ExecutionLimits::default()))
}

fn rendered(body: &str) -> String {
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

fn thrown(body: &str) -> (ExceptionKind, String) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected JavaScript exception");
        };
        (
            exception.kind().expect("engine exception kind"),
            exception
                .message()
                .expect("engine message")
                .to_utf8_lossy()
                .expect("UTF-8"),
        )
    })
}

#[test]
fn regexp_surface_matches_the_pinned_engine() {
    assert_eq!(
        rendered(
            "return Object.getOwnPropertyNames(RegExp).join(',')+'|'+\
             Object.getOwnPropertyNames(RegExp.prototype).join(',')+'|'+\
             RegExp.length+'|'+(RegExp[Symbol.species]===RegExp)+'|'+\
             (Object.getPrototypeOf(/a/)===RegExp.prototype);"
        ),
        "length,name,escape,prototype|flags,source,global,ignoreCase,multiline,dotAll,unicode,unicodeSets,sticky,hasIndices,exec,compile,test,toString,constructor|2|true|true"
    );
}

#[test]
fn regexp_constructor_preserves_identity_and_normative_observation_order() {
    assert_eq!(
        rendered(
            "var r=/a/g;var same=RegExp(r)===r;var copied=new RegExp(r)!==r;\
             var log=[];var pattern={\
               get [Symbol.match](){log.push('match');return true},\
               get source(){log.push('source');return {toString:function(){log.push('pstr');return 'a'}}},\
               get flags(){log.push('flags');return {toString:function(){log.push('fstr');return 'g'}}}\
             };\
             var Derived=(function(){}).bind(null);var derivedPrototype=Object.create(RegExp.prototype);\
             Object.defineProperty(Function.prototype,'prototype',{configurable:true,get:function(){log.push('prototype');return derivedPrototype}});\
             var made=Reflect.construct(RegExp,[pattern],Derived);\
             return [same,copied,log.join(','),made.source,made.flags,Object.getPrototypeOf(made)===derivedPrototype].join('|');"
        ),
        "true|true|match,source,flags,prototype,pstr,fstr|a|g|true"
    );
}

#[test]
fn regexp_accessors_preserve_original_source_and_canonical_flags() {
    assert_eq!(
        rendered(
            "var r=new RegExp('a/b\\n','umigd');\
             return [r.source,r.flags,r.hasIndices,r.global,r.ignoreCase,r.multiline,r.dotAll,r.unicode,r.unicodeSets,r.sticky,\
                     new RegExp('').source,RegExp.prototype.source,String(RegExp.prototype.global),RegExp.prototype.flags].join('|');"
        ),
        "a\\/b\\n|dgimu|true|true|true|true|false|true|false|false|(?:)|(?:)|undefined|"
    );
}

#[test]
fn regexp_builtin_exec_updates_last_index_and_materializes_captures() {
    assert_eq!(
        rendered(
            "var r=new RegExp('(?<letter>a)(b)?','dg');var first=r.exec('xab a');var afterFirst=r.lastIndex;\
             var second=r.exec('xab a');var afterSecond=r.lastIndex;var miss=r.exec('xab a');\
             return [first[0],first[1],first[2],first.index,first.input,first.groups.letter,\
                     first.indices[0].join(','),first.indices[1].join(','),first.indices[2].join(','),first.indices.groups.letter.join(','),\
                     afterFirst,second[0],String(second[2]),afterSecond,String(miss),r.lastIndex].join('|');"
        ),
        "ab|a|b|1|xab a|a|1,3|1,2|2,3|1,2|3|a|undefined|5|null|0"
    );
}

#[test]
fn regexp_exec_observes_last_index_for_every_receiver_and_enforces_sticky_writes() {
    assert_eq!(
        rendered(
            "var log=[];var plain=/a/;plain.lastIndex={valueOf:function(){log.push('lastIndex');return 99}};\
             var ordinary=plain.exec('a');var sticky=/a/y;sticky.lastIndex=1;var hit=sticky.exec('ba');\
             sticky.lastIndex=0;var miss=sticky.exec('ba');\
             return [log.join(','),ordinary.index,typeof plain.lastIndex,hit.index,sticky.lastIndex,String(miss)].join('|');"
        ),
        "lastIndex|0|object|1|0|null"
    );

    assert_eq!(
        thrown(
            "var r=/a/g;Object.defineProperty(r,'lastIndex',{writable:false});return r.exec('a');"
        )
        .0,
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "var r=/z/g;r.lastIndex=1;Object.defineProperty(r,'lastIndex',{writable:false});return r.exec('a');"
        )
        .0,
        ExceptionKind::TypeError
    );
}

#[test]
fn regexp_flags_getter_is_generic_and_uses_normative_canonical_order() {
    assert_eq!(
        rendered(
            "var receiver={};var log=[];\
             [['hasIndices','d'],['global','g'],['ignoreCase','i'],['multiline','m'],['dotAll','s'],\
              ['unicode','u'],['unicodeSets','v'],['sticky','y']].forEach(function(pair){\
                Object.defineProperty(receiver,pair[0],{get:function(){log.push(pair[1]);return true}})});\
             var getter=Object.getOwnPropertyDescriptor(RegExp.prototype,'flags').get;\
             return getter.call(receiver)+'|'+log.join('');"
        ),
        "dgimsuvy|dgimsuvy"
    );
}

#[test]
fn regexp_exec_brand_check_precedes_input_coercion_and_test_is_generic() {
    assert_eq!(
        rendered(
            "var touched=false;var input={toString:function(){touched=true;return 'x'}};var branded=false;\
             try{RegExp.prototype.exec.call({},input)}catch(error){branded=error instanceof TypeError}\
             var receiver={exec:function(value){return value==='needle'?{ok:true}:null}};\
             return [branded,touched,RegExp.prototype.test.call(receiver,'needle'),RegExp.prototype.test.call(receiver,'other')].join('|');"
        ),
        "true|false|true|false"
    );
    assert_eq!(
        thrown("return RegExp.prototype.test.call({exec:function(){return 1}},'x');").0,
        ExceptionKind::TypeError
    );
}

#[test]
fn regexp_invalid_source_and_flags_throw_syntax_errors_after_coercion() {
    assert_eq!(
        thrown("return new RegExp('(', 'g');").0,
        ExceptionKind::SyntaxError
    );
    assert_eq!(
        thrown("return new RegExp('a', 'gg');").0,
        ExceptionKind::SyntaxError
    );
}

#[test]
fn regexp_compile_reinitializes_only_branded_receivers() {
    assert_eq!(
        rendered(
            "var r=/x/g;r.lastIndex=7;var same=r.compile('a','i')===r;\
             var copied=r.compile(/b/g)===r;var log=[];\
             var raw={get [Symbol.match](){log.push('match');return true},toString:function(){log.push('string');return 'c'}};\
             r.compile(raw);var flagsError=false;try{r.compile(/d/g,'i')}catch(error){flagsError=error instanceof TypeError}\
             return [same,copied,r.source,r.flags,r.lastIndex,log.join(','),flagsError].join('|');"
        ),
        "true|true|c||0|string|true"
    );
    assert_eq!(
        thrown("return RegExp.prototype.compile.call({}, 'a');").0,
        ExceptionKind::TypeError
    );
}

#[test]
fn regexp_escape_follows_the_spec_scalar_and_punctuator_rules() {
    assert_eq!(
        rendered(
            "return [RegExp.escape('foo'),RegExp.escape('1a'),RegExp.escape('a-b'),RegExp.escape('a/b'),\
                     RegExp.escape('a b'),RegExp.escape('[x]'),RegExp.escape('é'),RegExp.escape('\\ud800')].join('|');"
        ),
        "\\x66oo|\\x31a|\\x61\\x2db|\\x61\\/b|\\x61\\x20b|\\[x\\]|é|\\ud800"
    );
}

#[test]
fn regexp_match_undefined_falls_back_to_the_internal_brand() {
    assert_eq!(
        rendered(
            "var r=/a/g;Object.defineProperty(r,Symbol.match,{value:undefined});\
             var identity=RegExp(r)===r;var nonGlobal=/a/;\
             Object.defineProperty(nonGlobal,Symbol.match,{value:undefined});var rejected=false;\
             try{'a'.replaceAll(nonGlobal,'x')}catch(error){rejected=error instanceof TypeError}\
             return identity+'|'+rejected;"
        ),
        "true|true"
    );
}
