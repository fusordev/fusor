//! ECMA-262 `JSON.parse`, including the ES2026 reviver context record.

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
                    Arc::from("<runtime JSON.parse>"),
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

fn exception_kind(body: &str) -> ExceptionKind {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript exception from {body}");
        };
        exception.kind().expect("engine exception kind")
    })
}

#[test]
fn json_parse_has_the_standard_identity_and_json_tag() {
    assert_eq!(
        text("return JSON.parse.name+','+JSON.parse.length;"),
        "parse,2"
    );
    assert_eq!(
        text("return Object.prototype.toString.call(JSON);"),
        "[object JSON]"
    );
    assert_eq!(
        text("return Object.getOwnPropertyNames(JSON).join(',');"),
        "isRawJSON,parse,rawJSON,stringify"
    );
    assert!(boolean(
        "const d=Object.getOwnPropertyDescriptor(this,'JSON');\
         const p=Object.getOwnPropertyDescriptor(JSON,'parse');\
         return d.writable&&!d.enumerable&&d.configurable&&p.writable&&!p.enumerable&&p.configurable;"
    ));
}

#[test]
fn json_stringify_has_the_standard_identity_and_descriptor() {
    assert_eq!(
        text("return JSON.stringify.name+','+JSON.stringify.length;"),
        "stringify,3"
    );
    assert!(boolean(
        "const d=Object.getOwnPropertyDescriptor(JSON,'stringify');\
         return d.value===JSON.stringify&&d.writable&&!d.enumerable&&d.configurable;"
    ));
}

#[test]
fn json_stringify_serializes_primitives_and_quotes_well_formed_strings() {
    assert!(boolean(
        r#"return JSON.stringify(null)==='null'&&
                  JSON.stringify(true)==='true'&&
                  JSON.stringify('x')==='\"x\"'&&
                  JSON.stringify(-0)==='0'&&
                  JSON.stringify(NaN)==='null'&&
                  JSON.stringify(Infinity)==='null'&&
                  JSON.stringify(undefined)===undefined&&
                  JSON.stringify(function(){})===undefined&&
                  JSON.stringify(Symbol('x'))===undefined&&
                  JSON.stringify('\b\t\n\f\r\"\\')==='\"\\b\\t\\n\\f\\r\\\"\\\\\"'&&
                  JSON.stringify('\ud800')==='\"\\ud800\"'&&
                  JSON.stringify('\udc00')==='\"\\udc00\"';"#
    ));
    assert_eq!(
        exception_kind("return JSON.stringify(BigInt(1));"),
        ExceptionKind::TypeError
    );
}

#[test]
fn json_stringify_snapshots_array_length_and_object_keys() {
    assert!(boolean(
        r#"const array=[1];
           array.length=4;array[2]=undefined;array.extra=9;
           if(JSON.stringify(array)!=='[1,null,null,null]')return false;
           const shrinking=[1,2];
           Object.defineProperty(shrinking,0,{enumerable:true,configurable:true,get(){shrinking.length=0;return 7;}});
           if(JSON.stringify(shrinking)!=='[7,null]')return false;
           const object={};
           object.b=1;object[2]=2;object[1]=3;object.a=4;
           Object.defineProperty(object,'hidden',{enumerable:false,value:5});
           object[Symbol('s')]=6;
           if(JSON.stringify(object)!=='{\"1\":3,\"2\":2,\"b\":1,\"a\":4}')return false;
           const changing={};
           Object.defineProperty(changing,'a',{enumerable:true,get(){delete changing.b;changing.c=3;return 1;}});
           changing.b=2;
           return JSON.stringify(changing)==='{\"a\":1}';"#
    ));
}

