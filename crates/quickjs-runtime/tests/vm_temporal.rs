//! Focused JavaScript boundary tests for the shared `temporal_rs` kernel.

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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Temporal>"))
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
fn duration_intrinsic_has_the_exact_initial_topology() {
    assert_eq!(
        rendered(
            "var d=new Temporal.Duration();
             var years=Object.getOwnPropertyDescriptor(Temporal.Duration.prototype,'years');
             return [Temporal.Duration.length,Temporal.Duration.name,
               Object.getPrototypeOf(d)===Temporal.Duration.prototype,
               Object.prototype.toString.call(d),years.enumerable,years.get.name,
               Temporal.Duration.prototype.constructor===Temporal.Duration].join('|');"
        ),
        "0|Duration|true|[object Temporal.Duration]|false|get years|true"
    );
}

#[test]
fn plain_date_intrinsic_constructor_accessors_and_iso_formatting_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var d=new Temporal.PlainDate(2020,12,24,'iso8601');
             var year=Object.getOwnPropertyDescriptor(Temporal.PlainDate.prototype,'year');
             return [Temporal.PlainDate.length,Temporal.PlainDate.name,
               Object.getPrototypeOf(d)===Temporal.PlainDate.prototype,
               Object.prototype.toString.call(d),year.enumerable,year.get.name,
               d.calendarId,d.year,d.month,d.monthCode,d.day,d.dayOfWeek,d.dayOfYear,
               d.weekOfYear,d.yearOfWeek,d.daysInWeek,d.daysInMonth,d.daysInYear,
               d.monthsInYear,d.inLeapYear,d.era,d.eraYear,d.toString(),d.toJSON(),
               d.toLocaleString()].join('|');"
        ),
        "3|PlainDate|true|[object Temporal.PlainDate]|false|get year|iso8601|2020|12|M12|24|4|359|52|2020|7|31|366|12|true|||2020-12-24|2020-12-24|2020-12-24"
    );
}

#[test]
fn plain_date_to_string_reads_calendar_name_resumably() {
    assert_eq!(
        rendered(
            "var date=new Temporal.PlainDate(2020,12,24);
             return [date.toString({calendarName:'always'}),
               date.toString({calendarName:'critical'}),
               date.toString({calendarName:'never'}),date.toJSON()].join('|');"
        ),
        "2020-12-24[u-ca=iso8601]|2020-12-24[!u-ca=iso8601]|2020-12-24|2020-12-24"
    );
    assert_eq!(
        rendered(
            "var log=[];
             var options={get calendarName(){log.push('calendarName');return {toString:function(){log.push('calendarName string');return 'auto'}}}};
             var result=new Temporal.PlainDate(2020,12,24).toString(options);
             return [result,log.join(',')].join('|');"
        ),
        "2020-12-24|calendarName,calendarName string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).toString({calendarName:'invalid'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).toString(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_and_date_time_to_zoned_date_time_preserve_observable_order() {
    assert_eq!(
        rendered(
            "var log=[];
             var date=new Temporal.PlainDate(2020,3,8);
             var item={
               get timeZone(){log.push('timeZone');return 'America/New_York';},
               get plainTime(){log.push('plainTime');return '12:34';}
             };
             var options={get disambiguation(){log.push('disambiguation');return {
               toString:function(){log.push('disambiguation string');return 'later';}}}};
             var dateTime=new Temporal.PlainDateTime(2020,11,1,1,30);
             return [Temporal.PlainDate.prototype.toZonedDateTime.length,
               Temporal.PlainDateTime.prototype.toZonedDateTime.length,
               date.toZonedDateTime('UTC').toString(),
               date.toZonedDateTime(item).toString(),
               date.toZonedDateTime(new Temporal.ZonedDateTime(0n,'UTC')).toString(),
               dateTime.toZonedDateTime('America/New_York',options).toString(),
               log.join(',')].join('|');"
        ),
        "1|1|2020-03-08T00:00:00+00:00[UTC]|2020-03-08T12:34:00-04:00[America/New_York]|2020-03-08T00:00:00+00:00[UTC]|2020-11-01T01:30:00-05:00[America/New_York]|timeZone,plainTime,disambiguation,disambiguation string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).toZonedDateTime(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).toZonedDateTime('UTC',null);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_month_day_converts_property_bags_and_preserves_observable_boundaries() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name);return value}}}
             function string(name,value){return {toString:function(){log.push(name);return value}}}
             var value=new Temporal.PlainMonthDay(2,29,'iso8601',2020);
             var from=Temporal.PlainMonthDay.from({month:13,day:34},{overflow:'constrain'});
             var changed=value.with({day:number('day',31),monthCode:string('monthCode','M12')},{get overflow(){log.push('overflow');return string('overflow string','constrain')}});
             var date=value.toPlainDate({year:number('year',2021)});
             return [Temporal.PlainMonthDay.length,Temporal.PlainMonthDay.name,
               Object.prototype.toString.call(value),value.calendarId,value.monthCode,value.day,
               value.toString({calendarName:'always'}),from.toString(),changed.toString(),
               date.toString(),log.join(',')].join('|');"
        ),
        "2|PlainMonthDay|[object Temporal.PlainMonthDay]|iso8601|M02|29|2020-02-29[u-ca=iso8601]|12-31|12-31|2021-02-28|day,monthCode,overflow,overflow string,year"
    );
    assert_eq!(
        rendered(
            "var values=['','1997-12-04[u-ca=iso8601]','notacal','11111111','1111-11-11'];
             var result=values.map(function(calendar){try{new Temporal.PlainMonthDay(12,15,calendar,1972)}catch(error){return error.name}});
             [Infinity,-Infinity].forEach(function(value){try{new Temporal.PlainMonthDay(value,1)}catch(error){result.push(error.name)}});
             return result.join('|');"
        ),
        "RangeError|RangeError|RangeError|RangeError|RangeError|RangeError|RangeError"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function O(value,name){return function(){return {valueOf:function(){log.push(name);return value}}}}
             var args=[O(Infinity,'month'),O(1,'day'),function(){return 'iso8601'},O(1,'year')];
             var values=args.map(function(factory){return factory()});
             try{new Temporal.PlainMonthDay(...values)}catch(error){return error.name+'|'+log.join(',')}",
        ),
        "RangeError|month"
    );
    assert_eq!(
        thrown("return new Temporal.PlainMonthDay(2,29).toPlainDate({year:Infinity});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainMonthDay(5,2).with({day:-1}, null);"),
        ExceptionKind::RangeError
    );
}

#[test]
fn plain_year_month_converts_property_bags_and_preserves_observable_boundaries() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name);return value}}}
             function string(name,value){return {toString:function(){log.push(name);return value}}}
             var value=new Temporal.PlainYearMonth(2020,12);
             var year=Object.getOwnPropertyDescriptor(Temporal.PlainYearMonth.prototype,'year');
             var from=Temporal.PlainYearMonth.from({
               get calendar(){log.push('calendar');return 'iso8601'},
               get month(){log.push('month');return number('month number',13)},
               get monthCode(){log.push('monthCode');return string('monthCode string','M12')},
               get year(){log.push('year');return number('year number',2021)}
             },{get overflow(){log.push('from overflow');return string('from overflow string','constrain')}});
             var changed=value.with({
               get calendar(){log.push('with calendar');return undefined},
               get timeZone(){log.push('with timeZone');return undefined},
               get month(){log.push('with month');return number('with month number',2)},
               get monthCode(){log.push('with monthCode');return undefined},
               get year(){log.push('with year');return undefined}
             },{get overflow(){log.push('with overflow');return string('with overflow string','constrain')}});
             var date=value.toPlainDate({day:number('day',29)});
             var added=value.add({months:number('add months',2)});
             var subtracted=value.subtract({years:1});
             var until=value.until(new Temporal.PlainYearMonth(2022,3));
             var since=value.since(new Temporal.PlainYearMonth(2022,3));
             return [Temporal.PlainYearMonth.length,Temporal.PlainYearMonth.name,
               Object.getPrototypeOf(value)===Temporal.PlainYearMonth.prototype,
               Object.prototype.toString.call(value),year.enumerable,year.get.name,
               value.calendarId,value.year,value.month,value.monthCode,value.daysInMonth,
               value.daysInYear,value.monthsInYear,value.inLeapYear,value.era,value.eraYear,
               value.toString({calendarName:'always'}),value.toJSON(),value.toLocaleString(),
               from.toString(),changed.toString(),date.toString(),added.toString(),
               subtracted.toString(),until.toString(),since.toString(),
               Temporal.PlainYearMonth.compare(value,from),value.equals(from),log.join(',')].join('|');"
        ),
        "2|PlainYearMonth|true|[object Temporal.PlainYearMonth]|false|get year|iso8601|2020|12|M12|31|366|12|true|||2020-12-01[u-ca=iso8601]|2020-12|2020-12|2021-12|2020-02|2020-12-29|2021-02|2019-12|P1Y3M|-P1Y3M|-1|false|calendar,month,month number,monthCode,monthCode string,year,year number,from overflow,from overflow string,with calendar,with timeZone,with month,with month number,with monthCode,with year,with overflow,with overflow string,day,add months"
    );
    assert_eq!(
        thrown("return new Temporal.PlainYearMonth(2020,12).with({calendar:'iso8601'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainYearMonth(2020,12).toPlainDate({day:Infinity});"),
        ExceptionKind::RangeError
    );
}

#[test]
fn zoned_date_time_constructor_and_string_from_preserve_branded_slots() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             var from=Temporal.ZonedDateTime.from('2019-05-17T12:34Z[UTC]');
             return [Temporal.ZonedDateTime.length,Temporal.ZonedDateTime.name,
               Object.getPrototypeOf(value)===Temporal.ZonedDateTime.prototype,
               Object.getPrototypeOf(from)===Temporal.ZonedDateTime.prototype,
               Object.prototype.toString.call(value),Object.prototype.toString.call(from),
               typeof Temporal.ZonedDateTime.from].join('|');"
        ),
        "2|ZonedDateTime|true|true|[object Temporal.ZonedDateTime]|[object Temporal.ZonedDateTime]|function"
    );
    assert_eq!(
        thrown("return Temporal.ZonedDateTime(0n,'UTC');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0,'UTC');"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_constructor_rejects_non_identifier_time_zone_and_calendar() {
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,1n);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,null);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,Symbol());"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'1997-12-04T12:34[+01:00]','iso8601');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC',1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC',{});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC',new Temporal.Duration());"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC','notacal');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'uTc','iSo8601');
             var fromZone=new Temporal.ZonedDateTime(0n,value,'iso8601');
             return [value.timeZoneId,value.calendarId,fromZone.timeZoneId].join('|');"
        ),
        "UTC|iso8601|UTC"
    );
}

