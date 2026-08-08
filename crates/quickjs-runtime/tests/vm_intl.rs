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

#[test]
fn number_format_constructor_resolved_options_and_order_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var log=[];
             var names=['localeMatcher','numberingSystem','style','currency','currencyDisplay',
               'currencySign','unit','unitDisplay','notation','minimumIntegerDigits',
               'minimumFractionDigits','maximumFractionDigits','minimumSignificantDigits',
               'maximumSignificantDigits','roundingIncrement','roundingMode','roundingPriority',
               'trailingZeroDisplay','compactDisplay','useGrouping','signDisplay'];
             var values={localeMatcher:'lookup',numberingSystem:'latn',style:'decimal',
               notation:'standard',minimumIntegerDigits:1,minimumFractionDigits:1,
               maximumFractionDigits:2,roundingIncrement:1,roundingMode:'halfEven',
               roundingPriority:'auto',trailingZeroDisplay:'auto',compactDisplay:'short',
               useGrouping:false,signDisplay:'auto'};
             var options={};names.forEach(function(name){Object.defineProperty(options,name,{get:function(){
               log.push(name);return values[name]}})});
             var nf=new Intl.NumberFormat('de-DE',options);var ro=nf.resolvedOptions();
             var d=Object.getOwnPropertyDescriptor(Intl.NumberFormat.prototype,'format');
             return [Intl.NumberFormat.length,Intl.NumberFormat.name,
               Object.getPrototypeOf(nf)===Intl.NumberFormat.prototype,
               Object.keys(ro).join(','),ro.locale,ro.numberingSystem,ro.roundingMode,
               ro.useGrouping,nf.format(1234.25),d.get.name,d.enumerable,d.configurable,
               log.join(',')].join('|');"
        ),
        "0|NumberFormat|true|locale,numberingSystem,style,minimumIntegerDigits,minimumFractionDigits,maximumFractionDigits,useGrouping,notation,signDisplay,roundingIncrement,roundingMode,roundingPriority,trailingZeroDisplay|de-DE|latn|halfEven|false|1234,25|get format|false|true|localeMatcher,numberingSystem,style,currency,currencyDisplay,currencySign,unit,unitDisplay,notation,minimumIntegerDigits,minimumFractionDigits,maximumFractionDigits,minimumSignificantDigits,maximumSignificantDigits,roundingIncrement,roundingMode,roundingPriority,trailingZeroDisplay,compactDisplay,useGrouping,signDisplay"
    );
}

#[test]
fn number_format_preserves_exact_values_rounding_and_parts() {
    assert_eq!(
        rendered(
            "var nf=new Intl.NumberFormat('en-US',{useGrouping:false,minimumFractionDigits:2,
               maximumFractionDigits:2,roundingIncrement:25,roundingMode:'halfExpand'});
             var bound=nf.format;var parts=nf.formatToParts(1234.5);
             var exact=new Intl.NumberFormat('en-US',{useGrouping:false,maximumFractionDigits:20});
             return [bound===nf.format,bound.name,bound.length,nf.format(7.235),
               exact.format('9007199254740993.1234567890123456789'),
               parts.map(function(p){return p.type+':'+p.value}).join(','),
               parts.map(function(p){return p.value}).join('')===nf.format(1234.5)].join('|');"
        ),
        "true||1|7.25|9007199254740993.1234567890123456789|integer:1234,decimal:.,fraction:50|true"
    );
}

#[test]
fn number_format_supported_locales_styles_and_ranges_are_available() {
    assert_eq!(
        rendered(
            "var supported=Intl.NumberFormat.supportedLocalesOf(['tlh','de-DE','en-u-nu-arab'],{localeMatcher:'lookup'});
             var currency=new Intl.NumberFormat('en-US',{style:'currency',currency:'USD',currencySign:'accounting'});
             var unit=new Intl.NumberFormat('en-US',{style:'unit',unit:'kilometer-per-hour',unitDisplay:'long'});
             var scientific=new Intl.NumberFormat('de-DE',{notation:'scientific',maximumFractionDigits:2});
             var range=new Intl.NumberFormat('en-US',{style:'currency',currency:'USD',maximumFractionDigits:0});
             return [supported.join(','),currency.format(-987),unit.format(987),scientific.format(12345),
               range.formatRange(3,5),range.formatRange(3.1,3.4),
               Intl.NumberFormat.supportedLocalesOf.length].join('|');"
        ),
        "de-DE,en-u-nu-arab|($987.00)|987 kilometers per hour|1,23E4|$3 – $5|~$3|1"
    );
}

