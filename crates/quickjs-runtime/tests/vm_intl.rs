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
fn intl_supported_values_of_is_spec_shaped_and_coerces_its_key() {
    assert_eq!(
        rendered(
            "var d=Object.getOwnPropertyDescriptor(Intl,'supportedValuesOf');
             var log=[];
             var first=Intl.supportedValuesOf({toString:function(){log.push('toString');return 'calendar'}});
             var second=Intl.supportedValuesOf('calendar');
             var range,type;
             try{Intl.supportedValuesOf('calendars')}catch(e){range=e.name}
             try{Intl.supportedValuesOf(Symbol())}catch(e){type=e.name}
             return [typeof Intl.supportedValuesOf,Intl.supportedValuesOf.length,
               Intl.supportedValuesOf.name,d.writable,d.enumerable,d.configurable,
               first!==second,Object.getPrototypeOf(first)===Array.prototype,
               log.join(','),range,type].join('|');"
        ),
        "function|1|supportedValuesOf|true|false|true|true|true|toString|RangeError|TypeError"
    );
}

#[test]
fn intl_supported_values_of_exposes_required_sorted_inventories() {
    assert_eq!(
        rendered(
            "var calendars=Intl.supportedValuesOf('calendar');
             var numbering=Intl.supportedValuesOf('numberingSystem');
             var units=Intl.supportedValuesOf('unit');
             return [calendars.join(','),numbering.includes('latn'),numbering.includes('tols'),
               numbering.length,units.length,units[0],units[units.length-1]].join('|');"
        ),
        "buddhist,chinese,coptic,dangi,ethioaa,ethiopic,gregory,hebrew,indian,islamic-civil,islamic-tbla,islamic-umalqura,iso8601,japanese,persian,roc|true|true|78|45|acre|year"
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

#[test]
fn locale_constructor_applies_options_in_spec_order_and_preserves_the_brand() {
    assert_eq!(
        rendered(
            "var log=[];
             var tag={toString:function(){log.push('tag');return 'und-Armn-SU-u-ca-islamicc'}};
             var options={
               get language(){log.push('language');return {toString:function(){log.push('language string');return 'ru'}}},
               get script(){log.push('script');return undefined},
               get region(){log.push('region');return undefined},
               get variants(){log.push('variants');return undefined},
               get calendar(){log.push('calendar');return 'gregory'},
               get collation(){log.push('collation');return undefined},
               get firstDayOfWeek(){log.push('firstDayOfWeek');return 1},
               get hourCycle(){log.push('hourCycle');return undefined},
               get caseFirst(){log.push('caseFirst');return undefined},
               get numeric(){log.push('numeric');return true},
               get numberingSystem(){log.push('numberingSystem');return 'latn'}
             };
             var locale=new Intl.Locale(tag,options);
             return [locale.toString(),locale.baseName,locale.language,locale.script,
               locale.region,locale.calendar,locale.firstDayOfWeek,locale.numeric,
               locale.numberingSystem,locale instanceof Intl.Locale,log.join(',')].join('|');"
        ),
        "ru-Armn-AM-u-ca-gregory-fw-mon-kn-nu-latn|ru-Armn-AM|ru|Armn|AM|gregory|mon|true|latn|true|tag,language,language string,script,region,variants,calendar,collation,firstDayOfWeek,hourCycle,caseFirst,numeric,numberingSystem"
    );
}

#[test]
fn locale_descriptors_subclassing_likely_subtags_and_locale_list_are_spec_shaped() {
    assert_eq!(
        rendered(
            "class CustomLocale extends Intl.Locale{}
             var locale=new CustomLocale('en-GB-u-ca-gregory');
             locale.toString=function(){throw Error('must not call')};
             var pd=Object.getOwnPropertyDescriptor(Intl.Locale,'prototype');
             var gd=Object.getOwnPropertyDescriptor(Intl.Locale.prototype,'baseName');
             var tagd=Object.getOwnPropertyDescriptor(Intl.Locale.prototype,Symbol.toStringTag);
             return [Intl.Locale.length,Intl.Locale.name,pd.writable,pd.enumerable,pd.configurable,
               gd.get.name,gd.set,gd.enumerable,gd.configurable,tagd.value,
               Object.getPrototypeOf(locale)===CustomLocale.prototype,
               Intl.getCanonicalLocales(locale)[0],
               Intl.Locale.prototype.maximize.call(locale).toString(),
               Intl.Locale.prototype.minimize.call(new Intl.Locale('en-Latn-GB')).toString()].join('|');"
        ),
        "1|Locale|false|false|false|get baseName||false|true|Intl.Locale|true|en-GB-u-ca-gregory|en-Latn-GB-u-ca-gregory|en-GB"
    );
}

#[test]
fn locale_info_methods_use_locale_data_and_create_fresh_spec_shaped_results() {
    assert_eq!(
        rendered(
            "var text=new Intl.Locale('ar').getTextInfo();
             var week=new Intl.Locale('en-US-u-fw-wed').getWeekInfo();
             var zones=new Intl.Locale('en-US').getTimeZones();
             var calendars=new Intl.Locale('th').getCalendars();
             calendars.push('changed');
             return [new Intl.Locale('th').getCalendars().join(','),
               new Intl.Locale('en-GB').getHourCycles().join(','),
               new Intl.Locale('und').getCollations().join(','),
               new Intl.Locale('en').getNumberingSystems().join(','),
               Object.keys(text).join(','),text.direction,
               Object.keys(week).join(','),week.firstDay,week.weekend.join(','),
               zones.length>0,zones[0]==='America/Adak'&&zones[zones.length-1]==='Pacific/Honolulu',
               new Intl.Locale('en').getTimeZones()===undefined,
               new Intl.Locale('en-u-ca-hebrew').getCalendars().join(','),
               new Intl.Locale('en-u-co-phonebk').getCollations().join(','),
               new Intl.Locale('en-u-hc-h11').getHourCycles().join(','),
               new Intl.Locale('en-u-nu-thai').getNumberingSystems().join(',')].join('|');"
        ),
        "buddhist,gregory|h23,h12|emoji,eor|latn|direction|rtl|firstDay,weekend|3|6,7|true|true|true|hebrew|phonebk|h11|thai"
    );
}

#[test]
fn collator_constructor_reads_options_in_order_and_resolves_unicode_extensions() {
    assert_eq!(
        rendered(
            "var log=[];
             var options={
               get usage(){log.push('usage');return 'sort'},
               get localeMatcher(){log.push('localeMatcher');return {toString:function(){log.push('localeMatcher string');return 'lookup'}}},
               get collation(){log.push('collation');return undefined},
               get numeric(){log.push('numeric');return true},
               get caseFirst(){log.push('caseFirst');return 'upper'},
               get sensitivity(){log.push('sensitivity');return 'base'},
               get ignorePunctuation(){log.push('ignorePunctuation');return true}
             };
             var collator=Intl.Collator.call({ignored:true},'de-u-co-phonebk-kf-lower-kn',options);
             var resolved=collator.resolvedOptions();
             var descriptor=Object.getOwnPropertyDescriptor(Intl,'Collator');
             return [Intl.Collator.length,Intl.Collator.name,descriptor.writable,
               descriptor.enumerable,descriptor.configurable,
               Object.getPrototypeOf(collator)===Intl.Collator.prototype,
               Object.keys(resolved).join(','),resolved.locale,resolved.usage,
               resolved.sensitivity,resolved.ignorePunctuation,resolved.collation,
               resolved.numeric,resolved.caseFirst,log.join(',')].join('|');"
        ),
        "0|Collator|true|false|true|true|locale,usage,sensitivity,ignorePunctuation,collation,numeric,caseFirst|de-u-co-phonebk-kn|sort|base|true|phonebk|true|upper|usage,localeMatcher,localeMatcher string,collation,numeric,caseFirst,sensitivity,ignorePunctuation"
    );
}

#[test]
fn collator_compare_is_cached_bound_and_observably_coerces_left_then_right() {
    assert_eq!(
        rendered(
            "var collator=new Intl.Collator('en',{sensitivity:'base',numeric:true});
             var compare=collator.compare;
             var log=[];
             var left={toString:function(){log.push('left');return 'A'}};
             var right={toString:function(){log.push('right');return 'á'}};
             var brand;
             try{Object.getOwnPropertyDescriptor(Intl.Collator.prototype,'compare').get.call({})}catch(e){brand=e.name}
             var search=new Intl.Collator('de',{usage:'search'});
             return [compare===collator.compare,compare.name,compare.length,
               Object.getOwnPropertyNames(compare).join(','),compare.call(null,left,right),
               compare('10','2')>0,log.join(','),brand,
               search.compare('AE','Ä')].join('|');"
        ),
        "true||2|length,name|0|true|left,right|TypeError|0"
    );
}

#[test]
fn collator_supported_locales_subclassing_and_default_options_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var log=[];
             var supported=Intl.Collator.supportedLocalesOf(['tlh','id','en-u-kn'],{
               get localeMatcher(){log.push('get');return {toString:function(){log.push('string');return 'lookup'}}}
             });
             class CustomCollator extends Intl.Collator{}
             var custom=new CustomCollator('en');
             Object.prototype.sensitivity='base';
             var defaultSensitivity=new Intl.Collator('en').resolvedOptions().sensitivity;
             delete Object.prototype.sensitivity;
             return [supported.join(','),log.join(','),
               Object.getPrototypeOf(custom)===CustomCollator.prototype,
               custom instanceof Intl.Collator,defaultSensitivity,
               Intl.Collator.supportedLocalesOf.length,
               Intl.Collator.supportedLocalesOf.name].join('|');"
        ),
        "id,en-u-kn|get,string|true|true|variant|1|supportedLocalesOf"
    );
}
