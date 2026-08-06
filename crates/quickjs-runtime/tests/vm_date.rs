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
fn date_local_constructor_converts_left_to_right_and_normalizes_components() {
    assert_eq!(
        rendered(
            "var log=[];function value(label,value){return {valueOf:function(){log.push(label);return value}}}
             var d=new Date(value('year',2020),value('month',12),value('date',1),
               value('hour',2),value('minute',3),value('second',4),value('ms',5));
             return [d.getFullYear(),d.getMonth(),d.getDate(),d.getHours(),d.getMinutes(),
               d.getSeconds(),d.getMilliseconds()].join(',')+'|'+log.join(',')+'|'+
               new Date(99,0,1).getFullYear()+'|'+Number.isNaN(new Date(2020,NaN).getTime());"
        ),
        "2021,0,1,2,3,4,5|year,month,date,hour,minute,second,ms|1999|true"
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
fn date_local_getters_use_one_host_time_zone_projection() {
    assert_eq!(
        rendered(
            "var d=new Date(0),offset=d.getTimezoneOffset();
             var projected=new Date(-offset*60000);
             var actual=[d.getFullYear(),d.getMonth(),d.getDate(),d.getDay(),
               d.getHours(),d.getMinutes(),d.getSeconds(),d.getMilliseconds()];
             var expected=[projected.getUTCFullYear(),projected.getUTCMonth(),
               projected.getUTCDate(),projected.getUTCDay(),projected.getUTCHours(),
               projected.getUTCMinutes(),projected.getUTCSeconds(),
               projected.getUTCMilliseconds()];
             return (actual.join(',')===expected.join(','))+'|'+
               (offset===d.getTimezoneOffset());"
        ),
        "true|true"
    );

    assert_eq!(
        rendered(
            "var d=new Date(NaN),names=['getFullYear','getMonth','getDate','getDay',
               'getHours','getMinutes','getSeconds','getMilliseconds','getTimezoneOffset'];
             for(var i=0;i<names.length;i++){if(!Number.isNaN(d[names[i]]()))return 'false'}
             return 'true';"
        ),
        "true"
    );
    assert_eq!(
        rendered(
            "return [Number.isNaN(new Date(8640000000000000).getTimezoneOffset()),
               Number.isNaN(new Date(-8640000000000000).getTimezoneOffset())].join('|');"
        ),
        "false|false"
    );
    assert_eq!(
        thrown("return Date.prototype.getFullYear.call({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn date_local_string_methods_have_spec_shapes_and_invalid_date_rules() {
    assert_eq!(
        rendered(
            "var d=new Date(0),whole=d.toString(),date=d.toDateString(),time=d.toTimeString();
             return [whole===date+' '+time,
               /^(Sun|Mon|Tue|Wed|Thu|Fri|Sat) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) [0-9]{2} -?[0-9]{4,}$/.test(date),
               /^[0-9]{2}:[0-9]{2}:[0-9]{2} GMT[+-][0-9]{4}$/.test(time),
               new Date(NaN).toString(),new Date(NaN).toDateString(),
               new Date(NaN).toTimeString()].join('|');"
        ),
        "true|true|true|Invalid Date|Invalid Date|Invalid Date"
    );
    assert_eq!(
        thrown("return Date.prototype.toString.call({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn date_local_read_methods_have_exact_intrinsic_names_and_lengths() {
    assert_eq!(
        rendered(
            "var expected={toString:0,toDateString:0,toTimeString:0,getFullYear:0,
               getMonth:0,getDate:0,getDay:0,getHours:0,getMinutes:0,getSeconds:0,
               getMilliseconds:0,getTimezoneOffset:0},actual=[];
             for(var name in expected){var method=Date.prototype[name];
               actual.push(typeof method+':'+method.name+':'+method.length)}
             return actual.join('|');"
        ),
        "function:toString:0|function:toDateString:0|function:toTimeString:0|\
         function:getFullYear:0|function:getMonth:0|function:getDate:0|\
         function:getDay:0|function:getHours:0|function:getMinutes:0|\
         function:getSeconds:0|function:getMilliseconds:0|function:getTimezoneOffset:0"
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

#[test]
fn callable_date_returns_a_local_time_string_without_observing_arguments() {
    let result = rendered(
        "var touched=false;
         var value=Date({valueOf:function(){touched=true;throw new Error('observed')}});
         return typeof value+'|'+touched+'|'+value;",
    );
    let mut parts = result.splitn(3, '|');
    assert_eq!(parts.next(), Some("string"));
    assert_eq!(parts.next(), Some("false"));
    let date = parts.next().expect("Date string");

    let fields = date.split_ascii_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 6, "unexpected Date string: {date}");
    assert!(
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].contains(&fields[0]),
        "unexpected weekday in Date string: {date}"
    );
    assert!(
        [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ]
        .contains(&fields[1]),
        "unexpected month in Date string: {date}"
    );
    assert!(is_ascii_digits(fields[2], 2));
    assert!(is_ascii_digits(fields[3], 4));

    let time = fields[4].as_bytes();
    assert_eq!(time.len(), 8, "unexpected time in Date string: {date}");
    assert_eq!((time[2], time[5]), (b':', b':'));
    assert!(is_ascii_digits(&fields[4][0..2], 2));
    assert!(is_ascii_digits(&fields[4][3..5], 2));
    assert!(is_ascii_digits(&fields[4][6..8], 2));

    let zone = fields[5].as_bytes();
    assert_eq!(zone.len(), 8, "unexpected time zone in Date string: {date}");
    assert_eq!(&zone[..3], b"GMT");
    assert!(matches!(zone[3], b'+' | b'-'));
    assert!(zone[4..].iter().all(u8::is_ascii_digit));
}

fn is_ascii_digits(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit())
}
