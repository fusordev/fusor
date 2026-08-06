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
fn date_parse_uses_local_time_without_an_offset_and_accepts_own_renderings() {
    assert_eq!(
        rendered(
            "var local=new Date(1970,0,1,0,0,0,0),zero=new Date(0);
             return [Date.parse('1970-01-01T00:00:00')===local.getTime(),
               Date.parse('1970-01-01T00:00')===local.getTime(),
               Date.parse('1970-01-01')===0,
               Date.parse(zero.toString())===0,
               Date.parse(zero.toUTCString())===0,
               Date.parse(zero.toISOString())===0].join('|');"
        ),
        "true|true|true|true|true|true"
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
fn date_utc_setters_normalize_components_and_recover_only_full_year() {
    assert_eq!(
        rendered(
            "function set(name,args){var d=new Date(Date.UTC(2000,0,2,3,4,5,6));
               var result=d[name].apply(d,args);return (result===d.getTime())+':'+d.toISOString()}
             var invalid=new Date(NaN),touched=false;
             var invalidDate=invalid.setUTCDate({valueOf:function(){touched=true;return 1}});
             var recovered=new Date(NaN),recoveredValue=recovered.setUTCFullYear(2001);
             return [set('setUTCMilliseconds',[1007]),set('setUTCSeconds',[61,8]),
               set('setUTCMinutes',[61,7,8]),set('setUTCHours',[25,6,7,8]),
               set('setUTCDate',[32]),set('setUTCMonth',[12,3]),
               set('setUTCFullYear',[2001,12,3]),Number.isNaN(invalidDate),touched,
               recoveredValue===recovered.getTime(),recovered.toISOString()].join('|');"
        ),
        "true:2000-01-02T03:04:06.007Z|true:2000-01-02T03:05:01.008Z|\
         true:2000-01-02T04:01:07.008Z|true:2000-01-03T01:06:07.008Z|\
         true:2000-02-01T03:04:05.006Z|true:2001-01-03T03:04:05.006Z|\
         true:2002-01-03T03:04:05.006Z|true|true|true|2001-01-01T00:00:00.000Z"
    );
}

#[test]
fn date_local_setters_preserve_local_fields_and_coerce_left_to_right() {
    assert_eq!(
        rendered(
            "function fields(d){return [d.getFullYear(),d.getMonth(),d.getDate(),d.getHours(),
               d.getMinutes(),d.getSeconds(),d.getMilliseconds()].join(',')}
             function set(name,args){var d=new Date(2000,0,2,3,4,5,6);
               var result=d[name].apply(d,args);return (result===d.getTime())+':'+fields(d)}
             var log=[],d=new Date(0);
             function value(label,value){return {valueOf:function(){log.push(label);return value}}}
             d.setHours(value('h',1),value('m',2),value('s',3),value('ms',4));
             var touched=false;try{Date.prototype.setDate.call({},
               {valueOf:function(){touched=true;return 1}})}catch(error){}
             var recovered=new Date(NaN);recovered.setFullYear(2001);
             return [set('setMilliseconds',[1007]),set('setSeconds',[61,8]),
               set('setMinutes',[61,7,8]),set('setHours',[25,6,7,8]),set('setDate',[32]),
               set('setMonth',[12,3]),set('setFullYear',[2001,12,3]),log.join(','),
               touched,recovered.getFullYear()].join('|');"
        ),
        "true:2000,0,2,3,4,6,7|true:2000,0,2,3,5,1,8|\
         true:2000,0,2,4,1,7,8|true:2000,0,3,1,6,7,8|\
         true:2000,1,1,3,4,5,6|true:2001,0,3,3,4,5,6|\
         true:2002,0,3,3,4,5,6|h,m,s,ms|false|2001"
    );
}

#[test]
fn date_to_primitive_validates_hints_and_uses_the_normative_method_order() {
    assert_eq!(
        rendered(
            "var method=Date.prototype[Symbol.toPrimitive],log=[];
             var receiver={toString:function(){log.push('string');return {}},
               valueOf:function(){log.push('number');return 7}};
             var first=method.call(receiver,'default'),defaultOrder=log.join(',');
             log=[];var second=method.call(receiver,'number'),numberOrder=log.join(',');
             var badHint=false,badThis=false;
             try{method.call(receiver,'invalid')}catch(error){badHint=error instanceof TypeError}
             try{method.call(1,'string')}catch(error){badThis=error instanceof TypeError}
             return [method.name,method.length,first,defaultOrder,second,numberOrder,
               badHint,badThis].join('|');"
        ),
        "[Symbol.toPrimitive]|1|7|string,number|7|number|true|true"
    );
}

