use quickjs::{DynamicFunctionLimits, construct_dynamic_function};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{
    Context, ExceptionKind, ExecutionError, ExecutionLimits, JsException, JsNumber, JsString,
    JsValue, PredefinedAtom, Runtime, RuntimeLimits,
};

fn compile_function(
    context: &mut Context<'_>,
    parameters: &[&str],
    body: &str,
) -> quickjs_runtime::Function {
    let parameters = parameters
        .iter()
        .map(|parameter| SourceFragment::new(parameter))
        .collect::<Vec<_>>();
    construct_dynamic_function(
        context,
        DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new(body),
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("ordinary function")
}

fn boolean(value: &JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn string(value: &JsValue) -> JsString {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .clone()
}

fn escaping_exception(result: Result<JsValue, ExecutionError>) -> JsException {
    match result {
        Err(ExecutionError::Exception(exception)) => exception,
        Err(error) => panic!("expected escaping JavaScript exception, found {error:?}"),
        Ok(value) => panic!("expected escaping JavaScript exception, returned {value:?}"),
    }
}

fn assert_type_error(result: Result<JsValue, ExecutionError>, expected_message: &str) {
    let exception = escaping_exception(result);
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        expected_message
    );
}

#[test]
fn string_call_construct_and_intrinsic_metadata_are_complete_for_the_core_vertical() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["value"],
        "let primitive=String(value);\
         let boxed=new String(value);\
         return primitive===value\
             && typeof boxed===\"object\"\
             && boxed.valueOf()===value\
             && boxed.toString()===value\
             && boxed.length===value.length\
             && String() === \"\"\
             && new String().valueOf()===\"\"\
             && String.prototype.valueOf()===\"\"\
             && String.prototype.length===0\
             && String.prototype.constructor===String\
             && String.name===\"String\"\
             && String.length===1\
             && String.prototype.valueOf.name===\"valueOf\"\
             && String.prototype.valueOf.length===0\
             && String.prototype.toString.name===\"toString\"\
             && String.prototype.toString.length===0\
             && String.toString()===\"function String() {\\n    [native code]\\n}\"\
             && String.prototype.valueOf.toString()\
                 ===\"function valueOf() {\\n    [native code]\\n}\";",
    );
    let argument = context.string(JsString::from_utf8("Aé").expect("String"));

    let result = context
        .call(&run, &[argument], ExecutionLimits::default())
        .expect("String call and construction");
    assert!(boolean(&result));
}

#[test]
fn string_conversion_is_resumable_and_uses_the_string_hint() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["toPrimitive"],
        "let log=\"\";\
         let exotic={\
             get [toPrimitive](){\
                 log=log+\"g\";\
                 return function convert(hint){log=log+hint;return \"first\";};\
             }\
         };\
         let fallback={\
             toString(){log=log+\"t\";return {};},\
             valueOf(){log=log+\"v\";return \"second\";}\
         };\
         return String(exotic)+\"|\"+new String(fallback).valueOf()+\"|\"+log;",
    );
    let to_primitive = context
        .well_known_symbol(PredefinedAtom::SymbolToPrimitive)
        .expect("Symbol.toPrimitive");

    let result = context
        .call(&run, &[to_primitive], ExecutionLimits::default())
        .expect("resumable String conversion");
    assert_eq!(
        string(&result),
        JsString::from_utf8("first|second|gstringtv").expect("expected")
    );
}

#[test]
fn string_conversion_preserves_abrupt_completion_and_rejects_nonprimitive_results() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let thrower = compile_function(
        &mut context,
        &[],
        "stringFallbackReached=0;\
         let value={\
             toString(){throw 23;},\
             valueOf(){stringFallbackReached=1;return \"unreachable\";}\
         };\
         return String(value);",
    );

    let exception = escaping_exception(context.call(&thrower, &[], ExecutionLimits::default()));
    assert_eq!(exception.kind(), None);
    assert!(
        exception
            .thrown_value()
            .expect("explicit throw")
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(23))
    );
    let read_marker = compile_function(&mut context, &[], "return stringFallbackReached;");
    let marker = context
        .call(&read_marker, &[], ExecutionLimits::default())
        .expect("conversion marker");
    assert!(
        marker
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(0))
    );

    let ordinary = compile_function(
        &mut context,
        &[],
        "return String({toString(){return {};},valueOf(){return {};}});",
    );
    assert_type_error(
        context.call(&ordinary, &[], ExecutionLimits::default()),
        "toPrimitive",
    );
    let exotic = compile_function(
        &mut context,
        &["toPrimitive"],
        "return String({[toPrimitive](){return {};}});",
    );
    let to_primitive = context
        .well_known_symbol(PredefinedAtom::SymbolToPrimitive)
        .expect("Symbol.toPrimitive");
    assert_type_error(
        context.call(&exotic, &[to_primitive], ExecutionLimits::default()),
        "toPrimitive",
    );
}

