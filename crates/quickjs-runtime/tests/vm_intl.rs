//! Focused ECMA-402 namespace and locale-list tests.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsString,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
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
        let body = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Intl>"))
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

fn rendered(body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    context
        .call(&run, &[], ExecutionLimits::default())
        .map_err(|error: ExecutionError| error.to_string())
        .expect("completed")
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn intl_namespace_and_get_canonical_locales_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var d=Object.getOwnPropertyDescriptor(Intl,'getCanonicalLocales');
             return [typeof Intl,Object.prototype.toString.call(Intl),
               Intl.getCanonicalLocales.length,Intl.getCanonicalLocales.name,
               d.writable,d.enumerable,d.configurable,
               Intl.getCanonicalLocales(['DE-de','cmn','de-DE']).join(',')].join('|');"
        ),
        "object|[object Intl]|1|getCanonicalLocales|true|false|true|de-DE,zh"
    );
}

#[test]
fn canonicalize_locale_list_preserves_observable_array_like_order() {
    assert_eq!(
        rendered(
            "var log=[];
             var locales=new Proxy({0:{toString:function(){log.push('toString 0');locales[1]='pt-br';return 'en-us'}},length:2},{
               has:function(target,key){log.push('has '+key);return key in target},
               get:function(target,key){log.push('get '+key);return target[key]}
             });
             var result=Intl.getCanonicalLocales(locales).join(',');
             return result+'|'+log.join(',');"
        ),
        "en-US,pt-BR|get length,has 0,get 0,toString 0,has 1,get 1"
    );
}

#[test]
fn canonicalize_locale_list_boxes_non_strings_and_validates_elements_before_coercion() {
    assert_eq!(
        rendered(
            "Number.prototype[0]='en-us';Number.prototype.length=1;
             var inherited=Intl.getCanonicalLocales(NaN)[0];
             var type,range;
             try{Intl.getCanonicalLocales([2])}catch(e){type=e.name}
             try{Intl.getCanonicalLocales('de_DE')}catch(e){range=e.name}
             return [inherited,type,range].join('|');"
        ),
        "en-US|TypeError|RangeError"
    );
}
