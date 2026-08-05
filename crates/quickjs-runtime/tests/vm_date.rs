//! ES2025 UTC/time-value foundation for `%Date%`.

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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Date>"))
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

fn thrown(body: &str) -> ExceptionKind {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected JavaScript exception");
        };
        exception.kind().expect("engine exception kind")
    })
}

#[test]
fn date_utc_core_has_branded_instances_and_exact_intrinsic_descriptors() {
    assert_eq!(
        rendered(
            "var d=new Date(0);return [Date.length,Date.name,
             Object.getPrototypeOf(d)===Date.prototype,
             Object.prototype.toString.call(d),
             Object.getOwnPropertyDescriptor(Date,'UTC').enumerable,
             Object.getOwnPropertyDescriptor(Date.prototype,'getTime').enumerable].join('|');"
        ),
        "7|Date|true|[object Date]|false|false"
    );
}

#[test]
fn date_constructor_clips_numbers_copies_dates_and_preserves_negative_boundaries() {
    assert_eq!(
        rendered(
            "var original=new Date(123.9),copy=new Date(original);
             return [original.getTime(),copy.getTime(),Object.is(new Date(-0).getTime(),0),
              new Date(-1).toISOString(),new Date(8640000000000000).toISOString(),
              new Date(-8640000000000000).toISOString(),
              Number.isNaN(new Date(8640000000000001).getTime())].join('|');"
        ),
        "123|123|true|1969-12-31T23:59:59.999Z|+275760-09-13T00:00:00.000Z|-271821-04-20T00:00:00.000Z|true"
    );
}

#[test]
fn date_utc_converts_arguments_left_to_right_and_normalizes_calendar_fields() {
    assert_eq!(
        rendered(
            "var log=[];function value(label,value){return {valueOf:function(){log.push(label);return value}}}
             var result=Date.UTC(value('year',2020),value('month',12),value('date',1),
               value('hour',2),value('minute',3),value('second',4),value('ms',5));
             return result+'|'+log.join(',')+'|'+Date.UTC(99,0,1)+'|'+String(Date.UTC(NaN,0));"
        ),
        "1609466584005|year,month,date,hour,minute,second,ms|915148800000|NaN"
    );
}

#[test]
fn date_parse_accepts_the_normative_iso_utc_and_offset_forms() {
    assert_eq!(
        rendered(
            "return [Date.parse('1970'),Date.parse('1970-02'),Date.parse('1970-01-01'),
              Date.parse('1970-01-01T00:00:00.000Z'),Date.parse('1970-01-01T01:00:00+01:00'),
              Date.parse('+000000-01-01T00:00:00.000Z'),
              Number.isNaN(Date.parse('-000000-01-01T00:00:00.000Z'))].join('|');"
        ),
        "0|2678400000|0|0|0|-62167219200000|true"
    );
}

#[test]
fn date_utc_getters_and_set_time_preserve_brand_and_invalid_date_rules() {
    assert_eq!(
        rendered(
            "var d=new Date(Date.UTC(2000,1,29,23,58,57,456));
             var fields=[d.getUTCFullYear(),d.getUTCMonth(),d.getUTCDate(),d.getUTCDay(),
               d.getUTCHours(),d.getUTCMinutes(),d.getUTCSeconds(),d.getUTCMilliseconds()];
             var set=d.setTime(-1);return fields.join(',')+'|'+set+'|'+d.toISOString();"
        ),
        "2000,1,29,2,23,58,57,456|-1|1969-12-31T23:59:59.999Z"
    );

    assert_eq!(
        thrown("return Date.prototype.getTime.call({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Date(NaN).toISOString();"),
        ExceptionKind::RangeError
    );
}

#[test]
fn date_set_time_brand_check_precedes_argument_coercion() {
    assert_eq!(
        rendered(
            "var touched=false,arg={valueOf:function(){touched=true;return 0}};
             try{Date.prototype.setTime.call({},arg)}catch(e){return (e instanceof TypeError)+'|'+touched}"
        ),
        "true|false"
    );
}