#[test]
fn date_to_json_is_generic_and_invokes_to_iso_string_after_number_hint_coercion() {
    assert_eq!(
        rendered(
            "var log=[],receiver,argumentCount;
             var object={
               valueOf:function(){log.push('valueOf');return 1},
               get toISOString(){log.push('get');return function(){log.push('call');
                 receiver=this;argumentCount=arguments.length;return 'ok'}}};
             var result=Date.prototype.toJSON.call(object,'key');
             var nan={valueOf:function(){return NaN},get toISOString(){throw new Error('read')}};
             var primitive={toISOString:function(){return 'symbol'},valueOf:function(){throw 0}};
             primitive[Symbol.toPrimitive]=function(hint){log.push(hint);return Symbol()};
             var symbolResult=Date.prototype.toJSON.call(primitive);
             return [Date.prototype.toJSON.name,Date.prototype.toJSON.length,result,
               log.join(','),receiver===object,argumentCount,
               Date.prototype.toJSON.call(nan)===null,
               symbolResult].join('|');"
        ),
        "toJSON|1|ok|valueOf,get,call,number|true|0|true|symbol"
    );
    assert_eq!(
        thrown("return Date.prototype.toJSON.call(null);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn date_no_intl_locale_fallbacks_ignore_options_and_utc_years_keep_four_digits() {
    assert_eq!(
        rendered(
            "var d=new Date(0),observed=false;
             var ignored={valueOf:function(){observed=true;throw 0}};
             return [d.toLocaleString(ignored,ignored)===d.toString(),
               d.toLocaleDateString(ignored,ignored)===d.toDateString(),
               d.toLocaleTimeString(ignored,ignored)===d.toTimeString(),observed,
               Date.prototype.toLocaleString.length,
               Date.prototype.toLocaleDateString.length,
               Date.prototype.toLocaleTimeString.length,
               new Date(NaN).toLocaleString(),
               new Date('-000001-07-01T00:00Z').toUTCString().split(' ')[3],
               new Date('-000012-07-01T00:00Z').toUTCString().split(' ')[3]].join('|');"
        ),
        "true|true|true|false|0|0|0|Invalid Date|-0001|-0012"
    );
}

#[test]
fn temporal_instant_constructor_exposes_exact_epoch_slots() {
    assert_eq!(
        rendered(
            "var instant=new Temporal.Instant(-217175010123456789n);
             return [typeof Temporal,Temporal.Instant.name,Temporal.Instant.length,
               instant instanceof Temporal.Instant,
               instant.epochMilliseconds,instant.epochNanoseconds,
               Object.prototype.toString.call(instant),instant.toString(),
               instant.toJSON()].join('|');"
        ),
        "object|Instant|1|true|-217175010124|-217175010123456789|[object Temporal.Instant]|1963-02-13T09:36:29.876543211Z|1963-02-13T09:36:29.876543211Z"
    );
    assert_eq!(
        thrown("return Temporal.Instant(0n);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn temporal_instant_epoch_factories_validate_units_and_ignore_subclass_receivers() {
    assert_eq!(
        rendered(
            "var millis=Temporal.Instant.fromEpochMilliseconds(217175010123);
             var nanos=Temporal.Instant.fromEpochNanoseconds(217175010123456789n);
             var borrowed=Temporal.Instant.fromEpochNanoseconds.call({},1n);
             var bad=false;try{Temporal.Instant.fromEpochMilliseconds(1.5)}
               catch(error){bad=error instanceof RangeError}
             return [millis.epochNanoseconds,nanos.epochMilliseconds,
               borrowed instanceof Temporal.Instant,borrowed.epochNanoseconds,bad,
               Temporal.Instant.fromEpochMilliseconds.length,
               Temporal.Instant.fromEpochNanoseconds.length].join('|');"
        ),
        "217175010123000000|217175010123|true|1|true|1|1"
    );
    assert_eq!(
        thrown("return Temporal.Instant.prototype.epochNanoseconds;"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).valueOf();"),
        ExceptionKind::TypeError
    );
}

#[test]
fn temporal_instant_epoch_bounds_are_exact_when_the_shared_helper_cannot_compile() {
    assert_eq!(
        rendered(
            "var max=8640000000000000000000n,min=-max,failures=0;
             var maxInstant=new Temporal.Instant(max),minInstant=new Temporal.Instant(min);
             var maxMillis=Temporal.Instant.fromEpochMilliseconds(8640000000000000);
             var minMillis=Temporal.Instant.fromEpochMilliseconds(-8640000000000000);
             for(var value of [max+1n,min-1n]){
               try{new Temporal.Instant(value)}catch(error){if(error instanceof RangeError)failures++}}
             for(var value of [8640000000000001,-8640000000000001]){
               try{Temporal.Instant.fromEpochMilliseconds(value)}
               catch(error){if(error instanceof RangeError)failures++}}
             return [maxInstant.epochNanoseconds===max,minInstant.epochNanoseconds===min,
               maxMillis.epochNanoseconds===max,minMillis.epochNanoseconds===min,
               failures].join('|');"
        ),
        "true|true|true|true|4"
    );
}

#[test]
fn temporal_instant_from_compare_and_equals_share_spec_ordered_string_conversion() {
    assert_eq!(
        rendered(
            "var original=new Temporal.Instant(1n),copy=Temporal.Instant.from(original),log=[];
             function makeValue(label,text){return {toString:function(){log.push(label);return text}}}
             var comparison=Temporal.Instant.compare(
               makeValue('left','1970-01-01T00:00:00.000000001Z'),
               makeValue('right','1970-01-01T00:00:00.000000002Z'));
             var equal=copy.equals('1970-01-01T00:00:00.000000001Z'),typeErrors=0;
             for(var value of [undefined,null,true,1,1n]){
               try{Temporal.Instant.from(value)}catch(error){if(error instanceof TypeError)typeErrors++}}
             var touched=false;try{Temporal.Instant.prototype.equals.call({},
               {toString:function(){touched=true;return '1970-01-01T00:00Z'}})}catch(error){}
             return [copy!==original,copy.epochNanoseconds,comparison,log.join(','),equal,
               typeErrors,touched,Temporal.Instant.from.length,Temporal.Instant.compare.length,
               Temporal.Instant.prototype.equals.length].join('|');"
        ),
        "true|1|-1|left,right|true|5|false|1|2|1"
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