#[test]
fn json_stringify_orders_to_json_and_replacer_observably() {
    assert_eq!(
        text(
            r"let log='';let receiver=false;let holder=false;
               const value={get toJSON(){log+='get;';return function serializer(key){receiver=this===value;log+='call:'+key+';';return {x:1};};}};
               const root={a:value};
               const result=JSON.stringify(root,function(key,current){
                 log+='replace:'+key+';';
                 if(key==='a')holder=this===root;
                 return current;
               });
               return result+'|'+log+'|'+receiver+'|'+holder;"
        ),
        "{\"a\":{\"x\":1}}|replace:;get;call:a;replace:a;replace:x;|true|true"
    );
    assert!(boolean(
        r#"BigInt.prototype.toJSON=function bigintToJSON(key){return key==='a'?'big':'bad';};
           return JSON.stringify({a:BigInt(1)})==='{\"a\":\"big\"}';"#
    ));
}

#[test]
fn json_stringify_applies_replacer_function_omission_rules() {
    assert_eq!(
        text(
            r"let rootThis=false;
               const object={a:1,b:2,c:[undefined,function(){},Symbol('x')]};
               const result=JSON.stringify(object,function(key,value){
                 if(key==='')rootThis=this!==object&&this['']===object;
                 if(key==='b')return undefined;
                 return value;
               });
               return result+'|'+rootThis;"
        ),
        "{\"a\":1,\"c\":[null,null,null]}|true"
    );
}

#[test]
fn json_stringify_builds_the_replacer_property_list_in_order() {
    assert_eq!(
        text(
            r"let log='';
               const boxed=new String('ignored');
               boxed.toString=function boxedToString(){log+='coerce;';return 'b';};
               const replacer=['a',boxed,'a',2,true,Symbol('x')];
               Object.defineProperty(replacer,0,{get(){log+='get0;';return 'a';}});
               Object.defineProperty(replacer,1,{get(){log+='get1;';return boxed;}});
               const result=JSON.stringify({a:1,b:2,2:3,c:4},replacer);
               return result+'|'+log;"
        ),
        "{\"a\":1,\"b\":2,\"2\":3}|get0;get1;coerce;"
    );
}

#[test]
fn json_stringify_clamps_gap_and_observes_boxed_space_conversion() {
    assert_eq!(
        text("return JSON.stringify({a:{b:1}},null,20);"),
        "{\n          \"a\": {\n                    \"b\": 1\n          }\n}"
    );
    assert_eq!(
        text(
            r"let log='';const space=new String('ignored');
               space.toString=function spaceToString(){log='space';return 'abcdefghijk';};
               return JSON.stringify({a:1},null,space)+'|'+log;"
        ),
        "{\nabcdefghij\"a\": 1\n}|space"
    );
}

#[test]
fn json_stringify_unboxes_branded_primitives_and_honors_callable_to_json() {
    assert_eq!(
        text(
            r"let log='';
               const number=new Number(1);
               number.valueOf=function numberValueOf(){log+='number;';return 2;};
               const string=new String('original');
               string.toString=function stringToString(){log+='string;';return 'changed';};
               const boolean=new Boolean(false);
               boolean.valueOf=function booleanValueOf(){return true;};
               function callable(){}
               callable.toJSON=function functionToJSON(key){return key;};
               return JSON.stringify(number)+'|'+JSON.stringify(string)+'|'+
                 JSON.stringify(boolean)+'|'+JSON.stringify({f:callable})+'|'+log;"
        ),
        "2|\"changed\"|false|{\"f\":\"f\"}|number;string;"
    );
    assert_eq!(
        exception_kind("return JSON.stringify(Object(BigInt(1)));"),
        ExceptionKind::TypeError
    );
}

#[test]
fn json_stringify_does_not_reapply_to_json_to_callback_results() {
    assert!(boolean(
        r#"let calls=0;
           const replacement={toJSON(){calls++;return "wrong";}};
           const original={toJSON(){calls++;return replacement;}};
           if(JSON.stringify(original)!=='{}'||calls!==1)return false;
           calls=0;
           const result=JSON.stringify({x:1},function(key,value){return key==='x'?replacement:value;});
           return result==='{"x":{}}'&&calls===0;"#
    ));
}