#[test]
fn zoned_date_time_accessors_expose_kernel_slots_and_getter_descriptors() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             var calendar=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'calendarId');
             return [value.calendarId,value.timeZoneId,value.year,value.month,value.monthCode,
               value.day,value.hour,value.minute,value.second,value.millisecond,value.microsecond,
               value.nanosecond,calendar.enumerable,calendar.get.name].join('|');"
        ),
        "iso8601|UTC|1970|1|M01|1|0|0|0|0|0|0|false|get calendarId"
    );
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             return [value.offset,value.offsetNanoseconds,value.dayOfWeek,value.dayOfYear,
               value.weekOfYear,value.yearOfWeek,value.daysInWeek,value.daysInMonth,
               value.daysInYear,value.monthsInYear,value.inLeapYear].join('|');"
        ),
        "+00:00|0|4|1|1|1970|7|31|365|12|false"
    );
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             return [value.era,value.eraYear,value.epochMilliseconds,value.epochNanoseconds,
               value.hoursInDay].join('|');"
        ),
        "||0|0|24"
    );
    assert_eq!(
        thrown(
            "return Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'year').get.call({});"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_projection_methods_preserve_branded_temporal_values() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(3600000000000n,'UTC','iso8601');
             var start=value.startOfDay();
             var instant=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'toInstant');
             return [value.toInstant().toString(),value.toPlainDate().toString(),
               value.toPlainTime().toString(),value.toPlainDateTime().toString(),
               start.epochNanoseconds,start.timeZoneId,start.calendarId,
               instant.value.length,instant.value.name,instant.enumerable,
               instant.writable,instant.configurable].join('|');"
        ),
        "1970-01-01T01:00:00Z|1970-01-01|01:00:00|1970-01-01T01:00:00|0|UTC|iso8601|0|toInstant|false|true|true"
    );
    assert_eq!(
        thrown("return Temporal.ZonedDateTime.prototype.toPlainDate.call({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_equals_accepts_branded_values_and_strings() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             var equals=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'equals');
             return [value.equals(new Temporal.ZonedDateTime(0n,'UTC','iso8601')),
               value.equals(new Temporal.ZonedDateTime(1n,'UTC','iso8601')),
               value.equals('1970-01-01T00:00+00:00[UTC][u-ca=iso8601]'),
               equals.value.length,equals.value.name,equals.enumerable,equals.writable,
               equals.configurable].join('|');"
        ),
        "true|false|true|1|equals|false|true|true"
    );
    assert_eq!(
        thrown("return Temporal.ZonedDateTime.prototype.equals.call({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_compare_accepts_branded_values_and_strings() {
    assert_eq!(
        rendered(
            "var first=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             var compare=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime,'compare');
             return [Temporal.ZonedDateTime.compare(first,new Temporal.ZonedDateTime(1n,'UTC')),
               Temporal.ZonedDateTime.compare('1970-01-01T00:00+00:00[UTC]',first),
               compare.value.length,compare.value.name,compare.enumerable,compare.writable,
               compare.configurable].join('|');"
        ),
        "-1|0|2|compare|false|true|true"
    );
    assert_eq!(
        thrown("return Temporal.ZonedDateTime.compare({},{});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_property_bags_and_from_options_preserve_observable_order() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name);return value}}}
             function string(name,value){return {toString:function(){log.push(name);return value}}}
             var fields={
               get calendar(){log.push('calendar');return 'iso8601'},
               get day(){log.push('day');return number('day number',31.8)},
               get hour(){log.push('hour');return number('hour number',23.8)},
               get microsecond(){log.push('microsecond');return number('microsecond number',8.8)},
               get millisecond(){log.push('millisecond');return number('millisecond number',7.8)},
               get minute(){log.push('minute');return number('minute number',59.8)},
               get month(){log.push('month');return number('month number',2.8)},
               get monthCode(){log.push('monthCode');return string('monthCode string','M02')},
               get nanosecond(){log.push('nanosecond');return number('nanosecond number',9.8)},
               get offset(){log.push('offset');return string('offset string','+00:00')},
               get second(){log.push('second');return number('second number',58.8)},
               get timeZone(){log.push('timeZone');return 'UTC'},
               get year(){log.push('year');return number('year number',2020.8)}
             };
             var options={
               get disambiguation(){log.push('disambiguation');return string('disambiguation string','compatible')},
               get offset(){log.push('option offset');return string('option offset string','reject')},
               get overflow(){log.push('overflow');return string('overflow string','constrain')}
             };
             var from=Temporal.ZonedDateTime.from(fields,options);
             var compare=Temporal.ZonedDateTime.compare(fields,{year:2021,month:1,day:1,timeZone:'UTC'});
             var equal=from.equals({year:2020,month:2,day:29,hour:23,minute:59,second:58,millisecond:7,microsecond:8,nanosecond:9,offset:'+00:00',timeZone:'UTC'});
             return [from.toString(),compare,equal,log.join(',')].join('|');"
        ),
        "2020-02-29T23:59:58.007008009+00:00[UTC]|-1|true|calendar,day,day number,hour,hour number,microsecond,microsecond number,millisecond,millisecond number,minute,minute number,month,month number,monthCode,monthCode string,nanosecond,nanosecond number,offset,offset string,second,second number,timeZone,year,year number,disambiguation,disambiguation string,option offset,option offset string,overflow,overflow string,calendar,day,day number,hour,hour number,microsecond,microsecond number,millisecond,millisecond number,minute,minute number,month,month number,monthCode,monthCode string,nanosecond,nanosecond number,offset,offset string,second,second number,timeZone,year,year number"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function option(name,value){return {get toString(){log.push('get '+name);return function(){log.push('call '+name);return value}}}}
             var options={
               get disambiguation(){log.push('disambiguation');return option('disambiguation','compatible')},
               get offset(){log.push('offset');return option('offset','prefer')},
               get overflow(){log.push('overflow');return option('overflow','constrain')}
             };
             try{Temporal.ZonedDateTime.from({year:2025,monthCode:'M08L',day:14,timeZone:'UTC'},options)}catch(error){return [error.name,log.join(',')].join('|')}"
        ),
        "RangeError|disambiguation,get disambiguation,call disambiguation,offset,get offset,call offset,overflow,get overflow,call overflow"
    );
    assert_eq!(
        rendered(
            "var zdt=new Temporal.ZonedDateTime(0n,'UTC');
             var temporalCalendar=new Temporal.PlainDate(2000,5,2);
             var from=Temporal.ZonedDateTime.from({year:2000,month:5,day:2,timeZone:zdt,calendar:temporalCalendar});
             return [from.timeZoneId,from.calendarId].join('|');"
        ),
        "UTC|iso8601"
    );
    assert_eq!(
        thrown(
            "return Temporal.ZonedDateTime.from(new Temporal.ZonedDateTime(0n,'UTC'),{offset:Symbol()});"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_json_and_non_intl_locale_rendering_are_ixdtf() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(0n,'UTC','iso8601');
             var json=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'toJSON');
             return [value.toJSON(),value.toLocaleString(),json.value.length,json.value.name,
               json.enumerable,json.writable,json.configurable].join('|');"
        ),
        "1970-01-01T00:00:00+00:00[UTC]|1970-01-01T00:00:00+00:00[UTC]|0|toJSON|false|true|true"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').valueOf();"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_to_string_reads_all_options_before_formatting() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             function option(name,value){Object.defineProperty(options,name,{get:function(){
               log.push('get '+name);return {toString:function(){log.push('string '+name);return value;}};
             }});}
             option('calendarName','always');option('fractionalSecondDigits','auto');
             option('offset','never');option('roundingMode','halfExpand');
             option('smallestUnit','millisecond');option('timeZoneName','critical');
             var value=new Temporal.ZonedDateTime(3661987654321n,'UTC');
             var descriptor=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'toString');
             return [value.toString(options),descriptor.value.length,descriptor.value.name,
               descriptor.enumerable,descriptor.writable,descriptor.configurable,log.join(',')].join('|');"
        ),
        "1970-01-01T01:01:01.988[!UTC][u-ca=iso8601]|0|toString|false|true|true|get calendarName,string calendarName,get fractionalSecondDigits,string fractionalSecondDigits,get offset,string offset,get roundingMode,string roundingMode,get smallestUnit,string smallestUnit,get timeZoneName,string timeZoneName"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').toString({smallestUnit:'hour'});"),
        ExceptionKind::RangeError
    );
}