#[test]
fn number_format_legacy_chain_uses_a_hidden_fallback_symbol() {
    assert_eq!(
        rendered(
            "var receiver=Object.create(Intl.NumberFormat.prototype);
             var chained=Intl.NumberFormat.call(receiver,'de-DE',{useGrouping:false});
             var symbols=Object.getOwnPropertySymbols(chained);
             var fallback=symbols.filter(function(symbol){return symbol.description==='IntlLegacyConstructedSymbol'})[0];
             var descriptor=Object.getOwnPropertyDescriptor(chained,fallback);
             var accessed;
             var proxy=new Proxy(chained,{get:function(target,key){accessed=key;return target[key]}});
             var resolved=Intl.NumberFormat.prototype.resolvedOptions.call(proxy);
             var format=Object.getOwnPropertyDescriptor(Intl.NumberFormat.prototype,'format').get.call(proxy);
             var strictBrand;
             try{Intl.NumberFormat.prototype.formatToParts.call(proxy,1)}catch(error){strictBrand=error.name}
             return [chained===receiver,chained[fallback] instanceof Intl.NumberFormat,
               fallback.description,descriptor.writable,descriptor.enumerable,descriptor.configurable,
               typeof accessed,accessed===fallback,resolved.locale,format(1234),strictBrand].join('|');"
        ),
        "true|true|IntlLegacyConstructedSymbol|false|false|false|symbol|true|de-DE|1234|TypeError"
    );
}

#[test]
fn number_format_notations_localized_specials_and_part_boundaries_are_spec_shaped() {
    assert_eq!(
        rendered(
            "var parts=function(nf,value){return nf.formatToParts(value).map(function(p){return p.type+':'+p.value}).join(',')};
             var engineering=new Intl.NumberFormat('en-US',{notation:'engineering'});
             var scientific=new Intl.NumberFormat('en-US',{notation:'scientific'});
             var fraction=new Intl.NumberFormat('en-US',{useGrouping:false,minimumIntegerDigits:3,minimumFractionDigits:1,maximumFractionDigits:3});
             var compactZh=new Intl.NumberFormat('zh-TW',{notation:'compact'});
             var compactLong=new Intl.NumberFormat('en-US',{notation:'compact',compactDisplay:'long'});
             var percent=new Intl.NumberFormat('en-US',{style:'percent'});
             var currencyDe=new Intl.NumberFormat('de-DE',{style:'currency',currency:'USD',signDisplay:'always'});
             var unitJa=new Intl.NumberFormat('ja-JP',{style:'unit',unit:'kilometer-per-hour',unitDisplay:'long'});
             var range=new Intl.NumberFormat('en-US',{style:'currency',currency:'USD',maximumFractionDigits:0});
             var rangeParts=range.formatRangeToParts(3,5).map(function(p){return p.type+':'+p.value+':'+p.source}).join(',');
             return [engineering.format(0.000345),scientific.format(543211.1),
               fraction.format(-0.0001),compactZh.format(NaN),
               parts(compactLong,987654321),parts(percent,-123),parts(currencyDe,987),
               parts(unitJa,987),rangeParts].join('|');"
        ),
        "345E-6|5.432E5|-000.0|非數值|integer:988,literal: ,compact:million|minusSign:-,integer:12,group:,,integer:300,percentSign:%|plusSign:+,integer:987,decimal:,,fraction:00,literal: ,currency:$|unit:時速,literal: ,integer:987,literal: ,unit:キロメートル|currency:$:startRange,integer:3:startRange,literal: – :shared,currency:$:endRange,integer:5:endRange"
    );
}