#[test]
fn json_stringify_uses_property_lists_for_inherited_values_without_recursion() {
    assert!(boolean(
        r#"const list=[];for(let i=0;i<512;i++){list[i]='x';}
           const object=Object.create({x:1});
           if(JSON.stringify(object,list)!=='{"x":1}')return false;
           const raw=JSON.stringify({x:1},function(key,value){
             return key==='x'?JSON.rawJSON('1e2'):value;
           });
           return raw==='{"x":1e2}';"#
    ));
}

#[test]
fn json_stringify_embeds_raw_json_and_rejects_cycles() {
    assert_eq!(
        text(r#"return JSON.stringify({a:JSON.rawJSON('1e2'),b:[JSON.rawJSON('\"x\"')]});"#),
        "{\"a\":1e2,\"b\":[\"x\"]}"
    );
    assert!(boolean(
        "const shared={x:1};return JSON.stringify([shared,shared])==='[{\"x\":1},{\"x\":1}]';"
    ));
    assert_eq!(
        exception_kind("const value={};value.self=value;return JSON.stringify(value);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn json_stringify_propagates_abrupt_completions() {
    assert!(boolean(
        r"let count=0;
           try{JSON.stringify({get a(){throw 41;}});}catch(error){if(error===41)count++;}
           try{JSON.stringify({a:{toJSON(){throw 42;}}});}catch(error){if(error===42)count++;}
           try{JSON.stringify({a:1},function(){throw 43;});}catch(error){if(error===43)count++;}
           const item=new String('a');item.toString=function itemToString(){throw 44;};
           try{JSON.stringify({a:1},[item]);}catch(error){if(error===44)count++;}
           const space=new Number(1);space.valueOf=function spaceValueOf(){throw 45;};
           try{JSON.stringify({a:1},null,space);}catch(error){if(error===45)count++;}
           return count===5;"
    ));
}

#[test]
fn json_stringify_routes_container_protocols_through_proxy_internals() {
    assert_eq!(
        text(
            "const log=[];\
             const array=new Proxy([1,2],{get(target,key,receiver){\
               log.push('g'+String(key));return Reflect.get(target,key,receiver);\
             }});\
             const arrayText=JSON.stringify(array);\
             const object=new Proxy({a:1,b:2},{\
               get(target,key,receiver){log.push('g'+String(key));return Reflect.get(target,key,receiver);},\
               ownKeys(target){log.push('keys');return Reflect.ownKeys(target);},\
               getOwnPropertyDescriptor(target,key){log.push('d'+String(key));\
                 return Reflect.getOwnPropertyDescriptor(target,key);}\
             });\
             const objectText=JSON.stringify(object);\
             return arrayText+'|'+objectText+'|'+log.join(',');"
        ),
        "[1,2]|{\"a\":1,\"b\":2}|gtoJSON,glength,g0,g1,gtoJSON,keys,da,db,ga,gb"
    );

    assert_eq!(
        text(
            "const log=[];\
             const list=new Proxy(['b'],{get(target,key,receiver){\
               log.push('g'+String(key));return Reflect.get(target,key,receiver);\
             }});\
             return JSON.stringify({a:1,b:2},list)+'|'+log.join(',');"
        ),
        "{\"b\":2}|glength,g0"
    );
}

#[test]
fn deeply_nested_json_stringify_uses_a_worklist() {
    assert!(boolean(
        "let value=0;for(let i=0;i<2000;i++){value=[value];}\
         if(JSON.stringify(value).length!==4001)return false;\
         value=0;for(let i=0;i<2000;i++){value={x:value};}\
         return JSON.stringify(value).length===12001;"
    ));
}

#[test]
fn raw_json_has_standard_identities_and_an_unforgeable_frozen_brand() {
    assert_eq!(
        text(
            "return JSON.isRawJSON.name+','+JSON.isRawJSON.length+','+\
             JSON.rawJSON.name+','+JSON.rawJSON.length;"
        ),
        "isRawJSON,1,rawJSON,1"
    );
    assert!(boolean(
        "const raw=JSON.rawJSON('1e2');\
         const d=Object.getOwnPropertyDescriptor(raw,'rawJSON');\
         return JSON.isRawJSON(raw)&&!JSON.isRawJSON({rawJSON:'1e2'})&&\
           !JSON.isRawJSON(1)&&Object.getPrototypeOf(raw)===null&&Object.isFrozen(raw)&&\
           raw.rawJSON==='1e2'&&!d.writable&&d.enumerable&&!d.configurable&&\
           Object.prototype.toString.call(raw)==='[object Object]';"
    ));
}

#[test]
fn raw_json_accepts_only_exact_primitive_json_text() {
    assert!(boolean(
        "const values=['null','true','false','-0','1e2','\"x\"','\"\\ud800\"'];\
         for(let i=0;i<values.length;i++){\
           if(JSON.rawJSON(values[i]).rawJSON!==values[i])return false;\
         }return true;"
    ));
    for source in [
        "",
        " 1",
        "1 ",
        "[]",
        "{}",
        "+1",
        "01",
        "NaN",
        "Infinity",
        "undefined",
        "\u{a0}null",
    ] {
        let escaped = source
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        assert_eq!(
            exception_kind(&format!("return JSON.rawJSON('{escaped}');")),
            ExceptionKind::SyntaxError,
            "accepted {source:?}"
        );
    }
}

#[test]
fn raw_json_performs_tostring_before_grammar_validation() {
    assert_eq!(
        text(
            "let log='';\
             const raw=JSON.rawJSON({toString(){log='called';return 'null';}});\
             return log+':'+raw.rawJSON;"
        ),
        "called:null"
    );
    assert_eq!(
        exception_kind("return JSON.rawJSON(Symbol('x'));"),
        ExceptionKind::TypeError
    );
}

#[test]
fn json_parse_materializes_exact_json_data_properties() {
    assert!(boolean(
        "const o=JSON.parse('{\"__proto__\":{\"polluted\":1},\"a\":1,\"a\":2,\"0\":\"zero\"}');\
         return o.a===2&&o[0]==='zero'&&Object.hasOwn(o,'__proto__')&&\
           Object.getPrototypeOf(o)===Object.prototype&&Object.getPrototypeOf(o.__proto__)===Object.prototype&&\
           !Object.hasOwn(Object.prototype,'polluted');"
    ));
    assert!(boolean(
        "const a=JSON.parse('[null,true,false,-0,1.25e2,\"\\ud800\"]');\
         return a.length===6&&a[0]===null&&a[1]===true&&a[2]===false&&\
           Object.is(a[3],-0)&&a[4]===125&&a[5].length===1;"
    ));
}

#[test]
fn json_parse_rejects_every_javascript_extension() {
    for source in [
        "",
        "undefined",
        "NaN",
        "Infinity",
        "+1",
        "01",
        "1.",
        "[1,]",
        "{'a':1}",
        "{\"a\":1,}",
        "\"\\x41\"",
        "true false",
        "\u{a0}null",
    ] {
        let escaped = source
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        assert_eq!(
            exception_kind(&format!("return JSON.parse('{escaped}');")),
            ExceptionKind::SyntaxError,
            "accepted {source:?}"
        );
    }
}

#[test]
fn json_parse_coerces_text_before_parsing() {
    assert_eq!(
        text(
            "let log='';\
             const source={toString(){log+='text;';return '{\"x\":1}';}};\
             const value=JSON.parse(source,function(k,v){log+=k+';';return v;});\
             return log+value.x;"
        ),
        "text;x;;1"
    );
    assert_eq!(
        exception_kind("return JSON.parse(Symbol('x'));"),
        ExceptionKind::TypeError
    );
}

#[test]
fn reviver_walks_postorder_and_reports_exact_primitive_source() {
    assert_eq!(
        text(
            "let log='';\
             const value=JSON.parse('{\"a\":1e2,\"b\":\"x\",\"c\":[true]}',\
               function(k,v,c){\
                 log+=k+':'+(Object.hasOwn(c,'source')?c.source:'-')+';';\
                 if(k==='a')return v+1;\
                 if(k==='b')return undefined;\
                 return v;\
               });\
             return log+'|'+value.a+'|'+Object.hasOwn(value,'b')+'|'+value.c[0];"
        ),
        "a:1e2;b:\"x\";0:true;c:-;:-;|101|false|true"
    );
    assert_eq!(
        text(
            "let source='';\
             JSON.parse('{\"a\":1,\"a\":2}',function(k,v,c){if(k==='a')source=c.source;return v;});\
             return source;"
        ),
        "2"
    );
}

#[test]
fn reviver_rechecks_values_and_observes_prior_mutation() {
    assert_eq!(
        text(
            "let seen='';\
             const value=JSON.parse('{\"a\":1,\"b\":2}',function(k,v,c){\
               if(k==='a'){Object.defineProperty(this,'b',{enumerable:true,configurable:true,get(){return 7;}});}\
               if(k==='b'){seen=v+':'+Object.hasOwn(c,'source');return 8;}\
               return v;\
             });\
             return seen+'|'+value.b;"
        ),
        "7:false|8"
    );
}

#[test]
fn reviver_ignores_a_rejected_create_data_property_result() {
    assert_eq!(
        text(
            "const value=JSON.parse('[1,2]',function(key,item){\
               if(key==='0')Object.defineProperty(this,'1',{configurable:false});\
               return key==='1'?22:item;\
             });\
             return value[0]+'|'+value[1];"
        ),
        "1|2"
    );
}

#[test]
fn reviver_routes_replaced_containers_through_proxy_internals() {
    assert_eq!(
        text(
            "const log=[];let proxy,target;\
             const result=JSON.parse('{\"a\":0,\"b\":{\"x\":1,\"y\":2}}',function(key,value){\
               if(key==='a'){\
                 target={x:3,y:4};\
                 proxy=new Proxy(target,{\
                   ownKeys(object){log.push('keys');return Reflect.ownKeys(object);},\
                   getOwnPropertyDescriptor(object,name){log.push('d'+String(name));\
                     return Reflect.getOwnPropertyDescriptor(object,name);},\
                   get(object,name,receiver){log.push('g'+String(name));\
                     return Reflect.get(object,name,receiver);},\
                   deleteProperty(object,name){log.push('del'+String(name));\
                     return Reflect.deleteProperty(object,name);},\
                   defineProperty(object,name,descriptor){\
                     log.push('def'+String(name)+'='+descriptor.value);\
                     return Reflect.defineProperty(object,name,descriptor);}\
                 });\
                 this.b=proxy;\
               }\
               if(key==='x')return undefined;\
               if(key==='y')return 9;\
               return value;\
             });\
             return (result.b===proxy)+'|'+String(target.x)+'|'+target.y+'|'+log.join(',');"
        ),
        "true|undefined|9|keys,dx,dy,gx,delx,gy,defy=9"
    );

    assert_eq!(
        text(
            "const log=[];let proxy,target;\
             const result=JSON.parse('{\"a\":0,\"b\":[]}',function(key,value){\
               if(key==='a'){target=[3,4];proxy=new Proxy(target,{\
                 get(object,name,receiver){log.push('g'+String(name));\
                   return Reflect.get(object,name,receiver);},\
                 defineProperty(object,name,descriptor){log.push('d'+String(name));\
                   return Reflect.defineProperty(object,name,descriptor);}\
               });this.b=proxy;}\
               return value;\
             });\
             return (result.b===proxy)+'|'+target.join(',')+'|'+log.join(',');"
        ),
        "true|3,4|glength,g0,d0,g1,d1"
    );
}

#[test]
fn reviver_abrupt_completion_propagates() {
    assert!(boolean(
        "try{JSON.parse('{\"a\":1}',function(k,v){if(k==='a')throw 92;return v;});}\
         catch(error){return error===92;}return false;"
    ));
}

#[test]
fn deeply_nested_json_uses_worklists_instead_of_the_rust_stack() {
    assert!(boolean(
        "let source='0';for(let i=0;i<2000;i++){source='['+source+']';}\
         let value=JSON.parse(source);for(let i=0;i<2000;i++){value=value[0];}\
         return value===0;"
    ));
}