#[test]
fn string_call_has_the_symbol_special_case_but_construction_does_not() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let stringify = compile_function(&mut context, &["value"], "return String(value);");
    let construct = compile_function(&mut context, &["value"], "return new String(value);");
    let missing = context.symbol(None).expect("symbol without description");
    let described = context
        .symbol(Some(&JsString::from_utf8("token").expect("description")))
        .expect("described symbol");
    let iterator = context
        .well_known_symbol(PredefinedAtom::SymbolIterator)
        .expect("Symbol.iterator");

    let missing_text = context
        .call(
            &stringify,
            std::slice::from_ref(&missing),
            ExecutionLimits::default(),
        )
        .expect("String(Symbol())");
    assert_eq!(
        string(&missing_text),
        JsString::from_utf8("Symbol()").expect("expected")
    );
    let described_text = context
        .call(
            &stringify,
            std::slice::from_ref(&described),
            ExecutionLimits::default(),
        )
        .expect("String(Symbol(description))");
    assert_eq!(
        string(&described_text),
        JsString::from_utf8("Symbol(token)").expect("expected")
    );
    let iterator_text = context
        .call(&stringify, &[iterator], ExecutionLimits::default())
        .expect("String(Symbol.iterator)");
    assert_eq!(
        string(&iterator_text),
        JsString::from_utf8("Symbol(Symbol.iterator)").expect("expected")
    );
    assert_type_error(
        context.call(&construct, &[missing], ExecutionLimits::default()),
        "cannot convert symbol to string",
    );
    assert_type_error(
        context.call(&construct, &[described], ExecutionLimits::default()),
        "cannot convert symbol to string",
    );
}

#[test]
fn string_wrapper_and_primitive_index_properties_are_utf16_exotics() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["value", "high", "low"],
        "String.prototype[0]=\"prototype\";\
         let boxed=new String(value);\
         boxed.extra=9;\
         boxed[0]=\"changed\";\
         boxed.length=99;\
         return value.length===4\
             && value[0]===\"A\"\
             && value[1]===high\
             && value[2]===low\
             && value[3]===\"Z\"\
             && value[4]===void 0\
             && boxed.length===4\
             && boxed[0]===\"A\"\
             && boxed[1]===high\
             && boxed[2]===low\
             && boxed.extra===9\
             && \"\"[0]===\"prototype\";",
    );
    let value = context.string(
        JsString::from_code_units([u16::from(b'A'), 0xd83d, 0xde00, u16::from(b'Z')])
            .expect("UTF-16 String"),
    );
    let high = context.string(JsString::from_code_units([0xd83d]).expect("high surrogate"));
    let low = context.string(JsString::from_code_units([0xde00]).expect("low surrogate"));

    let result = context
        .call(&run, &[value, high, low], ExecutionLimits::default())
        .expect("String exotic properties");
    assert!(boolean(&result));
}

#[test]
fn strict_string_writes_distinguish_primitive_and_wrapper_properties() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    for (body, message) in [
        ("\"use strict\";\"ab\"[0]=\"z\";", "not an object"),
        ("\"use strict\";\"ab\".length=3;", "'length' is read-only"),
        (
            "\"use strict\";let value=new String(\"ab\");value[0]=\"z\";",
            "'0' is read-only",
        ),
        (
            "\"use strict\";let value=new String(\"ab\");value.length=3;",
            "'length' is read-only",
        ),
    ] {
        let run = compile_function(&mut context, &[], body);
        assert_type_error(context.call(&run, &[], ExecutionLimits::default()), message);
    }
    let extra = compile_function(
        &mut context,
        &[],
        "\"use strict\";let value=new String(\"ab\");value.extra=7;return value.extra===7;",
    );
    let result = context
        .call(&extra, &[], ExecutionLimits::default())
        .expect("ordinary String wrapper property");
    assert!(boolean(&result));
}

#[test]
fn string_brand_methods_reject_other_receivers_and_are_not_constructors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    for body in [
        "return String.prototype.valueOf.call({});",
        "return String.prototype.toString.call(0);",
        "return String.prototype.valueOf.call(true);",
    ] {
        let run = compile_function(&mut context, &[], body);
        assert_type_error(
            context.call(&run, &[], ExecutionLimits::default()),
            "not a string",
        );
    }
    for (body, message) in [
        (
            "return new String.prototype.valueOf();",
            "valueOf is not a constructor",
        ),
        (
            "return new String.prototype.toString();",
            "toString is not a constructor",
        ),
    ] {
        let run = compile_function(&mut context, &[], body);
        assert_type_error(context.call(&run, &[], ExecutionLimits::default()), message);
    }
}