#[test]
fn zoned_date_time_with_time_zone_preserves_the_instant() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(3661987654321n,'UTC');
             var result=value.withTimeZone('+01:00');
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'withTimeZone');
             return [result.epochNanoseconds,result.timeZoneId,result.toPlainDateTime().toString(),
               result===value,method.value.length,method.value.name,method.enumerable,
               method.writable,method.configurable].join('|');"
        ),
        "3661987654321|+01:00|1970-01-01T02:01:01.987654321|false|1|withTimeZone|false|true|true"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withTimeZone('');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withTimeZone({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_with_calendar_replaces_only_the_calendar_slot() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(3661987654321n,'UTC');
             var result=value.withCalendar('gregory');
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'withCalendar');
             return [result.epochNanoseconds,result.timeZoneId,result.calendarId,result.year,
               result===value,method.value.length,method.value.name,method.enumerable,
               method.writable,method.configurable].join('|');"
        ),
        "3661987654321|UTC|gregory|1970|false|1|withCalendar|false|true|true"
    );
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(3661987654321n,'UTC','gregory');
             return [value.withCalendar('2020-01-01[u-ca=iso8601]').calendarId,
               value.withCalendar(new Temporal.PlainDate(2000,5,2,'japanese')).calendarId,
               value.withCalendar(new Temporal.PlainMonthDay(5,2,'hebrew')).calendarId,
               value.withCalendar(value).calendarId].join('|');"
        ),
        "iso8601|japanese|hebrew|gregory"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withCalendar();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withCalendar(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withCalendar({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').withCalendar(new Temporal.Duration());"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').withCalendar('1997-12-04[u-ca=notacal]');"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'withCalendar').value.call({},'iso8601');"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_with_merges_partial_fields_and_observes_option_order() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(3661987654321n,'UTC');
             var result=value.with({year:2019,month:5,day:4,hour:3,minute:2,second:1});
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'with');
             return [result.toString(),result.epochNanoseconds,result===value,
               method.value.length,method.value.name,method.enumerable,
               method.writable,method.configurable].join('|');"
        ),
        "2019-05-04T03:02:01.987654321+00:00[UTC]|1556938921987654321|false|1|with|false|true|true"
    );
    assert_eq!(
        rendered(
            "var log=[],fields={},options={};
             Object.defineProperty(fields,'calendar',{get:function(){log.push('get calendar');return undefined;}});
             Object.defineProperty(fields,'timeZone',{get:function(){log.push('get timeZone');return undefined;}});
             Object.defineProperty(fields,'month',{get:function(){log.push('get month');return 5;}});
             Object.defineProperty(options,'overflow',{get:function(){log.push('get overflow');return 'constrain';}});
             var result=new Temporal.ZonedDateTime(0n,'UTC').with(fields,options);
             return [result.toString(),log.join(',')].join('|');"
        ),
        "1970-05-01T00:00:00+00:00[UTC]|get calendar,get timeZone,get month,get overflow"
    );
    assert_eq!(
        rendered(
            "var value=new Temporal.PlainDateTime(1976,11,18,15,23,30,123,456,789).toZonedDateTime('UTC');
             return [value.with({month:5}).toPlainDateTime().toString(),
               value.with({day:31}).toPlainDateTime().toString(),
               value.with({hour:29},{overflow:'constrain'}).toPlainDateTime().toString()].join('|');"
        ),
        "1976-05-18T15:23:30.123456789|1976-11-30T15:23:30.123456789|1976-11-18T23:23:30.123456789"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with('1976-11-18');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({month:2,calendar:'iso8601'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({month:2,timeZone:'UTC'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').with(new Temporal.PlainDate(1976,11,18));"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({month:5,monthCode:'M06'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({hour:Infinity});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').with({month:2,day:31},{overflow:'reject'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({offset:0});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({offset:'00:00'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').with({hour:2},{disambiguation:'balance'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').with({hour:2},null);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_round_uses_time_zone_aware_day_rounding() {
    assert_eq!(
        rendered(
            "var value=new Temporal.ZonedDateTime(3661987654321n,'UTC');
             var result=value.round({smallestUnit:'minute'});
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'round');
             return [result.toString(),result===value,method.value.length,method.value.name,
               method.enumerable,method.writable,method.configurable].join('|');"
        ),
        "1970-01-01T01:01:00+00:00[UTC]|false|1|round|false|true|true"
    );
    assert_eq!(
        rendered(
            "var berlin=new Temporal.ZonedDateTime(1743294600000000000n,'Europe/Berlin');
             return [berlin.toString(),berlin.round('day').toString(),
               berlin.round({smallestUnit:'day',roundingMode:'ceil'}).toString(),
               berlin.round({smallestUnit:'hour'}).toString()].join('|');"
        ),
        "2025-03-30T01:30:00+01:00[Europe/Berlin]|2025-03-30T00:00:00+01:00[Europe/Berlin]|2025-03-31T00:00:00+02:00[Europe/Berlin]|2025-03-30T03:00:00+02:00[Europe/Berlin]"
    );
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperty(options,'roundingIncrement',{get:function(){log.push('get roundingIncrement');return 1;}});
             Object.defineProperty(options,'roundingMode',{get:function(){log.push('get roundingMode');return 'halfExpand';}});
             Object.defineProperty(options,'smallestUnit',{get:function(){log.push('get smallestUnit');return 'second';}});
             new Temporal.ZonedDateTime(0n,'UTC').round(options);
             return log.join(',');"
        ),
        "get roundingIncrement,get roundingMode,get smallestUnit"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').round();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').round(5);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').round({});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').round({smallestUnit:'year'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').round({smallestUnit:'fortnight'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').round({smallestUnit:'hour',roundingIncrement:25});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').round({smallestUnit:'day',roundingIncrement:2});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').round({smallestUnit:'hour',roundingIncrement:NaN});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').round({smallestUnit:'hour',roundingMode:'balance'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'round').value.call({},'hour');"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_until_since_difference_across_dst_and_options() {
    assert_eq!(
        rendered(
            "var a=new Temporal.PlainDateTime(2025,3,8,12,0).toZonedDateTime('America/New_York');
             var b=new Temporal.PlainDateTime(2025,3,10,12,0).toZonedDateTime('America/New_York');
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'until');
             return [a.until(b).toString(),a.until(b,{largestUnit:'day'}).toString(),
               a.since(b,{largestUnit:'day'}).toString(),a.until(a).toString(),
               method.value.length,method.value.name,method.enumerable,
               method.writable,method.configurable].join('|');"
        ),
        "PT47H|P2D|-P2D|PT0S|1|until|false|true|true"
    );
    assert_eq!(
        rendered(
            "var a=new Temporal.PlainDateTime(2025,3,8,12,30).toZonedDateTime('America/New_York');
             var b=new Temporal.PlainDateTime(2025,3,10,12,0).toZonedDateTime('America/New_York');
             return [a.until(b,{largestUnit:'day'}).toString(),
               a.until(b,{smallestUnit:'hour',roundingMode:'halfExpand'}).toString(),
               a.until(b,{largestUnit:'days',smallestUnit:'minutes'}).toString(),
               a.until('2025-03-10T12:00:00-04:00[America/New_York]',{largestUnit:'day'}).toString(),
               a.until({year:2025,month:3,day:10,hour:12,timeZone:'America/New_York'},{largestUnit:'day'}).toString()].join('|');"
        ),
        "P1DT23H30M|PT47H|P1DT23H30M|P1DT23H30M|P1DT23H30M"
    );
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperty(options,'largestUnit',{get:function(){log.push('get largestUnit');return 'hour';}});
             Object.defineProperty(options,'roundingIncrement',{get:function(){log.push('get roundingIncrement');return 1;}});
             Object.defineProperty(options,'roundingMode',{get:function(){log.push('get roundingMode');return 'trunc';}});
             Object.defineProperty(options,'smallestUnit',{get:function(){log.push('get smallestUnit');return 'minute';}});
             var a=new Temporal.ZonedDateTime(0n,'UTC');
             a.until(new Temporal.ZonedDateTime(3661000000000n,'UTC'),options).toString();
             return log.join(',');"
        ),
        "get largestUnit,get roundingIncrement,get roundingMode,get smallestUnit"
    );
    assert_eq!(
        rendered(
            "var relativeTo=new Temporal.ZonedDateTime(1546300800000000000n,'UTC');
             return [new Temporal.Duration(1,6).total({unit:'months',relativeTo}),
               Temporal.Duration.compare(new Temporal.Duration(0,13),new Temporal.Duration(1),{relativeTo:relativeTo.toPlainDate()})].join('|');"
        ),
        "18|1"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').until(5);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').until(new Temporal.ZonedDateTime(0n,'UTC'),null);"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').until(new Temporal.ZonedDateTime(0n,'UTC','gregory'));"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').until(new Temporal.ZonedDateTime(0n,'Europe/Berlin'),{largestUnit:'day'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').until(new Temporal.ZonedDateTime(0n,'UTC'),{largestUnit:'hour',smallestUnit:'day'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').until(new Temporal.ZonedDateTime(0n,'UTC'),{largestUnit:'fortnight'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').until(new Temporal.ZonedDateTime(0n,'UTC'),{roundingIncrement:NaN});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').since(new Temporal.ZonedDateTime(0n,'UTC'),{roundingMode:'balance'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'since').value.call({},new Temporal.ZonedDateTime(0n,'UTC'));"
        ),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_transition_accepts_string_and_resumable_options_forms() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperty(options,'direction',{get:function(){
               log.push('get direction');return {toString:function(){log.push('string direction');return 'previous';}};
             }});
             var fixed=new Temporal.ZonedDateTime(0n,'+01:00');
             var berlin=new Temporal.ZonedDateTime(1616893200000000000n,'Europe/Berlin');
             var previous=berlin.getTimeZoneTransition(options);
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'getTimeZoneTransition');
             return [fixed.getTimeZoneTransition('next'),fixed.getTimeZoneTransition('previous'),
               previous.toString(),method.value.length,method.value.name,method.enumerable,
               method.writable,method.configurable,log.join(',')].join('|');"
        ),
        "||2020-10-25T02:00:00+01:00[Europe/Berlin]|1|getTimeZoneTransition|false|true|true|get direction,string direction"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').getTimeZoneTransition();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').getTimeZoneTransition({});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').getTimeZoneTransition(false);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.ZonedDateTime(0n,'UTC').getTimeZoneTransition({direction:false});"
        ),
        ExceptionKind::RangeError
    );
}

#[test]
fn zoned_date_time_with_plain_time_uses_shared_temporal_time_conversion() {
    assert_eq!(
        rendered(
            "var log=[],fields={};
             function field(name,value){Object.defineProperty(fields,name,{get:function(){
               log.push('get '+name);return {valueOf:function(){log.push('number '+name);return value;}};
             }});}
             field('hour',2);field('minute',30);field('second',6);field('millisecond',5);
             field('microsecond',4);field('nanosecond',3);
             var value=new Temporal.ZonedDateTime(1000000000000000000n,'UTC');
             var bag=value.withPlainTime(fields);
             var string=value.withPlainTime('12:34:56.987654321');
             var defaulted=value.withPlainTime();
             var source=new Temporal.ZonedDateTime(3661001001001n,'-00:02');
             var local=new Temporal.ZonedDateTime(86400000000000n,'UTC').withPlainTime(source);
             var method=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'withPlainTime');
             return [bag.toPlainTime().toString(),string.toPlainTime().toString(),
               defaulted.toPlainTime().toString(),local.hour,local.minute,method.value.length,
               method.value.name,method.enumerable,method.writable,method.configurable,
               log.join(',')].join('|');"
        ),
        "02:30:06.005004003|12:34:56.987654321|00:00:00|0|59|0|withPlainTime|false|true|true|get hour,number hour,get microsecond,number microsecond,get millisecond,number millisecond,get minute,number minute,get nanosecond,number nanosecond,get second,number second"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withPlainTime({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').withPlainTime(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn zoned_date_time_arithmetic_uses_resumable_duration_and_overflow_options() {
    assert_eq!(
        rendered(
            "var log=[],duration={},options={};
             function field(name,value){Object.defineProperty(duration,name,{get:function(){
               log.push('get '+name);return {valueOf:function(){log.push('number '+name);return value;}};
             }});}
             field('days',1);field('hours',2);field('microseconds',0);field('milliseconds',0);
             field('minutes',3);field('months',0);field('nanoseconds',0);field('seconds',0);
             field('weeks',0);field('years',0);
             Object.defineProperty(options,'overflow',{get:function(){log.push('get overflow');return {
               toString:function(){log.push('string overflow');return 'constrain';}
             };}});
             var value=new Temporal.ZonedDateTime(0n,'UTC');
             var added=value.add(duration,options),subtracted=added.subtract('PT2H3M');
             var monthEnd=new Temporal.ZonedDateTime(2592000000000000n,'UTC').add({months:1});
             var add=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'add');
             var subtract=Object.getOwnPropertyDescriptor(Temporal.ZonedDateTime.prototype,'subtract');
             return [added.toPlainDateTime().toString(),subtracted.toPlainDateTime().toString(),
               monthEnd.toPlainDateTime().toString(),add.value.length,add.value.name,
               subtract.value.length,subtract.value.name,add.enumerable,add.writable,add.configurable,
               log.join(',')].join('|');"
        ),
        "1970-01-02T02:03:00|1970-01-02T00:00:00|1970-02-28T00:00:00|1|add|1|subtract|false|true|true|get days,number days,get hours,number hours,get microseconds,number microseconds,get milliseconds,number milliseconds,get minutes,number minutes,get months,number months,get nanoseconds,number nanoseconds,get seconds,number seconds,get weeks,number weeks,get years,number years,get overflow,string overflow"
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').add({days:1},null);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.ZonedDateTime(0n,'UTC').subtract(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_time_constructor_accessors_and_iso_formatting_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var d=new Temporal.PlainDateTime(2020,12,24,12,34,56,7,8,9,'iso8601');
             return [d.calendarId,d.year,d.month,d.monthCode,d.day,d.hour,d.minute,d.second,
               d.millisecond,d.microsecond,d.nanosecond,d.toString()].join('|');"
        ),
        "iso8601|2020|12|M12|24|12|34|56|7|8|9|2020-12-24T12:34:56.007008009"
    );
    assert_eq!(
        rendered(
            "var d=new Temporal.PlainDateTime(2020,12,24);
             var hour=Object.getOwnPropertyDescriptor(Temporal.PlainDateTime.prototype,'hour');
             return [Temporal.PlainDateTime.length,Temporal.PlainDateTime.name,
               Object.getPrototypeOf(d)===Temporal.PlainDateTime.prototype,
               Object.prototype.toString.call(d),hour.enumerable,hour.get.name].join('|');"
        ),
        "3|PlainDateTime|true|[object Temporal.PlainDateTime]|false|get hour"
    );
    assert_eq!(
        rendered(
            "var d=new Temporal.PlainDateTime(2020,12,24,12,34,56,7,8,9);
             return [d.dayOfWeek,d.dayOfYear,d.weekOfYear,d.yearOfWeek,d.daysInWeek,
               d.daysInMonth,d.daysInYear,d.monthsInYear,d.inLeapYear,d.era,d.eraYear,
               d.toJSON(),d.toLocaleString()].join('|');"
        ),
        "4|359|52|2020|7|31|366|12|true|||2020-12-24T12:34:56.007008009|2020-12-24T12:34:56.007008009"
    );
    assert_eq!(
        rendered("return new Temporal.PlainDateTime(2020,2,29).toString();"),
        "2020-02-29T00:00:00"
    );
}

