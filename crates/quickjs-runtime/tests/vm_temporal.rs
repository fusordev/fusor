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