#[test]
fn primitive_string_lookup_and_sloppy_receiver_boxing_use_the_intrinsic() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "String.prototype.marker=17;\
         String.prototype.sloppy=function sloppyStringReceiver(){\
             let first=this;\
             return typeof first===\"object\"\
                 && first===this\
                 && first.valueOf()===\"xy\"\
                 && first.length===2\
                 && first[0]===\"x\";\
         };\
         String.prototype.strict=function strictStringReceiver(){\
             \"use strict\";\
             return typeof this===\"string\" && this===\"xy\";\
         };\
         return \"xy\".marker===17\
             && \"xy\".valueOf()===\"xy\"\
             && \"xy\".toString()===\"xy\"\
             && \"xy\".sloppy()\
             && \"xy\".strict();",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("primitive String lookup and receiver binding");
    assert!(boolean(&result));
}

#[test]
fn object_prototype_boxes_and_tags_string_receivers() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["toStringTag"],
        "let object={};\
         let boxed=object.valueOf.call(\"xy\");\
         let second=object.valueOf.call(\"xy\");\
         let base=object.toString.call(\"xy\")===\"[object String]\"\
             && object.toString.call(boxed)===\"[object String]\"\
             && boxed!==second\
             && boxed.valueOf()===\"xy\"\
             && boxed.length===2\
             && boxed[1]===\"y\";\
         String.prototype[toStringTag]=\"Tagged\";\
         let tagged=object.toString.call(\"xy\")===\"[object Tagged]\"\
             && object.toString.call(boxed)===\"[object Tagged]\";\
         String.prototype[toStringTag]=7;\
         return base && tagged\
             && object.toString.call(\"xy\")===\"[object String]\";",
    );
    let to_string_tag = context
        .well_known_symbol(PredefinedAtom::SymbolToStringTag)
        .expect("Symbol.toStringTag");

    let result = context
        .call(&run, &[to_string_tag], ExecutionLimits::default())
        .expect("Object.prototype String boxing and tagging");
    assert!(boolean(&result));
}

#[test]
fn string_boxing_and_construction_use_the_callee_and_new_target_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let home_realm = runtime.create_realm().expect("home realm");
    let invoking_realm = runtime.create_realm().expect("invoking realm");
    let (target, constructor, boxed, method) = {
        let mut context = runtime.context(&home_realm).expect("home context");
        let setup = compile_function(
            &mut context,
            &[],
            "String.prototype.realmMarker=11;return true;",
        );
        assert!(boolean(
            &context
                .call(&setup, &[], ExecutionLimits::default())
                .expect("home setup")
        ));
        let target = compile_function(&mut context, &[], "return this.realmMarker;");
        let extract_constructor = compile_function(&mut context, &[], "return String;");
        let constructor = context
            .call(&extract_constructor, &[], ExecutionLimits::default())
            .expect("String constructor")
            .into_function()
            .expect("constructor function");
        let construct = compile_function(&mut context, &[], "return new String(\"home\");");
        let boxed = context
            .call(&construct, &[], ExecutionLimits::default())
            .expect("home wrapper");
        let extract_method =
            compile_function(&mut context, &[], "return String.prototype.valueOf;");
        let method = context
            .call(&extract_method, &[], ExecutionLimits::default())
            .expect("String.prototype.valueOf")
            .into_function()
            .expect("valueOf method");
        (target, constructor, boxed, method)
    };
    let mut context = runtime.context(&invoking_realm).expect("invoking context");
    let setup = compile_function(
        &mut context,
        &[],
        "String.prototype.realmMarker=22;return true;",
    );
    assert!(boolean(
        &context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("invoking setup")
    ));
    let bridge = compile_function(
        &mut context,
        &["target", "ctor", "boxed", "method"],
        "let constructed=new ctor(\"remote\");\
         let local=new String(\"local\");\
         return target.call(\"xy\")===11\
             && \"xy\".realmMarker===22\
             && constructed.realmMarker===11\
             && method.call(boxed)===\"home\"\
             && String.prototype.valueOf.call(boxed)===\"home\"\
             && method.call(local)===\"local\"\
             && method.call(\"primitive\")===\"primitive\";",
    );

    let result = context
        .call(
            &bridge,
            &[
                target.as_value(),
                constructor.as_value(),
                boxed,
                method.as_value(),
            ],
            ExecutionLimits::default(),
        )
        .expect("cross-realm String behavior");
    assert!(boolean(&result));
}
