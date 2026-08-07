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
    Function, JsString, Object, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
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

fn call_with_state(
    runtime: &mut Runtime,
    realm: &quickjs_runtime::Realm,
    body: &str,
    state: &Object,
) -> String {
    let mut context = runtime.context(realm).expect("context");
    let run = dynamic_function(&mut context, body);
    context
        .call(&run, &[state.as_value()], ExecutionLimits::default())
        .expect("state call")
        .as_string()
        .expect("live result")
        .expect("String result")
        .to_utf8_lossy()
        .expect("UTF-8")
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
fn regexp_constructor_routes_proxy_pattern_and_new_target_gets() {
    assert_eq!(
        rendered(
            "var log='';var pattern=new Proxy({}, {get:function(target,key,receiver){\
               if(key===Symbol.match){log=log+'m';return true;}\
               if(key==='source'){log=log+'s';return 'a';}\
               if(key==='flags'){log=log+'f';return 'g';}\
               return Reflect.get(target,key,receiver);}});\
             var prototype=Object.create(RegExp.prototype);\
             var newTarget=new Proxy(function(){},{get:function(target,key,receiver){\
               if(key==='prototype'){log=log+'p';return prototype;}\
               return Reflect.get(target,key,receiver);}});\
             var value=Reflect.construct(RegExp,[pattern],newTarget);\
             return value.source+'|'+value.flags+'|'+\
                    (Object.getPrototypeOf(value)===prototype)+'|'+log;"
        ),
        "a|g|true|msfp"
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

#[test]
fn regexp_symbol_match_preserves_es2025_flags_exec_and_empty_advance_order() {
    assert_eq!(
        rendered(
            "var log=[];var exact={ok:true};var receiver={\
               get flags(){log.push('flags');return {toString:function(){log.push('flagsString');return ''}}},\
               get exec(){log.push('execGet');return function(input){log.push('execCall:'+input);return exact}}\
             };var input={toString:function(){log.push('input');return 'needle'}};\
             var result=RegExp.prototype[Symbol.match].call(receiver,input);\
             return (result===exact)+'|'+log.join(',');"
        ),
        "true|input,flags,flagsString,execGet,execCall:needle"
    );

    assert_eq!(
        rendered(
            "var log=[];var backing=9;var calls=0;var receiver={\
               get flags(){log.push('flags');return 'gu'},\
               get lastIndex(){var seen=backing;log.push('get:'+seen);return {valueOf:function(){log.push('valueOf:'+seen);return seen}}},\
               set lastIndex(value){log.push('set:'+value);backing=value},\
               exec:function(input){log.push('exec:'+backing+':'+input.length);\
                 if(calls++<2)return {get 0(){log.push('zero');return ''}};return null}\
             };var result=RegExp.prototype[Symbol.match].call(receiver,'😀');\
             return result.join('|')+'#'+log.join(',')+'#'+backing;"
        ),
        "|#flags,set:0,exec:0:2,zero,get:0,valueOf:0,set:2,exec:2:2,zero,get:2,valueOf:2,set:3,exec:3:2#3"
    );

    assert_eq!(
        thrown(
            "return RegExp.prototype[Symbol.match].call({flags:'',exec:function(){return 1}},'x');"
        )
        .0,
        ExceptionKind::TypeError
    );
}

#[test]
fn regexp_symbol_match_runs_the_builtin_global_path() {
    assert_eq!(
        rendered(
            "var regexp=/a/g;regexp.lastIndex=99;var result=regexp[Symbol.match]('baab');\
             return result.join(',')+'|'+regexp.lastIndex;"
        ),
        "a,a|0"
    );
}

#[test]
fn regexp_symbol_replace_expands_captures_and_every_substitution_form() {
    assert_eq!(
        rendered(
            "return 'xabcy'.replace(/(?<first>a)(b)?(z)?/g,\
             '[$$][$&][$`][$\\'][$1][$2][$3][$<first>][$<missing>][$12][$99]');"
        ),
        "x[$][ab][x][cy][a][b][][a][][a2][$99]cy"
    );
}

#[test]
fn regexp_symbol_replace_rejects_an_impossible_capture_template_before_materializing_it() {
    assert_eq!(
        rendered(
            "var input='x'.repeat(4096);var template='$1'.repeat(262144);\
             try { input.replace(/(.+)/,template); return 'missed'; }\
             catch (error) { return error instanceof InternalError ? 'caught' : 'wrong'; }"
        ),
        "caught"
    );
}

#[test]
fn regexp_symbol_replace_passes_functional_capture_arguments() {
    assert_eq!(
        rendered(
            "var log=[];var output='a1 b'.replace(/(?<letter>[a-z])(\\d)?/g,function(){\
               var args=arguments;log.push([args[0],args[1],String(args[2]),args[3],args[4],args[5].letter].join(','));\
               return args[5].letter+':'+args[3]});\
             return output+'|'+log.join(';');"
        ),
        "a:0 b:3|a1,a,1,0,a1 b,a;b,b,undefined,3,a1 b,b"
    );
}

#[test]
fn regexp_symbol_replace_collects_results_before_processing_them() {
    assert_eq!(
        rendered(
            "var log=[],call=0,backing=9;\
             function result(text,index){var value={};\
               Object.defineProperty(value,'0',{get:function(){log.push('zero:'+index);return text}});\
               Object.defineProperty(value,'length',{get:function(){log.push('length:'+index);return 1}});\
               Object.defineProperty(value,'index',{get:function(){log.push('index:'+index);return index}});\
               Object.defineProperty(value,'groups',{get:function(){log.push('groups:'+index);return undefined}});\
               return value}\
             var receiver={\
               get flags(){log.push('flags');return {toString:function(){log.push('flags-string');return 'g'}}},\
               get lastIndex(){log.push('get-last:'+backing);return backing},\
               set lastIndex(value){log.push('set-last:'+value);backing=value},\
               get exec(){log.push('exec-get');return function(input){var current=call++;\
                 log.push('exec:'+current+':'+input);\
                 return current===0?result('a',0):current===1?result('b',2):null}}\
             };\
             var output=RegExp.prototype[Symbol.replace].call(receiver,'abc',function(match,position){\
               log.push('replace:'+match+':'+position);return match.toUpperCase()});\
             return output+'|'+log.join(',');"
        ),
        "AbB|flags,flags-string,set-last:0,exec-get,exec:0:abc,zero:0,\
         exec-get,exec:1:abc,zero:2,exec-get,exec:2:abc,length:0,zero:0,index:0,groups:0,\
         replace:a:0,length:2,zero:2,index:2,groups:2,replace:b:2"
    );
}

#[test]
fn regexp_symbol_replace_observes_input_replacement_flags_and_reset_order() {
    assert_eq!(
        rendered(
            "var log=[];var receiver={\
               get flags(){log.push('flags');return {toString:function(){log.push('flags-string');return 'g'}}},\
               set lastIndex(value){log.push('set:'+value)},\
               get exec(){log.push('exec-get');return function(input){log.push('exec:'+input);return null}}\
             };\
             var input={toString:function(){log.push('input');return 'subject'}};\
             var replacement={toString:function(){log.push('replacement');return 'unused'}};\
             var output=RegExp.prototype[Symbol.replace].call(receiver,input,replacement);\
             return output+'|'+log.join(',');"
        ),
        "subject|input,replacement,flags,flags-string,set:0,exec-get,exec:subject"
    );
}

#[test]
fn regexp_symbol_replace_advances_empty_global_matches_with_full_unicode() {
    assert_eq!(
        rendered(
            "var log=[],backing=7,calls=0;var match={length:1,index:0,groups:undefined,\
               get 0(){log.push('zero');return ''}};\
             var receiver={get flags(){log.push('flags');return 'gu'},\
               get lastIndex(){log.push('get:'+backing);return backing},\
               set lastIndex(value){log.push('set:'+value);backing=value},\
               get exec(){log.push('exec-get');return function(){log.push('exec:'+backing);return calls++?null:match}}};\
             var output=RegExp.prototype[Symbol.replace].call(receiver,'😀','x');\
             return output+'|'+log.join(',')+'|'+backing;"
        ),
        "x😀|flags,set:0,exec-get,exec:0,zero,get:0,set:2,exec-get,exec:2,zero|2"
    );
}

#[test]
fn regexp_symbol_replace_resumes_capture_and_named_group_conversions_in_order() {
    assert_eq!(
        rendered(
            "var log=[];var groups={get name(){log.push('name');return {toString:function(){log.push('name-string');return 'N'}}}};\
             var result={get length(){log.push('length');return 2},\
               get 0(){log.push('matched');return 'a'},\
               get 1(){log.push('capture');return {toString:function(){log.push('capture-string');return 'C'}}},\
               get index(){log.push('index');return {valueOf:function(){log.push('index-value');return 0}}},\
               get groups(){log.push('groups');return groups}};\
             var receiver={get flags(){log.push('flags');return ''},\
               get exec(){log.push('exec-get');return function(){log.push('exec');return result}}};\
             var output=RegExp.prototype[Symbol.replace].call(receiver,'abc','$1:$<name>');\
             return output+'|'+log.join(',');"
        ),
        "C:Nbc|flags,exec-get,exec,length,matched,index,index-value,capture,capture-string,groups,name,name-string"
    );
}

#[test]
fn regexp_symbol_replace_computes_but_ignores_backwards_results() {
    assert_eq!(
        rendered(
            "var calls=0,log=[];var receiver={flags:'g',lastIndex:0,exec:function(){\
               var call=calls++;return call===0?{0:'c',length:1,index:2,groups:undefined}:\
                 call===1?{0:'b',length:1,index:1,groups:undefined}:null}};\
             var output=RegExp.prototype[Symbol.replace].call(receiver,'abc',function(match,position){\
               log.push(match+':'+position);return match.toUpperCase()});\
             return output+'|'+log.join(',');"
        ),
        "abC|c:2,b:1"
    );

    assert_eq!(
        thrown(
            "return RegExp.prototype[Symbol.replace].call({flags:'',exec:function(){return 1}},'x','y');"
        )
        .0,
        ExceptionKind::TypeError
    );
}

#[test]
fn regexp_symbol_split_splices_captures_and_honours_limit() {
    assert_eq!(
        rendered(
            "var full='A<B>bold</B>'.split(/<(\\/)?([^<>]+)>/);\
             var limited='a,b,c'.split(/(,)/,3);\
             return full.map(String).join('|')+'#'+limited.join('|');"
        ),
        "A|undefined|B|bold|/|B|#a|,|b"
    );

    assert_eq!(
        rendered(
            "return ['ab'.split(/(?:)/).join('|'),'😀'.split(/(?:)/u).join('|'),\
                    ''.split(/(?:)/).length,''.split(/x/).length].join('#');"
        ),
        "a|b#😀#0#1"
    );
}

#[test]
fn regexp_symbol_split_preserves_species_flags_limit_and_sticky_exec_order() {
    assert_eq!(
        rendered(
            "var log=[],backing=9,calls=0;var splitter={\
               get lastIndex(){var seen=backing;log.push('get:'+seen);return {valueOf:function(){log.push('value:'+seen);return seen}}},\
               set lastIndex(value){log.push('set:'+value);backing=value},\
               get exec(){log.push('exec-get');return function(input){var call=calls++;log.push('exec:'+call+':'+input);\
                 if(call===0)return null;backing=2;return {length:1}}}};\
             function Species(pattern,flags){log.push('construct:'+(pattern===receiver)+':'+flags);return splitter}\
             var receiver={\
               get constructor(){log.push('constructor');return {get [Symbol.species](){log.push('species');return Species}}},\
               get flags(){log.push('flags');return {toString:function(){log.push('flags-string');return 'u'}}}};\
             var input={toString:function(){log.push('input');return 'ab'}};\
             var limit={valueOf:function(){log.push('limit');return 3}};\
             var output=RegExp.prototype[Symbol.split].call(receiver,input,limit);\
             return output.join('|')+'#'+log.join(',');"
        ),
        "a|#input,constructor,species,flags,flags-string,construct:true:uy,limit,\
         set:0,exec-get,exec:0:ab,set:1,exec-get,exec:1:ab,get:2,value:2"
    );
}

#[test]
fn regexp_symbol_split_reads_only_captures_admitted_by_the_limit() {
    assert_eq!(
        rendered(
            "var log=[],backing=0;var result={\
               get length(){log.push('length');return 4},\
               get 1(){log.push('capture:1');return 'X'},\
               get 2(){log.push('capture:2');return undefined},\
               get 3(){log.push('capture:3');return 'unread'}};\
             var splitter={get lastIndex(){log.push('get:'+backing);return backing},\
               set lastIndex(value){log.push('set:'+value);backing=value},\
               exec:function(){log.push('exec:'+backing);if(backing===0)return null;backing=2;return result}};\
             function Species(){return splitter}\
             var receiver={constructor:{[Symbol.species]:Species},flags:'y'};\
             var output=RegExp.prototype[Symbol.split].call(receiver,'abc',3);\
             return output.map(String).join('|')+'#'+log.join(',');"
        ),
        "a|X|undefined#set:0,exec:0,set:1,exec:1,get:2,length,capture:1,capture:2"
    );
}

#[test]
fn regexp_symbol_split_converts_zero_limit_before_empty_input_exec() {
    assert_eq!(
        rendered(
            "var log=[];var splitter={exec:function(){log.push('exec');return null}};\
             function Species(){log.push('construct');return splitter}\
             var receiver={constructor:{[Symbol.species]:Species},flags:''};\
             var input={toString:function(){log.push('input');return ''}};\
             var limit={valueOf:function(){log.push('limit');return 0}};\
             var output=RegExp.prototype[Symbol.split].call(receiver,input,limit);\
             return output.length+'|'+log.join(',');"
        ),
        "0|input,construct,limit"
    );
}

#[test]
fn regexp_symbol_search_restores_last_index_before_reading_the_result_index() {
    assert_eq!(
        rendered(
            "var log=[];var backing=-0;var receiver={\
               get lastIndex(){log.push('get:'+(1/backing));return backing},\
               set lastIndex(value){log.push('set:'+(1/value));backing=value},\
               get exec(){log.push('execGet');return function(input){log.push('execCall:'+input);backing=7;\
                 return {get index(){log.push('index');return 3}}}}\
             };var input={toString:function(){log.push('input');return 'abc'}};\
             var result=RegExp.prototype[Symbol.search].call(receiver,input);\
             return result+'|'+log.join(',')+'|'+(1/backing);"
        ),
        "3|input,get:-Infinity,set:Infinity,execGet,execCall:abc,get:0.14285714285714285,set:-Infinity,index|-Infinity"
    );

    assert_eq!(
        rendered(
            "var regexp=/a/g;regexp.lastIndex=2;var result=regexp[Symbol.search]('ba');\
             return result+'|'+regexp.lastIndex;"
        ),
        "1|2"
    );
}

#[test]
fn regexp_symbol_match_all_clones_last_index_and_exposes_the_exact_iterator_surface() {
    assert_eq!(
        rendered(
            "var regexp=/(?<letter>a)?/g;regexp.lastIndex=1;\
             var iterator=regexp[Symbol.matchAll]('ba');\
             var first=iterator.next();var second=iterator.next();var done=iterator.next();\
             var prototype=Object.getPrototypeOf(iterator);\
             return [regexp.lastIndex,first.value[0],first.value.index,first.value.groups.letter,first.done,\
                     second.value[0],second.value.index,String(second.value.groups.letter),second.done,done.done,\
                     Object.prototype.toString.call(iterator),prototype.next.name,prototype.next.length,\
                     iterator[Symbol.iterator]()===iterator].join('|');"
        ),
        "1|a|1|a|false||2|undefined|false|true|[object RegExp String Iterator]|next|0|true"
    );
}

#[test]
fn regexp_symbol_match_all_is_lazy_and_preserves_species_construction_order() {
    assert_eq!(
        rendered(
            "var log=[];var match={0:'',length:1};var calls=0;var backing=0;\
             function Species(pattern,flags){log.push('construct:'+(pattern===receiver)+':'+flags);return {\
               get lastIndex(){log.push('matcher-get:'+backing);return backing},\
               set lastIndex(value){log.push('matcher-set:'+value);backing=value},\
               get exec(){log.push('exec-get');return function(input){log.push('exec:'+input+':'+backing);return calls++?null:match}}\
             }}\
             var receiver={\
               get constructor(){log.push('constructor');return {get [Symbol.species](){log.push('species');return Species}}},\
               get flags(){log.push('flags');return {toString:function(){log.push('flags-string');return 'gu'}}},\
               get lastIndex(){log.push('last-index');return {valueOf:function(){log.push('last-index-value');return 0}}}\
             };\
             var input={toString:function(){log.push('input');return '😀'}};\
             var iterator=RegExp.prototype[Symbol.matchAll].call(receiver,input);\
             var before=log.join(',');var first=iterator.next();var after=log.join(',');var done=iterator.next();\
             return [before,first.value===match,first.done,after,backing,done.done].join('|');"
        ),
        "input,constructor,species,flags,flags-string,construct:true:gu,last-index,last-index-value,matcher-set:0|true|false|input,constructor,species,flags,flags-string,construct:true:gu,last-index,last-index-value,matcher-set:0,exec-get,exec:😀:0,matcher-get:0,matcher-set:2|2|true"
    );
}

#[test]
fn regexp_symbol_match_all_yields_once_when_not_global_and_validates_exec_results() {
    assert_eq!(
        rendered(
            "var calls=0;function Species(){return receiver}var receiver={constructor:{[Symbol.species]:Species},flags:'',lastIndex:0,\
               exec:function(){calls++;return calls===1?{0:'x',length:1}:null}};\
             var iterator=RegExp.prototype[Symbol.matchAll].call(receiver,'x');\
             var first=iterator.next();var done=iterator.next();\
             return [first.value[0],first.done,done.done,calls].join('|');"
        ),
        "x|false|true|1"
    );
    assert_eq!(
        thrown(
            "var receiver={flags:'g',lastIndex:0,exec:function(){return 1}};function Species(){return receiver}receiver.constructor={[Symbol.species]:Species};var iterator=RegExp.prototype[Symbol.matchAll].call(receiver,'x');return iterator.next();"
        )
        .0,
        ExceptionKind::TypeError
    );
}

#[test]
fn regexp_string_iterator_rejects_reentry_and_resumes_the_outer_next() {
    assert_eq!(
        rendered(
            "var log=[];var iterator;var receiver={flags:'g',lastIndex:0,\
             exec:function(){try{iterator.next()}catch(error){log.push(error instanceof TypeError)}return null}};\
             function Species(){return receiver}receiver.constructor={[Symbol.species]:Species};\
             iterator=RegExp.prototype[Symbol.matchAll].call(receiver,'x');\
             var first=iterator.next();var second=iterator.next();\
             return [log.join(','),first.done,second.done].join('|');"
        ),
        "true|true|true"
    );
}

#[test]
fn regexp_string_iterator_closes_after_abrupt_exec_or_result_access() {
    assert_eq!(
        rendered(
            "function iteratorFor(receiver){function Species(){return receiver}\
               receiver.constructor={[Symbol.species]:Species};\
               return RegExp.prototype[Symbol.matchAll].call(receiver,'x')}\
             var execIterator=iteratorFor({flags:'g',lastIndex:0,exec:function(){throw 'exec'}});\
             var resultIterator=iteratorFor({flags:'g',lastIndex:0,exec:function(){\
               return {get 0(){throw 'zero'},length:1,index:0}}});\
             var symbolIterator=iteratorFor({flags:'g',lastIndex:0,exec:function(){\
               return {0:Symbol('match'),length:1,index:0}}});\
             var calls=0;var indexIterator=iteratorFor({flags:'g',\
               get lastIndex(){return calls?Symbol('index'):0},set lastIndex(value){},\
               exec:function(){calls++;return {0:'',length:1,index:0}}});\
             var caught=[];try{execIterator.next()}catch(error){caught.push(error)}\
             try{resultIterator.next()}catch(error){caught.push(error)}\
             try{symbolIterator.next()}catch(error){caught.push(error instanceof TypeError?'symbol':'bad')}\
             try{indexIterator.next()}catch(error){caught.push(error instanceof TypeError?'index':'bad')}\
             return [caught.join(','),execIterator.next().done,resultIterator.next().done,\
                     symbolIterator.next().done,indexIterator.next().done].join('|');"
        ),
        "exec,zero,symbol,index|true|true|true|true"
    );
}

#[test]
fn string_match_all_enforces_global_before_protocol_dispatch_and_has_a_regexp_fallback() {
    assert_eq!(
        rendered(
            "var log=[];var receiver={toString:function(){log.push('receiver');return 'aba'}};\
             var pattern={\
               get [Symbol.match](){log.push('match');return true},\
               get flags(){log.push('flags');return {toString:function(){log.push('flags-string');return 'g'}}},\
               get [Symbol.matchAll](){log.push('matchAll');return function(value){log.push('call');return value===receiver}}\
             };var direct=String.prototype.matchAll.call(receiver,pattern);\
             var matches=[];for(var match of 'aba'.matchAll('a'))matches.push(match.index);\
             return direct+'|'+log.join(',')+'|'+matches.join(',');"
        ),
        "true|match,flags,flags-string,matchAll,call|0,2"
    );

    assert_eq!(
        rendered(
            "var log=[];var receiver={toString:function(){log.push('receiver');return 'x'}};\
             var pattern={get [Symbol.match](){log.push('match');return true},get flags(){log.push('flags');return 'i'},\
               get [Symbol.matchAll](){log.push('matchAll');return function(){}}};\
             var rejected=false;try{String.prototype.matchAll.call(receiver,pattern)}catch(error){rejected=error instanceof TypeError}\
             return rejected+'|'+log.join(',');"
        ),
        "true|match,flags"
    );
}

#[test]
fn regexp_string_iterator_keeps_its_matcher_alive_only_until_completion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let state = {
        let mut context = runtime.context(&realm).expect("setup context");
        let setup = dynamic_function(
            &mut context,
            "var matcher={lastIndex:0,exec:function(){return null}};\
             var reference=new WeakRef(matcher);\
             var regexp=/x/g;function Species(){return matcher}\
             regexp.constructor={[Symbol.species]:Species};\
             var iterator=regexp[Symbol.matchAll]('x');matcher=null;\
             return {iterator:iterator,reference:reference};",
        );
        context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("setup")
            .into_object()
            .expect("state")
    };

    runtime.collect_cycles().expect("live matcher collection");
    assert_eq!(
        call_with_state(
            &mut runtime,
            &realm,
            "var state=arguments[0];return [state.reference.deref()!==undefined,state.iterator.next().done].join('|');",
            &state,
        ),
        "true|true"
    );

    runtime
        .collect_cycles()
        .expect("completed matcher collection");
    assert_eq!(
        call_with_state(
            &mut runtime,
            &realm,
            "return String(arguments[0].reference.deref());",
            &state,
        ),
        "undefined"
    );
}
