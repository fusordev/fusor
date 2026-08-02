//! ECMA-262 `JSON.parse`, including the ES2026 reviver context record.

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
        "parse"
    );
    assert!(boolean(
        "const d=Object.getOwnPropertyDescriptor(this,'JSON');\
         const p=Object.getOwnPropertyDescriptor(JSON,'parse');\
         return d.writable&&!d.enumerable&&d.configurable&&p.writable&&!p.enumerable&&p.configurable;"
    ));
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