#[test]
fn plain_date_time_to_string_formats_with_resumable_options() {
    assert_eq!(
        rendered(
            "var dateTime=new Temporal.PlainDateTime(2020,12,24,12,34,56,987,654,321);
             return [dateTime.toString({calendarName:'always'}),
               dateTime.toString({fractionalSecondDigits:2}),
               dateTime.toString({smallestUnit:'minute',fractionalSecondDigits:5}),
               dateTime.toString({smallestUnit:'second',roundingMode:'ceil'}),
               dateTime.toJSON()].join('|');"
        ),
        "2020-12-24T12:34:56.987654321[u-ca=iso8601]|2020-12-24T12:34:56.98|2020-12-24T12:34|2020-12-24T12:34:57|2020-12-24T12:34:56.987654321"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function string(name,value){return {toString:function(){log.push(name+' string');return value}}}
             var options={
               get calendarName(){log.push('calendarName');return string('calendarName','auto')},
               get fractionalSecondDigits(){log.push('fractionalSecondDigits');return string('fractionalSecondDigits','auto')},
               get roundingMode(){log.push('roundingMode');return string('roundingMode','halfExpand')},
               get smallestUnit(){log.push('smallestUnit');return string('smallestUnit','millisecond')}
             };
             var result=new Temporal.PlainDateTime(2020,12,24,12,34,56,987,654,321).toString(options);
             return [result,log.join(',')].join('|');"
        ),
        "2020-12-24T12:34:56.988|calendarName,calendarName string,fractionalSecondDigits,fractionalSecondDigits string,roundingMode,roundingMode string,smallestUnit,smallestUnit string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).toString({calendarName:'invalid'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).toString({smallestUnit:'hour'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).toString(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_time_constructor_conversion_and_accessors_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var time=new Temporal.PlainTime(12,34,56,7,8,9);
             var hour=Object.getOwnPropertyDescriptor(Temporal.PlainTime.prototype,'hour');
             var from=Temporal.PlainTime.from({hour:25,minute:70,microsecond:1000});
             var parsed=Temporal.PlainTime.from('01:02:03.004005006');
             return [Temporal.PlainTime.length,Temporal.PlainTime.name,
               Object.getPrototypeOf(time)===Temporal.PlainTime.prototype,
               Object.prototype.toString.call(time),hour.enumerable,hour.get.name,
               time.hour,time.minute,time.second,time.millisecond,time.microsecond,time.nanosecond,
               time.toString(),time.toJSON(),time.toLocaleString(),
               Temporal.PlainTime.from.name,Temporal.PlainTime.from.length,
               Temporal.PlainTime.compare.name,Temporal.PlainTime.compare.length,
               from.toString(),parsed.toString(),Temporal.PlainTime.compare(time,parsed)].join('|');"
        ),
        "1|PlainTime|true|[object Temporal.PlainTime]|false|get hour|12|34|56|7|8|9|12:34:56.007008009|12:34:56.007008009|12:34:56.007008009|from|1|compare|2|23:59:00.000999|01:02:03.004005006|1"
    );
    assert_eq!(
        rendered(
            "var log=[];
             var fields={
               get hour(){log.push('hour');return 1.7},
               get microsecond(){log.push('microsecond');return 2.7},
               get millisecond(){log.push('millisecond');return 3.7},
               get minute(){log.push('minute');return 4.7},
               get nanosecond(){log.push('nanosecond');return 5.7},
               get second(){log.push('second');return 6.7}
             };
             var time=Temporal.PlainTime.from(fields);
             return [time.toString(),log.join(',')].join('|');"
        ),
        "01:04:06.003002005|hour,microsecond,millisecond,minute,nanosecond,second"
    );
    assert_eq!(
        thrown("return Temporal.PlainTime(12);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime(24);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        rendered("return new Temporal.PlainTime().toString();"),
        "00:00:00"
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime(undefined);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.PlainTime.from({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainTime.prototype.hour.call({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime(1).valueOf();"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_time_add_and_subtract_convert_duration_like_and_ignore_date_units() {
    assert_eq!(
        rendered(
            "var log=[];
             var duration={
               get years(){log.push('years');return undefined},
               get months(){log.push('months');return undefined},
               get weeks(){log.push('weeks');return undefined},
               get days(){log.push('days');return 1},
               get hours(){log.push('hours');return {valueOf:function(){log.push('hours number');return 2}}},
               get minutes(){log.push('minutes');return 35},
               get seconds(){log.push('seconds');return undefined},
               get milliseconds(){log.push('milliseconds');return undefined},
               get microseconds(){log.push('microseconds');return undefined},
               get nanoseconds(){log.push('nanoseconds');return undefined}
             };
             var start=new Temporal.PlainTime(23,30);
             var added=start.add(duration);
             var subtracted=added.subtract('PT2H35M');
             return [Temporal.PlainTime.prototype.add.length,
               Temporal.PlainTime.prototype.subtract.length,start.toString(),added.toString(),
               subtracted.toString(),added===start,log.join(',')].join('|');"
        ),
        "1|1|23:30:00|02:05:00|23:30:00|false|days,hours,hours number,microseconds,milliseconds,minutes,months,nanoseconds,seconds,weeks,years"
    );
    assert_eq!(
        rendered(
            "var time=new Temporal.PlainTime(12,34,56,7,8,9);
             return [time.add(new Temporal.Duration(2,3,4,5,6,7,8,9,10,11)).toString(),
               time.subtract({days:1}).toString()].join('|');"
        ),
        "18:42:04.01601802|12:34:56.007008009"
    );
    assert_eq!(
        thrown("return Temporal.PlainTime.prototype.add.call({}, {hours:1});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().subtract({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_time_with_prepares_fields_before_overflow_and_rejects_temporal_inputs() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name+' number');return value}}}
             var fields={
               get calendar(){log.push('calendar');return undefined},
               get timeZone(){log.push('timeZone');return undefined},
               get hour(){log.push('hour');return number('hour',25.7)},
               get microsecond(){log.push('microsecond');return number('microsecond',8.7)},
               get millisecond(){log.push('millisecond');return number('millisecond',7.7)},
               get minute(){log.push('minute');return number('minute',6.7)},
               get nanosecond(){log.push('nanosecond');return number('nanosecond',9.7)},
               get second(){log.push('second');return number('second',10.7)}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var result=new Temporal.PlainTime(12,34,56,1,2,3).with(fields,options);
             return [Temporal.PlainTime.prototype.with.length,result.toString(),log.join(',')].join('|');"
        ),
        "1|23:06:10.007008009|calendar,timeZone,hour,hour number,microsecond,microsecond number,millisecond,millisecond number,minute,minute number,nanosecond,nanosecond number,second,second number,overflow,overflow string"
    );
    assert_eq!(
        rendered(
            "var time=new Temporal.PlainTime(12,34,56,7,8,9);
             return [time.with({minute:60}).toString(),time.with({second:undefined,hour:8}).toString()].join('|');"
        ),
        "12:59:56.007008009|08:34:56.007008009"
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().with({hour:25},{overflow:'reject'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().with(new Temporal.PlainTime());"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().with(new Temporal.PlainDate(2020,1,1));"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().with(new Temporal.PlainDateTime(2020,1,1));"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().with({calendar:'iso8601',hour:1});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainTime.prototype.with.call({}, {hour:1});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_time_until_and_since_preserve_time_only_difference_semantics() {
    assert_eq!(
        rendered(
            "var earlier=new Temporal.PlainTime(9,2,3,4,5,6);
             var later=new Temporal.PlainTime(10,17,18,19,20,21);
             var until=earlier.until(later);
             var since=later.since(earlier);
             return [Temporal.PlainTime.prototype.until.name,
               Temporal.PlainTime.prototype.until.length,
               Temporal.PlainTime.prototype.since.name,
               Temporal.PlainTime.prototype.since.length,
               until.toString(),since.toString()].join('|');"
        ),
        "until|1|since|1|PT1H15M15.015015015S|PT1H15M15.015015015S"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function string(name,value){return {toString:function(){log.push(name+' string');return value}}}
             function number(name,value){return {valueOf:function(){log.push(name+' number');return value}}}
             var other={
               get hour(){log.push('hour');return 10},
               get microsecond(){log.push('microsecond');return 0},
               get millisecond(){log.push('millisecond');return 0},
               get minute(){log.push('minute');return 17},
               get nanosecond(){log.push('nanosecond');return 0},
               get second(){log.push('second');return 0}
             };
             var options={
               get largestUnit(){log.push('largestUnit');return string('largestUnit','hour')},
               get roundingIncrement(){log.push('roundingIncrement');return number('roundingIncrement',15)},
               get roundingMode(){log.push('roundingMode');return string('roundingMode','floor')},
               get smallestUnit(){log.push('smallestUnit');return string('smallestUnit','minute')}
             };
             var result=new Temporal.PlainTime(9,2).until(other,options);
             return [result.toString(),log.join(',')].join('|');"
        ),
        "PT1H15M|hour,microsecond,millisecond,minute,nanosecond,second,largestUnit,largestUnit string,roundingIncrement,roundingIncrement number,roundingMode,roundingMode string,smallestUnit,smallestUnit string"
    );
    assert_eq!(
        rendered(
            "var time=new Temporal.PlainTime(9,2);
             return [time.until('10:17',{smallestUnit:'minute',roundingIncrement:30}).toString(),
               time.since('10:17',{smallestUnit:'minute',roundingMode:'floor'}).toString()].join('|');"
        ),
        "PT1H|-PT1H15M"
    );
    assert_eq!(
        thrown(
            "return new Temporal.PlainTime().until(new Temporal.PlainTime(), {largestUnit:'day'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.PlainTime().until(new Temporal.PlainTime(), {smallestUnit:'day'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().until({}, 1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.PlainTime().until({}, {get largestUnit(){throw new Error('wrong order')}});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainTime.prototype.since.call({}, new Temporal.PlainTime());"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_time_round_uses_time_only_options_in_observable_order() {
    assert_eq!(
        rendered(
            "var time=new Temporal.PlainTime(23,59,59,900);
             return [Temporal.PlainTime.prototype.round.name,
               Temporal.PlainTime.prototype.round.length,
               time.round('second').toString(),
               time.round({smallestUnit:'minute',roundingIncrement:15}).toString()].join('|');"
        ),
        "round|1|00:00:00|00:00:00"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name+' number');return value}}}
             function string(name,value){return {toString:function(){log.push(name+' string');return value}}}
             var options={
               get roundingIncrement(){log.push('roundingIncrement');return number('roundingIncrement',15)},
               get roundingMode(){log.push('roundingMode');return string('roundingMode','halfExpand')},
               get smallestUnit(){log.push('smallestUnit');return string('smallestUnit','minute')}
             };
             var result=new Temporal.PlainTime(12,37,30).round(options);
             return [result.toString(),log.join(',')].join('|');"
        ),
        "12:45:00|roundingIncrement,roundingIncrement number,roundingMode,roundingMode string,smallestUnit,smallestUnit string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().round('day');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().round();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainTime.prototype.round.call({}, 'minute');"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_time_to_string_formats_with_resumable_options() {
    assert_eq!(
        rendered(
            "var time=new Temporal.PlainTime(12,34,56,987,654,321);
             return [time.toString({fractionalSecondDigits:2}),
               time.toString({smallestUnit:'minute',fractionalSecondDigits:5}),
               time.toString({smallestUnit:'second',roundingMode:'ceil'}),
               time.toJSON()].join('|');"
        ),
        "12:34:56.98|12:34|12:34:57|12:34:56.987654321"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function string(name,value){return {toString:function(){log.push(name+' string');return value}}}
             var options={
               get fractionalSecondDigits(){log.push('fractionalSecondDigits');return string('fractionalSecondDigits','auto')},
               get roundingMode(){log.push('roundingMode');return string('roundingMode','halfExpand')},
               get smallestUnit(){log.push('smallestUnit');return string('smallestUnit','millisecond')}
             };
             var result=new Temporal.PlainTime(12,34,56,987,654,321).toString(options);
             return [result,log.join(',')].join('|');"
        ),
        "12:34:56.988|fractionalSecondDigits,fractionalSecondDigits string,roundingMode,roundingMode string,smallestUnit,smallestUnit string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().toString({smallestUnit:'hour'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainTime().toString(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_time_constructor_preserves_order_defaults_and_branding() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name);return value}}}
             function F(){};var p={marker:true};F.prototype=p;
             var d=Reflect.construct(Temporal.PlainDateTime,[
               number('year',2020.9),number('month',2.8),number('day',29.6),
               number('hour',23.9),number('minute',59.9),number('second',58.9),
               number('millisecond',7.9),number('microsecond',8.9),number('nanosecond',9.9),'iso8601'
             ],F);
             return [Object.getPrototypeOf(d)===p,Temporal.PlainDateTime.prototype.toString.call(d),log.join(',')].join('|');"
        ),
        "true|2020-02-29T23:59:58.007008009|year,month,day,hour,minute,second,millisecond,microsecond,nanosecond"
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime(2020,1,1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime.prototype.hour.call({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,2,30);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1,0,0,0,0,0,0,{});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).valueOf();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        rendered(
            "var values=[[null,'null'],[1,'number'],[{},'object']];var output='';
             for(const [calendar,label] of values){
               var throws=(()=>{try{new Temporal.PlainDateTime(2020,1,1,0,0,0,0,0,0,calendar)}catch(error){return error instanceof TypeError}})();
               output=output+(throws?label:'wrong');
             }
             return output;"
        ),
        "nullnumberobject"
    );
}

#[test]
fn plain_date_time_from_compare_and_equals_preserve_conversion_order() {
    assert_eq!(
        rendered(
            "var source=new Temporal.PlainDateTime(2021,2,3,4,5,6,7,8,9);
             var cloned=Temporal.PlainDateTime.from(source);
             var date=Temporal.PlainDateTime.from(new Temporal.PlainDate(2021,2,3));
             var parsed=Temporal.PlainDateTime.from('2021-02-03T04:05:06.007008009');
             return [Temporal.PlainDateTime.from.name,Temporal.PlainDateTime.from.length,
               Temporal.PlainDateTime.compare.name,Temporal.PlainDateTime.compare.length,
               Temporal.PlainDateTime.prototype.equals.name,Temporal.PlainDateTime.prototype.equals.length,
               cloned.toString(),date.toString(),parsed.toString(),
               Temporal.PlainDateTime.compare(date,source),
               Temporal.PlainDateTime.compare(source,'2021-02-03T04:05:06.007008009'),
               source.equals('2021-02-03T04:05:06.007008009'),
               source.equals({year:2021,month:2,day:3,hour:4,minute:5,second:6,millisecond:7,microsecond:8,nanosecond:8})].join('|');"
        ),
        "from|1|compare|2|equals|1|2021-02-03T04:05:06.007008009|2021-02-03T00:00:00|2021-02-03T04:05:06.007008009|-1|0|true|false"
    );
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name+' number');return value}}}
             var fields={
               get calendar(){log.push('calendar');return 'iso8601'},
               get day(){log.push('day');return number('day',31.8)},
               get hour(){log.push('hour');return number('hour',23.8)},
               get microsecond(){log.push('microsecond');return number('microsecond',8.8)},
               get millisecond(){log.push('millisecond');return number('millisecond',7.8)},
               get minute(){log.push('minute');return number('minute',59.8)},
               get month(){log.push('month');return number('month',2.8)},
               get monthCode(){log.push('monthCode');return {toString:function(){log.push('monthCode string');return 'M02'}}},
               get nanosecond(){log.push('nanosecond');return number('nanosecond',9.8)},
               get second(){log.push('second');return number('second',58.8)},
               get year(){log.push('year');return number('year',2020.8)}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var result=Temporal.PlainDateTime.from(fields,options);
             return [result.toString(),log.join(',')].join('|');"
        ),
        "2020-02-29T23:59:58.007008009|calendar,day,day number,hour,hour number,microsecond,microsecond number,millisecond,millisecond number,minute,minute number,month,month number,monthCode,monthCode string,nanosecond,nanosecond number,second,second number,year,year number,overflow,overflow string"
    );
    assert_eq!(
        thrown(
            "return Temporal.PlainDateTime.from({year:2021,month:2,day:30},{overflow:'reject'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2021,1,1).equals(0);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return Temporal.PlainDateTime.from(new Temporal.PlainDateTime(2021,1,1),{overflow:Symbol()});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        rendered(
            "var log=[];
             var overflow={
               get valueOf(){log.push('get valueOf');return function(){log.push('call valueOf');return 'wrong'}},
               get toString(){log.push('get toString');return function(){log.push('call toString');return 'constrain'}}
             };
             var result=Temporal.PlainDateTime.from(new Temporal.PlainDateTime(2021,1,1),{overflow:overflow});
             return [result.toString(),log.join(',')].join('|');"
        ),
        "2021-01-01T00:00:00|get toString,call toString"
    );
    assert_eq!(
        rendered(
            "function kind(overflow){
               try{Temporal.PlainDateTime.from(new Temporal.PlainDateTime(2021,1,1),{overflow:overflow})}
               catch(error){return error instanceof TypeError?'type':error instanceof RangeError?'range':'other'}
               return 'normal'
             }
             return [kind(null),kind(true),kind(false),kind(Symbol()),kind(2),kind(2n),kind({})].join('|');"
        ),
        "range|range|range|type|range|range|range"
    );
}

#[test]
fn plain_date_constructor_observes_component_and_calendar_conversion_order() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name);return value}}}
             var d=new Temporal.PlainDate(number('year',2020.9),number('month',2.8),number('day',29.6),'iso8601');
             return [d.toString(),log.join(',')].join('|');"
        ),
        "2020-02-29|year,month,day"
    );
}

#[test]
fn plain_date_from_equals_and_new_target_prototype_preserve_branding() {
    assert_eq!(
        rendered(
            "function F(){};var p={marker:true};F.prototype=p;
             var d=Reflect.construct(Temporal.PlainDate,[2019,3,15],F);
             var from=Temporal.PlainDate.from('2019-03-15');
             return [Object.getPrototypeOf(d)===p,from.toString(),
               from.equals(d),from.equals('2019-03-15'),from.equals('2019-03-16'),
               Temporal.PlainDate.compare.length,Temporal.PlainDate.compare(from,d),
               Temporal.PlainDate.compare('2019-03-14',from),
               Temporal.PlainDate.compare(from,'2019-03-16')].join('|');"
        ),
        "true|2019-03-15|true|true|false|2|0|-1|-1"
    );
}

#[test]
fn plain_date_from_property_bags_observe_field_and_overflow_conversion_order() {
    assert_eq!(
        rendered(
            "var log=[];
             var fields={
               get calendar(){log.push('calendar');return 'iso8601'},
               get day(){log.push('day');return {valueOf:function(){log.push('day number');return 32.8}}},
               get month(){log.push('month');return {valueOf:function(){log.push('month number');return 1.9}}},
               get monthCode(){log.push('monthCode');return {toString:function(){log.push('monthCode string');return 'M01'}}},
               get year(){log.push('year');return {valueOf:function(){log.push('year number');return 2021.7}}}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var date=Temporal.PlainDate.from(fields,options);
             return [date.toString(),log.join(',')].join('|');"
        ),
        "2021-01-31|calendar,day,day number,month,month number,monthCode,monthCode string,year,year number,overflow,overflow string"
    );
    assert_eq!(
        rendered(
            "return [Temporal.PlainDate.from({year:1976,month:11,day:18}).toString(),
              Temporal.PlainDate.from({year:1976,monthCode:'M11',day:18},{overflow:'reject'}).toString()].join('|');"
        ),
        "1976-11-18|1976-11-18"
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.from({year:2019,month:1,day:32},{overflow:'reject'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.from({year:2019,day:15});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.from({year:2021,month:12,monthCode:'M11',day:17});"),
        ExceptionKind::RangeError
    );
}

#[test]
fn plain_date_compare_reuses_resumable_property_bag_conversion_for_both_operands() {
    assert_eq!(
        rendered(
            "var log=[];
             function bag(label,year,month,day){return {
               get calendar(){log.push(label+'.calendar');return 'iso8601'},
               get day(){log.push(label+'.day');return day},
               get month(){log.push(label+'.month');return month},
               get monthCode(){log.push(label+'.monthCode');return undefined},
               get year(){log.push(label+'.year');return year}
             }}
             var result=Temporal.PlainDate.compare(bag('first',2021,2,3),bag('second',2021,2,4));
             return [result,log.join(',')].join('|');"
        ),
        "-1|first.calendar,first.day,first.month,first.monthCode,first.year,second.calendar,second.day,second.month,second.monthCode,second.year"
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.compare({year:2021,month:2,day:3},{year:2021,day:4});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_add_and_subtract_reuse_duration_bags_then_read_overflow() {
    assert_eq!(
        rendered(
            "var log=[];
             var duration={
               get years(){log.push('years');return undefined},
               get months(){log.push('months');return {valueOf:function(){log.push('months number');return 1}}},
               get weeks(){log.push('weeks');return undefined},
               get days(){log.push('days');return undefined},
               get hours(){log.push('hours');return undefined},
               get minutes(){log.push('minutes');return undefined},
               get seconds(){log.push('seconds');return undefined},
               get milliseconds(){log.push('milliseconds');return undefined},
               get microseconds(){log.push('microseconds');return undefined},
               get nanoseconds(){log.push('nanoseconds');return undefined}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var start=new Temporal.PlainDate(2020,1,31);
             var added=start.add(duration,options);
             var subtracted=added.subtract('P1M');
             return [Temporal.PlainDate.prototype.add.length,Temporal.PlainDate.prototype.subtract.length,
               added.toString(),subtracted.toString(),log.join(',')].join('|');"
        ),
        "1|1|2020-02-29|2020-01-29|days,hours,microseconds,milliseconds,minutes,months,months number,nanoseconds,seconds,weeks,years,overflow,overflow string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2021,1,31).add({months:1},{overflow:'reject'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.prototype.add.call({}, {days:1});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_time_add_and_subtract_reuse_duration_bags_then_read_overflow() {
    assert_eq!(
        rendered(
            "var log=[];
             var duration={
               get years(){log.push('years');return undefined},
               get months(){log.push('months');return {valueOf:function(){log.push('months number');return 1}}},
               get weeks(){log.push('weeks');return undefined},
               get days(){log.push('days');return undefined},
               get hours(){log.push('hours');return {valueOf:function(){log.push('hours number');return 2}}},
               get minutes(){log.push('minutes');return undefined},
               get seconds(){log.push('seconds');return undefined},
               get milliseconds(){log.push('milliseconds');return undefined},
               get microseconds(){log.push('microseconds');return undefined},
               get nanoseconds(){log.push('nanoseconds');return undefined}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var start=new Temporal.PlainDateTime(2020,1,31,23,30);
             var added=start.add(duration,options);
             var subtracted=added.subtract('P1M');
             return [Temporal.PlainDateTime.prototype.add.length,Temporal.PlainDateTime.prototype.subtract.length,
               added.toString(),subtracted.toString(),log.join(',')].join('|');"
        ),
        "1|1|2020-03-01T01:30:00|2020-02-01T01:30:00|days,hours,hours number,microseconds,milliseconds,minutes,months,months number,nanoseconds,seconds,weeks,years,overflow,overflow string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2021,1,31).add({months:1},{overflow:'reject'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime.prototype.add.call({}, {days:1});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_time_with_prepares_all_fields_before_reading_overflow() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name+' number');return value}}}
             var fields={
               get calendar(){log.push('calendar');return undefined},
               get timeZone(){log.push('timeZone');return undefined},
               get day(){log.push('day');return number('day',1.7)},
               get hour(){log.push('hour');return number('hour',5.7)},
               get microsecond(){log.push('microsecond');return number('microsecond',8.7)},
               get millisecond(){log.push('millisecond');return number('millisecond',7.7)},
               get minute(){log.push('minute');return number('minute',6.7)},
               get month(){log.push('month');return number('month',2.8)},
               get monthCode(){log.push('monthCode');return {toString:function(){log.push('monthCode string');return 'M02'}}},
               get nanosecond(){log.push('nanosecond');return number('nanosecond',9.7)},
               get second(){log.push('second');return number('second',10.7)},
               get year(){log.push('year');return number('year',2021.7)}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var result=new Temporal.PlainDateTime(2020,5,31,12,34,56,7,8,9).with(fields,options);
             return [Temporal.PlainDateTime.prototype.with.length,result.toString(),log.join(',')].join('|');"
        ),
        "1|2021-02-01T05:06:10.007008009|calendar,timeZone,day,day number,hour,hour number,microsecond,microsecond number,millisecond,millisecond number,minute,minute number,month,month number,monthCode,monthCode string,nanosecond,nanosecond number,second,second number,year,year number,overflow,overflow string"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2021,1,1).with({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2021,1,1).with({calendar:'iso8601'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2021,1,1).with({timeZone:'UTC'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2021,1,31).with({month:2},{overflow:'reject'});"),
        ExceptionKind::RangeError
    );
}

#[test]
fn plain_date_time_differences_convert_before_options_and_normalize_duration_fields() {
    assert_eq!(
        rendered(
            "var log=[];
             var other={
               get calendar(){log.push('calendar');return 'iso8601'},
               get day(){log.push('day');return 2},
               get hour(){log.push('hour');return 1},
               get microsecond(){log.push('microsecond');return 5},
               get millisecond(){log.push('millisecond');return 4},
               get minute(){log.push('minute');return 2},
               get month(){log.push('month');return 1},
               get monthCode(){log.push('monthCode');return undefined},
               get nanosecond(){log.push('nanosecond');return 6},
               get second(){log.push('second');return 3},
               get year(){log.push('year');return 2020}
             };
             var options={};
             Object.defineProperties(options,{
               largestUnit:{get:function(){log.push('largestUnit');return {toString:function(){log.push('largestUnit string');return 'day'}}}},
               roundingIncrement:{get:function(){log.push('roundingIncrement');return undefined}},
               roundingMode:{get:function(){log.push('roundingMode');return {toString:function(){log.push('roundingMode string');return 'trunc'}}}},
               smallestUnit:{get:function(){log.push('smallestUnit');return undefined}}
             });
             var start=new Temporal.PlainDateTime(2020,1,1);
             var until=start.until(other,options);
             var since=Temporal.PlainDateTime.from(other).since(start);
             return [Temporal.PlainDateTime.prototype.until.length,
               Temporal.PlainDateTime.prototype.since.length,until.toString(),
               since.toString(),log.join(',')].join('|');"
        ),
        "1|1|P1DT1H2M3.004005006S|P1DT1H2M3.004005006S|calendar,day,hour,microsecond,millisecond,minute,month,monthCode,nanosecond,second,year,largestUnit,largestUnit string,roundingIncrement,roundingMode,roundingMode string,smallestUnit,calendar,day,hour,microsecond,millisecond,minute,month,monthCode,nanosecond,second,year"
    );
    assert_eq!(
        rendered(
            "var start=new Temporal.PlainDateTime(1970,1,1);
             var end=new Temporal.PlainDateTime(2554,7,21,23,34,33,709,551,616);
             var diff=start.until(end,{largestUnit:'microseconds'});
             var half=new Temporal.PlainDateTime(2019,1,1).until(
               new Temporal.PlainDateTime(2020,7,2));
             return [diff.microseconds===18446744073709552,diff.toString(),half.total({unit:'years',relativeTo:
               new Temporal.PlainDateTime(2019,1,1)})].join('|');"
        ),
        "true|PT18446744073.709552616S|1.5"
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime.prototype.until.call({}, '2020-01-01T00:00');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).since(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.PlainDateTime(2020,1,1).until('2020-01-02',{largestUnit:'invalid'});"
        ),
        ExceptionKind::RangeError
    );
}

#[test]
fn plain_date_time_round_preserves_option_order_and_date_time_rounding_rules() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               roundingIncrement:{get:function(){log.push('increment');return {valueOf:function(){log.push('increment number');return 15}}}},
               roundingMode:{get:function(){log.push('mode');return {toString:function(){log.push('mode string');return 'trunc'}}}},
               smallestUnit:{get:function(){log.push('unit');return {toString:function(){log.push('unit string');return 'minute'}}}}
             });
             var value=new Temporal.PlainDateTime(2020,1,1,12,34,35,678,901,234);
             return [Temporal.PlainDateTime.prototype.round.length,
               value.round('minute').toString(),value.round(options).toString(),
               new Temporal.PlainDateTime(2020,1,1,12).round('day').toString(),
               log.join(',')].join('|');"
        ),
        "1|2020-01-01T12:35:00|2020-01-01T12:30:00|2020-01-02T00:00:00|increment,increment number,mode,mode string,unit,unit string"
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime.prototype.round.call({}, 'second');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).round();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).round({smallestUnit:'invalid'});"),
        ExceptionKind::RangeError
    );
}

#[test]
fn plain_date_time_to_plain_date_allocates_a_branded_calendar_preserving_copy() {
    assert_eq!(
        rendered(
            "var value=new Temporal.PlainDateTime(2020,2,29,12,34,56,7,8,9);
             var date=value.toPlainDate();
             return [Temporal.PlainDateTime.prototype.toPlainDate.length,date.toString(),
               date.calendarId,date===value,Object.getPrototypeOf(date)===Temporal.PlainDate.prototype].join('|');"
        ),
        "0|2020-02-29|iso8601|false|true"
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime.prototype.toPlainDate.call({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_time_to_plain_time_allocates_a_branded_time_copy() {
    assert_eq!(
        rendered(
            "var value=new Temporal.PlainDateTime(2020,2,29,12,34,56,7,8,9);
             var time=value.toPlainTime();
             return [Temporal.PlainDateTime.prototype.toPlainTime.length,time.toString(),
               time===value,Object.getPrototypeOf(time)===Temporal.PlainTime.prototype].join('|');"
        ),
        "0|12:34:56.007008009|false|true"
    );
    assert_eq!(
        thrown("return Temporal.PlainDateTime.prototype.toPlainTime.call({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_time_with_calendar_accepts_identifiers_and_temporal_fast_paths_only() {
    assert_eq!(
        rendered(
            "var value=new Temporal.PlainDateTime(2020,2,29,12,34,56,7,8,9);
             var fromString=value.withCalendar('iso8601');
             var fromDate=value.withCalendar(new Temporal.PlainDate(2001,1,2));
             var fromDateTime=value.withCalendar(new Temporal.PlainDateTime(2001,1,2));
             return [Temporal.PlainDateTime.prototype.withCalendar.length,
               fromString.toString(),fromDate.toString(),fromDateTime.toString(),
               fromString===value].join('|');"
        ),
        "1|2020-02-29T12:34:56.007008009|2020-02-29T12:34:56.007008009|2020-02-29T12:34:56.007008009|false"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).withCalendar();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDateTime(2020,1,1).withCalendar({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_with_calendar_accepts_identifiers_and_temporal_fast_paths_only() {
    assert_eq!(
        rendered(
            "var value=new Temporal.PlainDate(2020,2,29);
             var fromString=value.withCalendar('iso8601');
             var fromDate=value.withCalendar(new Temporal.PlainDate(2001,1,2));
             var fromDateTime=value.withCalendar(new Temporal.PlainDateTime(2001,1,2));
             return [Temporal.PlainDate.prototype.withCalendar.length,
               fromString.toString(),fromDate.toString(),fromDateTime.toString(),
               fromString===value].join('|');"
        ),
        "1|2020-02-29|2020-02-29|2020-02-29|false"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).withCalendar();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).withCalendar({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_to_plain_date_time_defaults_and_prepares_time_fields_in_order() {
    assert_eq!(
        rendered(
            "var log=[];
             function number(name,value){return {valueOf:function(){log.push(name);return value}}}
             var fields={
               get hour(){log.push('hour');return number('hour number',25)},
               get microsecond(){log.push('microsecond');return number('microsecond number',8)},
               get millisecond(){log.push('millisecond');return number('millisecond number',7)},
               get minute(){log.push('minute');return number('minute number',70)},
               get nanosecond(){log.push('nanosecond');return number('nanosecond number',9)},
               get second(){log.push('second');return number('second number',23)}
             };
             var date=new Temporal.PlainDate(2020,2,29);
             var defaulted=date.toPlainDateTime();
             var string=date.toPlainDateTime('11:30:23');
             var fieldsResult=date.toPlainDateTime(fields);
             var fromDateTime=date.toPlainDateTime(new Temporal.PlainDateTime(2001,1,2,3,4,5,6,7,8));
             var fromTime=date.toPlainDateTime(new Temporal.PlainTime(9,8,7,6,5,4));
             return [Temporal.PlainDate.prototype.toPlainDateTime.length,
               defaulted.toString(),string.toString(),fieldsResult.toString(),fromDateTime.toString(),
               fromTime.toString(),log.join(',')].join('|');"
        ),
        "0|2020-02-29T00:00:00|2020-02-29T11:30:23|2020-02-29T23:59:23.007008009|2020-02-29T03:04:05.006007008|2020-02-29T09:08:07.006005004|hour,hour number,microsecond,microsecond number,millisecond,millisecond number,minute,minute number,nanosecond,nanosecond number,second,second number"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).toPlainDateTime({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).toPlainDateTime(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).toPlainDateTime('');"),
        ExceptionKind::RangeError
    );
}

#[test]
fn plain_date_with_prepares_partial_fields_before_reading_overflow() {
    assert_eq!(
        rendered(
            "var log=[];
             var fields={
               get calendar(){log.push('calendar');return undefined},
               get timeZone(){log.push('timeZone');return undefined},
               get day(){log.push('day');return {valueOf:function(){log.push('day number');return 1.7}}},
               get month(){log.push('month');return {valueOf:function(){log.push('month number');return 2.8}}},
               get monthCode(){log.push('monthCode');return undefined},
               get year(){log.push('year');return undefined}
             };
             var options={get overflow(){log.push('overflow');return {toString:function(){log.push('overflow string');return 'constrain'}}}};
             var date=new Temporal.PlainDate(2020,5,31).with(fields,options);
             return [Temporal.PlainDate.prototype.with.length,date.toString(),log.join(',')].join('|');"
        ),
        "1|2020-02-01|calendar,timeZone,day,day number,month,month number,monthCode,year,overflow,overflow string"
    );
    assert_eq!(
        rendered(
            "var d=new Temporal.PlainDate(2006,1,24);return d.with({day:1,year:undefined}).toString();"
        ),
        "2006-01-01"
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).with({calendar:'iso8601'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).with({timeZone:'UTC'});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).with({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn plain_date_rejects_unbranded_receivers_invalid_components_and_call_without_new() {
    assert_eq!(
        thrown("return Temporal.PlainDate(2020,1,1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.prototype.year.call({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,2,30);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(Infinity,1,1);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1,'invalid-calendar');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1,{});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.PlainDate.from({year:2020,month:1,day:1,calendar:{}});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.PlainDate(2020,1,1).valueOf();"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_constructor_and_accessors_preserve_all_ten_fields() {
    assert_eq!(
        rendered(
            "var d=new Temporal.Duration(1,2,3,4,5,6,7,8,9,10),z=new Temporal.Duration();
             return [d.years,d.months,d.weeks,d.days,d.hours,d.minutes,d.seconds,
               d.milliseconds,d.microseconds,d.nanoseconds,d.sign,d.blank,
               d.toString(),d.toJSON(),d.toLocaleString(),z.sign,z.blank,z.toString()].join('|');"
        ),
        "1|2|3|4|5|6|7|8|9|10|1|false|P1Y2M3W4DT5H6M7.00800901S|P1Y2M3W4DT5H6M7.00800901S|P1Y2M3W4DT5H6M7.00800901S|0|true|PT0S"
    );
}

#[test]
fn duration_to_string_formats_fractional_precision_and_rounding() {
    assert_eq!(
        rendered(
            "var time=Temporal.Duration.from('PT1H29M31.987654321S');
             var calendar=Temporal.Duration.from('P1Y2M3W4DT5H6M7.123456789S');
             return [time.toString(),time.toString({}),
               time.toString({fractionalSecondDigits:0}),
               time.toString({fractionalSecondDigits:3}),
               time.toString({fractionalSecondDigits:'auto'}),
               time.toString({roundingMode:'ceil',smallestUnit:'second'}),
               time.toString({fractionalSecondDigits:3,smallestUnit:'second'}),
               calendar.toString({fractionalSecondDigits:3}),
               calendar.toString({roundingMode:'ceil',smallestUnit:'second'})].join('|');"
        ),
        "PT1H29M31.987654321S|PT1H29M31.987654321S|PT1H29M31S|PT1H29M31.987S|PT1H29M31.987654321S|PT1H29M32S|PT1H29M31S|P1Y2M3W4DT5H6M7.123S|P1Y2M3W4DT5H6M8S"
    );
}

#[test]
fn duration_to_string_observes_options_and_coercions_in_specified_order() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               fractionalSecondDigits:{get:function(){log.push('digits');return {toString:function(){log.push('digits string');return 'auto';}}}},
               roundingMode:{get:function(){log.push('mode');return {toString:function(){log.push('mode string');return 'trunc';}}}},
               smallestUnit:{get:function(){log.push('unit');return {toString:function(){log.push('unit string');return 'millisecond';}}}}
             });
             return Temporal.Duration.from('PT1.234S').toString(options)+'|'+log.join(',');"
        ),
        "PT1.234S|digits,digits string,mode,mode string,unit,unit string"
    );
}

#[test]
fn duration_to_string_rejects_invalid_options_and_keeps_json_locale_defaults() {
    assert_eq!(
        thrown("return new Temporal.Duration().toString(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().toString({fractionalSecondDigits:'invalid'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().toString({smallestUnit:'minute'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               fractionalSecondDigits:{get:function(){log.push('digits');return undefined;}},
               roundingMode:{get:function(){log.push('mode');return 'trunc';}},
               smallestUnit:{get:function(){log.push('unit');return 'hour';}}
             });
             try { new Temporal.Duration().toString(options); }
             catch (error) { return error.name+'|'+log.join(','); }"
        ),
        "RangeError|digits,mode,unit"
    );
    assert_eq!(
        rendered(
            "var duration=Temporal.Duration.from('PT1.23456789S');
             return [duration.toJSON(),duration.toLocaleString('fr',{smallestUnit:'second'})].join('|');"
        ),
        "PT1.23456789S|PT1.23456789S"
    );
}

#[test]
fn duration_constructor_coerces_left_to_right_and_skips_undefined() {
    assert_eq!(
        rendered(
            "var log=[];function value(label,value){return {valueOf:function(){log.push(label);return value}}}
             var d=new Temporal.Duration(value('years',1),undefined,value('weeks',2),
               undefined,undefined,value('minutes',3));
             return [d.years,d.months,d.weeks,d.days,d.minutes,log.join(',')].join('|');"
        ),
        "1|0|2|0|3|years,weeks,minutes"
    );
}

#[test]
fn duration_constructor_rejects_non_integral_and_mixed_sign_fields() {
    assert_eq!(
        thrown("return new Temporal.Duration(1.5);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(Infinity);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(1,-1);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(1n);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_prototype_methods_enforce_brand_and_primitive_rejection() {
    assert_eq!(
        thrown("return Temporal.Duration.prototype.years;"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.Duration.prototype.toString.call({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().valueOf();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.Duration();"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_abs_negated_and_subclass_prototypes_allocate_fresh_branded_values() {
    assert_eq!(
        rendered(
            "var d=new Temporal.Duration(0,0,0,-2,-3),a=d.abs(),n=a.negated();
             function Sub(){};var s=Reflect.construct(Temporal.Duration,[1],Sub);
             return [a.days,a.hours,a===d,n.days,n.hours,
               Object.getPrototypeOf(s)===Sub.prototype,
               Object.getOwnPropertyDescriptor(Temporal.Duration.prototype,'years').get.call(s),
               Temporal.Duration.prototype.toString.call(s)].join('|');"
        ),
        "2|3|false|-2|-3|true|1|P1Y"
    );
}

#[test]
fn duration_from_parses_strings_and_copies_branded_values() {
    assert_eq!(
        rendered(
            "var first=Temporal.Duration.from('P1Y2M3DT4H5M6.007008009S');
             var copy=Temporal.Duration.from(first);
             return [Temporal.Duration.from.length,Temporal.Duration.from.name,
               first.toString(),copy.toString(),first===copy].join('|');"
        ),
        "1|from|P1Y2M3DT4H5M6.007008009S|P1Y2M3DT4H5M6.007008009S|false"
    );
    assert_eq!(
        thrown("return Temporal.Duration.from('not a duration');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.Duration.from(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_compare_orders_time_units_and_requires_context_for_calendar_units() {
    assert_eq!(
        rendered(
            "var a=Temporal.Duration.from('PT5H5M'),b=Temporal.Duration.from('PT5H4M');
             return [Temporal.Duration.compare.length,Temporal.Duration.compare.name,
               Temporal.Duration.compare(a,a),Temporal.Duration.compare(a,b),
               Temporal.Duration.compare(b,a),Temporal.Duration.compare('-PT1S','PT0S')].join('|');"
        ),
        "2|compare|0|1|-1|-1"
    );
    assert_eq!(
        thrown("return Temporal.Duration.compare('P1Y','P2Y');"),
        ExceptionKind::RangeError
    );
}

#[test]
fn duration_property_bags_read_and_convert_fields_in_normative_order() {
    assert_eq!(
        rendered(
            "var log=[],fields={};
             ['years','months','weeks','days','hours','minutes','seconds','milliseconds','microseconds','nanoseconds']
               .forEach(function(name){Object.defineProperty(fields,name,{get:function(){
                 log.push('get '+name);return {valueOf:function(){log.push('convert '+name);return 1;}};
               }});});
             var d=Temporal.Duration.from(fields);
             return d.toString()+'|'+log.join(',');"
        ),
        "P1Y1M1W1DT1H1M1.001001001S|get days,convert days,get hours,convert hours,get microseconds,convert microseconds,get milliseconds,convert milliseconds,get minutes,convert minutes,get months,convert months,get nanoseconds,convert nanoseconds,get seconds,convert seconds,get weeks,convert weeks,get years,convert years"
    );
}

#[test]
fn duration_property_bags_require_one_valid_integral_field() {
    assert_eq!(
        thrown("return Temporal.Duration.from({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.Duration.from({days:1.5});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.Duration.from({days:1,hours:-1});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        rendered(
            "var a={hours:2},b={minutes:119};return [Temporal.Duration.compare(a,b),
             Temporal.Duration.from({days:undefined,seconds:3}).toString()].join('|');"
        ),
        "1|PT3S"
    );
}

#[test]
fn duration_compare_reads_options_after_both_duration_conversions() {
    assert_eq!(
        rendered(
            "var log=[];
             function bag(label,value){var o={};Object.defineProperty(o,'hours',{get:function(){
               log.push(label);return value;}});return o;}
             var options={};Object.defineProperty(options,'relativeTo',{get:function(){
               log.push('relativeTo');return undefined;}});
             var result=Temporal.Duration.compare(bag('first',2),bag('second',1),options);
             return result+'|'+log.join(',')+'|'+
               Temporal.Duration.compare({hours:1},{minutes:60},{})+'|'+
               Temporal.Duration.compare({days:31},{months:1},{relativeTo:'2019-11-01'});"
        ),
        "1|first,second,relativeTo|0|1"
    );
    assert_eq!(
        thrown("return Temporal.Duration.compare({hours:1},{hours:1},null);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_add_and_subtract_convert_the_other_duration_and_allocate_results() {
    assert_eq!(
        rendered(
            "var log=[],other={};
             Object.defineProperty(other,'hours',{get:function(){log.push('get hours');
               return {valueOf:function(){log.push('convert hours');return 25;}};}});
             var original=new Temporal.Duration(0,0,0,1),sum=original.add(other);
             var difference=sum.subtract({hours:1});
             return [Temporal.Duration.prototype.add.length,
               Temporal.Duration.prototype.subtract.length,original.toString(),sum.toString(),
               difference.toString(),sum===original,log.join(',')].join('|');"
        ),
        "1|1|P1D|P2DT1H|P2D|false|get hours,convert hours"
    );
}

#[test]
fn duration_arithmetic_enforces_brand_and_rejects_unanchored_calendar_units() {
    assert_eq!(
        thrown("return Temporal.Duration.prototype.add.call({}, {hours:1});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(1).add(new Temporal.Duration(1));"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().subtract({});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn instant_add_and_subtract_share_the_temporal_duration_conversion_boundary() {
    assert_eq!(
        rendered(
            "var instant=Temporal.Instant.from('1970-01-01T00:00Z');
             var result=instant.add('PT1H2M3.004005006S');
             var difference=result.subtract({seconds:1,nanoseconds:1});
             return [Temporal.Instant.prototype.add.length,
               Temporal.Instant.prototype.subtract.length,instant.toString(),result.toString(),
               difference.epochNanoseconds,result===instant].join('|');"
        ),
        "1|1|1970-01-01T00:00:00Z|1970-01-01T01:02:03.004005006Z|3722004005005|false"
    );
}

#[test]
fn instant_arithmetic_reads_duration_bags_before_rejecting_date_units() {
    assert_eq!(
        rendered(
            "var log=[],bag={};
             for(var name of ['days','hours','microseconds','milliseconds','minutes','months',
                 'nanoseconds','seconds','weeks','years']){
               (function(name){Object.defineProperty(bag,name,{get:function(){
                 log.push(name);return name==='hours'?1:undefined;}})})(name);
             }
             var instant=new Temporal.Instant(0n);
             var result=instant.add(bag);
             return [result.toString(),log.join(',')].join('|');"
        ),
        "1970-01-01T01:00:00Z|days,hours,microseconds,milliseconds,minutes,months,nanoseconds,seconds,weeks,years"
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).add({days:1});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(8640000000000000000000n).add({nanoseconds:1});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).subtract('P1D');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).add({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return Temporal.Instant.prototype.add.call({}, {hours:1});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn instant_round_supports_string_shorthand_modes_and_increments() {
    assert_eq!(
        rendered(
            "var instant=Temporal.Instant.fromEpochNanoseconds(123456789123456789n);
             var descriptor=Object.getOwnPropertyDescriptor(Temporal.Instant.prototype,'round');
             return [Temporal.Instant.prototype.round.length,descriptor.enumerable,
               descriptor.writable,descriptor.configurable,
               instant.round('second').toString(),
               instant.round({smallestUnit:'minute',roundingIncrement:15,roundingMode:'ceil'}).toString(),
               instant.round({smallestUnit:'millisecond',roundingMode:'floor'}).toString(),
               instant.round('second')===instant].join('|');"
        ),
        "1|false|true|true|1973-11-29T21:33:09Z|1973-11-29T21:45:00Z|1973-11-29T21:33:09.123Z|false"
    );
}

#[test]
fn instant_round_observes_options_and_coercions_in_specified_order() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               roundingIncrement:{get:function(){log.push('get roundingIncrement');return {
                 valueOf:function(){log.push('number roundingIncrement');return 1;}}}},
               roundingMode:{get:function(){log.push('get roundingMode');return {
                 toString:function(){log.push('string roundingMode');return 'floor';}}}},
               smallestUnit:{get:function(){log.push('get smallestUnit');return {
                 toString:function(){log.push('string smallestUnit');return 'second';}}}}
             });
             return Temporal.Instant.fromEpochNanoseconds(123456789123456789n).round(options).toString()+'|'+log.join(',');"
        ),
        "1973-11-29T21:33:09Z|get roundingIncrement,number roundingIncrement,get roundingMode,string roundingMode,get smallestUnit,string smallestUnit"
    );
}

#[test]
fn instant_round_requires_a_time_smallest_unit_and_a_branded_receiver() {
    assert_eq!(
        thrown("return Temporal.Instant.prototype.round.call({}, 'second');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).round();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).round({});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).round({smallestUnit:'day'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Instant(0n).round({smallestUnit:'second',roundingIncrement:86401});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Instant(0n).round({smallestUnit:'second',roundingMode:'invalid'});"
        ),
        ExceptionKind::RangeError
    );
}

#[test]
fn instant_difference_supports_until_since_defaults_and_time_unit_rounding() {
    assert_eq!(
        rendered(
            "var before=Temporal.Instant.from('2020-01-01T00:00:00Z');
             var after=Temporal.Instant.from('2020-01-02T01:02:03.456789123Z');
             return [Temporal.Instant.prototype.until.length,
               Temporal.Instant.prototype.since.length,
               before.until(after).toString(),before.since(after).toString(),
               before.until(after,{smallestUnit:'minute'}).toString(),
               before.until(after,{largestUnit:'hour',smallestUnit:'minute'}).toString()].join('|');"
        ),
        "1|1|PT90123.456789123S|-PT90123.456789123S|PT1502M|PT25H2M"
    );
}

#[test]
fn instant_difference_observes_operand_then_options_and_coercions_in_specified_order() {
    assert_eq!(
        rendered(
            "var log=[];
             var other={toString:function(){log.push('other toString');return '2020-01-01T00:00:01Z';}};
             var options={};
             Object.defineProperties(options,{
               largestUnit:{get:function(){log.push('get largestUnit');return {toString:function(){log.push('string largestUnit');return 'second';}}}},
               roundingIncrement:{get:function(){log.push('get roundingIncrement');return {valueOf:function(){log.push('number roundingIncrement');return 1;}}}},
               roundingMode:{get:function(){log.push('get roundingMode');return {toString:function(){log.push('string roundingMode');return 'trunc';}}}},
               smallestUnit:{get:function(){log.push('get smallestUnit');return {toString:function(){log.push('string smallestUnit');return 'second';}}}}
             });
             return Temporal.Instant.from('2020-01-01T00:00:00Z').until(other,options).toString()+'|'+log.join(',');"
        ),
        "PT1S|other toString,get largestUnit,string largestUnit,get roundingIncrement,number roundingIncrement,get roundingMode,string roundingMode,get smallestUnit,string smallestUnit"
    );
}

#[test]
fn instant_difference_rejects_invalid_receivers_options_and_units_after_reading_all_options() {
    assert_eq!(
        thrown("return Temporal.Instant.prototype.until.call({}, '2020-01-01T00:00Z');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).until('2020-01-01T00:00Z', null);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               largestUnit:{get:function(){log.push('largest');return 'year';}},
               roundingIncrement:{get:function(){log.push('increment');return 1;}},
               roundingMode:{get:function(){log.push('mode');return 'trunc';}},
               smallestUnit:{get:function(){log.push('smallest');return 'second';}}
             });
             try { new Temporal.Instant(0n).until('2020-01-01T00:00Z',options); }
             catch (error) { return error.name+'|'+log.join(','); }"
        ),
        "RangeError|largest,increment,mode,smallest"
    );
}

#[test]
fn instant_to_string_formats_fractional_precision_rounding_and_time_zones() {
    assert_eq!(
        rendered(
            "var instant=Temporal.Instant.from('2020-01-02T03:04:05.678901234Z');
             return [instant.toString(),instant.toString({fractionalSecondDigits:0}),
               instant.toString({fractionalSecondDigits:3}),
               instant.toString({roundingMode:'ceil',smallestUnit:'second'}),
               instant.toString({smallestUnit:'minute'}),
               instant.toString({timeZone:'UTC'}),instant.toString({timeZone:'+05:30'}),
               instant.toString({timeZone:'America/New_York'})].join('|');"
        ),
        "2020-01-02T03:04:05.678901234Z|2020-01-02T03:04:05Z|2020-01-02T03:04:05.678Z|2020-01-02T03:04:06Z|2020-01-02T03:04Z|2020-01-02T03:04:05.678901234+00:00|2020-01-02T08:34:05.678901234+05:30|2020-01-01T22:04:05.678901234-05:00"
    );
}

#[test]
fn instant_to_string_observes_options_and_coercions_in_specified_order() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               fractionalSecondDigits:{get:function(){log.push('digits');return {toString:function(){log.push('digits string');return 'auto';}}}},
               roundingMode:{get:function(){log.push('mode');return {toString:function(){log.push('mode string');return 'trunc';}}}},
               smallestUnit:{get:function(){log.push('unit');return {toString:function(){log.push('unit string');return 'millisecond';}}}},
               timeZone:{get:function(){log.push('zone');return 'UTC';}}
             });
             return Temporal.Instant.from('2020-01-02T03:04:05.678901234Z').toString(options)+'|'+log.join(',');"
        ),
        "2020-01-02T03:04:05.678+00:00|digits,digits string,mode,mode string,unit,unit string,zone"
    );
}

#[test]
fn instant_to_string_rejects_invalid_options_and_validates_units_before_time_zone_type() {
    assert_eq!(
        thrown("return new Temporal.Instant(0n).toString(1);"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Instant(0n).toString({fractionalSecondDigits:'invalid'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               smallestUnit:{get:function(){log.push('unit');return 'hour';}},
               timeZone:{get:function(){log.push('zone');return {toString:function(){log.push('coerce');return 'UTC';}}}}
             });
             try { new Temporal.Instant(0n).toString(options); }
             catch (error) { return error.name+'|'+log.join(','); }"
        ),
        "RangeError|unit,zone"
    );
    assert_eq!(
        rendered(
            "var called=false;
             try { new Temporal.Instant(0n).toString({timeZone:{toString:function(){called=true;return 'UTC';}}}); }
             catch (error) { return error.name+'|'+called; }"
        ),
        "TypeError|false"
    );
}

#[test]
fn duration_with_merges_defined_fields_in_normative_order() {
    assert_eq!(
        rendered(
            "var log=[],partial={};
             Object.defineProperty(partial,'days',{get:function(){log.push('get days');
               return {valueOf:function(){log.push('convert days');return 7;}};}});
             Object.defineProperty(partial,'hours',{get:function(){log.push('get hours');
               return undefined;}});
             var original=new Temporal.Duration(1,2,3,4,5,6,7,8,9,10);
             var result=original.with(partial);
             return [Temporal.Duration.prototype.with.length,original.toString(),
               result.toString(),result===original,log.join(',')].join('|');"
        ),
        "1|P1Y2M3W4DT5H6M7.00800901S|P1Y2M3W7DT5H6M7.00800901S|false|get days,convert days,get hours"
    );
    assert_eq!(
        thrown("return new Temporal.Duration().with({});"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(1).with({months:-1});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return Temporal.Duration.prototype.with.call({}, {days:1});"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_total_reads_relative_to_before_coercing_unit() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperty(options,'relativeTo',{get:function(){log.push('relativeTo');
               return undefined;}});
             Object.defineProperty(options,'unit',{get:function(){log.push('unit');return {
               toString:function(){log.push('unit toString');return 'hour';}};}});
             var duration=new Temporal.Duration(0,0,0,2,12);
             return [Temporal.Duration.prototype.total.length,duration.total(options),
               duration.total('minute'),new Temporal.Duration(0,1).total({
                 unit:'day',relativeTo:'2020-02-01'}),log.join(',')].join('|');"
        ),
        "1|60|3600|29|relativeTo,unit,unit toString"
    );
}

#[test]
fn duration_total_validates_receiver_options_and_unit() {
    assert_eq!(
        thrown("return Temporal.Duration.prototype.total.call({}, 'second');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().total({});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().total('auto');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(1).total('year');"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().total(1);"),
        ExceptionKind::TypeError
    );
}

#[test]
fn duration_round_supports_smallest_unit_modes_increments_and_relative_to() {
    assert_eq!(
        rendered(
            "var duration=Temporal.Duration.from('PT1H29M31S');
             return [Temporal.Duration.prototype.round.length,
               duration.round('minute').toString(),
               duration.round({smallestUnit:'minute',roundingMode:'trunc'}).toString(),
               duration.round({smallestUnit:'minute',roundingIncrement:15}).toString(),
               Temporal.Duration.from('PT26H').round('day').toString(),
               Temporal.Duration.from('P1M15D').round({smallestUnit:'day',relativeTo:'2020-02-01'}).toString()].join('|');"
        ),
        "1|PT1H30M|PT1H29M|PT1H30M|P1D|P1M15D"
    );
}

#[test]
fn duration_round_observes_options_and_coercions_in_specified_order() {
    assert_eq!(
        rendered(
            "var log=[],options={};
             Object.defineProperties(options,{
               largestUnit:{get:function(){log.push('get largestUnit');return {toString:function(){log.push('string largestUnit');return 'hour';}}}},
               relativeTo:{get:function(){log.push('get relativeTo');return undefined}},
               roundingIncrement:{get:function(){log.push('get roundingIncrement');return {valueOf:function(){log.push('number roundingIncrement');return 15;}}}},
               roundingMode:{get:function(){log.push('get roundingMode');return {toString:function(){log.push('string roundingMode');return 'halfExpand';}}}},
               smallestUnit:{get:function(){log.push('get smallestUnit');return {toString:function(){log.push('string smallestUnit');return 'minute';}}}}
             });
             return Temporal.Duration.from('PT1H29M31S').round(options).toString()+'|'+log.join(',');"
        ),
        "PT1H30M|get largestUnit,string largestUnit,get relativeTo,get roundingIncrement,number roundingIncrement,get roundingMode,string roundingMode,get smallestUnit,string smallestUnit"
    );
}

#[test]
fn duration_round_rejects_absent_invalid_and_unanchored_options() {
    assert_eq!(
        thrown("return Temporal.Duration.prototype.round.call({}, 'second');"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().round();"),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().round({});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration().round({smallestUnit:'auto'});"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration().round({smallestUnit:'minute',roundingIncrement:7});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration().round({smallestUnit:'minute',roundingMode:'invalid'});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(1).round({smallestUnit:'day'});"),
        ExceptionKind::RangeError
    );
}

#[test]
fn duration_relative_to_accepts_property_bags_in_observed_field_order() {
    assert_eq!(
        rendered(
            "var hours25=new Temporal.Duration(0,0,0,0,25);
             return [hours25.round({largestUnit:'days',relativeTo:{year:2019,month:11,day:2}}).toString(),
               new Temporal.Duration(0,1).total({unit:'day',relativeTo:{year:2020,month:2,day:1}}),
               Temporal.Duration.compare(new Temporal.Duration(0,0,0,1),new Temporal.Duration(0,0,0,0,24),{relativeTo:{year:2019,month:11,day:3}}),
               hours25.round({largestUnit:'days',relativeTo:{year:2019,month:11,day:2,timeZone:'UTC'}}).toString(),
               hours25.round({largestUnit:'days',relativeTo:{year:2019,month:11,day:2,hour:10,offset:'+00:00',timeZone:'UTC'}}).toString()].join('|');"
        ),
        "P1DT1H|29|0|P1DT1H|P1DT1H"
    );
    assert_eq!(
        rendered(
            "var log=[],relativeTo={};
             Object.defineProperty(relativeTo,'calendar',{get:function(){log.push('calendar');return 'iso8601';}});
             Object.defineProperty(relativeTo,'day',{get:function(){log.push('day');return 2;}});
             Object.defineProperty(relativeTo,'timeZone',{get:function(){log.push('timeZone');return undefined;}});
             Object.defineProperty(relativeTo,'year',{get:function(){log.push('year');return 2019;}});
             Object.defineProperty(relativeTo,'month',{get:function(){log.push('month');return 11;}});
             new Temporal.Duration(0,0,0,0,25).round({largestUnit:'days',relativeTo});
             return log.join(',');"
        ),
        "calendar,day,month,timeZone,year"
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{month:1,day:2}});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{year:2000,month:5,day:2,timeZone:1}});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{year:2000,month:5,day:2,timeZone:''}});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{year:2000,month:5,day:2,timeZone:'UTC',offset:0}});"
        ),
        ExceptionKind::TypeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{year:2000,month:5,day:2,timeZone:'UTC',offset:'00:00'}});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{year:2000,month:5,day:2,calendar:'1997-12-04[u-ca=notacal]'}});"
        ),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown(
            "return new Temporal.Duration(1).round({largestUnit:'months',relativeTo:{year:2000,month:5,day:2,hour:Infinity}});"
        ),
        ExceptionKind::RangeError
    );
}

#[test]
fn duration_constructor_rejects_infinite_fields_at_their_own_conversion() {
    assert_eq!(
        thrown("return new Temporal.Duration(Infinity);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        thrown("return new Temporal.Duration(0,0,0,0,0,0,0,0,0,-Infinity);"),
        ExceptionKind::RangeError
    );
    assert_eq!(
        rendered(
            "var log=[];
             try{new Temporal.Duration(0,{valueOf:function(){log.push('months');return Infinity;}},
               {valueOf:function(){log.push('weeks');return 0;}})}catch(error){log.push(error.name);}
             return log.join(',');"
        ),
        "months,RangeError"
    );
}
