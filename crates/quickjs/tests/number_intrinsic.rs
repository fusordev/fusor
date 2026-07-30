use quickjs::{DynamicFunctionLimits, construct_dynamic_function};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{
    Context, EngineFault, ExceptionKind, ExecutionError, ExecutionLimits, JsException, JsNumber,
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

fn number(value: &JsValue) -> JsNumber {
    value.as_number().expect("live value").expect("Number")
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
fn number_call_construct_and_intrinsic_metadata_are_complete_for_the_core_vertical() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["value"],
        "let primitive=Number(value);\
         let boxed=new Number(value);\
         return primitive===value\
             && typeof boxed===\"object\"\
             && boxed.valueOf()===value\
             && boxed.toString()===primitive+\"\"\
             && new Number().valueOf()===0\
             && Number.prototype.valueOf()===0\
             && Number.prototype.constructor===Number\
             && Number.name===\"Number\"\
             && Number.length===1\
             && Number.prototype.valueOf.name===\"valueOf\"\
             && Number.prototype.valueOf.length===0\
             && Number.prototype.toString.name===\"toString\"\
             && Number.prototype.toString.length===1\
             && Number.prototype.toString.call(value,10)===primitive+\"\"\
             && Number.toString()===\"function Number() {\\n    [native code]\\n}\"\
             && Number.prototype.valueOf.toString()\
                 ===\"function valueOf() {\\n    [native code]\\n}\";",
    );

    for value in [
        JsNumber::from_i32(0),
        JsNumber::from_i32(42),
        JsNumber::from_f64(-3.5),
    ] {
        let argument = context.number(value);
        let result = context
            .call(&run, &[argument], ExecutionLimits::default())
            .expect("Number call and construction");
        assert!(boolean(&result), "{value:?}");
    }
}

#[test]
fn number_to_string_radix_coercion_and_non_decimal_formatting_remain_fail_closed() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    for body in ["return (15).toString(16);", "return (15).toString(\"10\");"] {
        let run = compile_function(&mut context, &[], body);
        assert!(matches!(
            context.call(&run, &[], ExecutionLimits::default()),
            Err(ExecutionError::EngineFault(EngineFault::RuntimeInvariant {
                message: "Number.prototype.toString radix coercion and non-decimal formatting are not implemented",
            }))
        ));
    }
}

#[test]
fn number_distinguishes_no_arguments_from_explicit_undefined() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let missing=Number();\
         let explicit=Number(void 0);\
         let boxedMissing=new Number().valueOf();\
         let boxedExplicit=new Number(void 0).valueOf();\
         return missing===0\
             && boxedMissing===0\
             && explicit!==explicit\
             && boxedExplicit!==boxedExplicit\
             && Number(null)===0\
             && Number(false)===0\
             && Number(true)===1\
             && Number(\"  0x10  \")===16;",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Number primitive conversion");
    assert!(boolean(&result));
}

#[test]
fn number_preserves_negative_zero_and_nan_through_wrappers() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let primitive=Number(-0);\
         let boxed=new Number(-0);\
         let nan=Number(void 0);\
         let boxedNan=new Number(void 0).valueOf();\
         return 1/primitive===-1/0\
             && 1/boxed.valueOf()===-1/0\
             && nan!==nan\
             && boxedNan!==boxedNan\
             && Number.prototype.toString.call(-0)===\"0\"\
             && Number.prototype.toString.call(nan)===\"NaN\";",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Number edge values");
    assert!(boolean(&result));
}

#[test]
fn number_conversion_is_resumable_and_uses_the_number_hint() {
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
                 return function convert(hint){log=log+hint;return \"7\";};\
             }\
         };\
         let fallback={\
             valueOf(){log=log+\"v\";return {};},\
             toString(){log=log+\"t\";return \"8\";}\
         };\
         let primitive=Number(exotic);\
         let boxed=new Number(fallback);\
         return primitive===7 && boxed.valueOf()===8 && log===\"gnumbervt\";",
    );
    let to_primitive = context
        .well_known_symbol(PredefinedAtom::SymbolToPrimitive)
        .expect("Symbol.toPrimitive");

    let result = context
        .call(&run, &[to_primitive], ExecutionLimits::default())
        .expect("resumable Number conversion");
    assert!(boolean(&result));
}

#[test]
fn number_conversion_abrupt_completion_stops_before_fallback() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let value={\
             valueOf(){throw 41;},\
             toString(){throw 42;}\
         };\
         return new Number(value);",
    );

    let exception = escaping_exception(context.call(&run, &[], ExecutionLimits::default()));
    assert_eq!(exception.kind(), None);
    assert!(
        number(exception.thrown_value().expect("explicit throw"))
            .strict_equals(JsNumber::from_i32(41))
    );
}

#[test]
fn number_symbol_and_receiver_errors_are_exact() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let symbol_conversion = compile_function(&mut context, &["symbol"], "return Number(symbol);");
    let wrong_object = compile_function(
        &mut context,
        &[],
        "return Number.prototype.valueOf.call({});",
    );
    let wrong_primitive = compile_function(
        &mut context,
        &[],
        "return Number.prototype.toString.call(true);",
    );
    let symbol = context
        .well_known_symbol(PredefinedAtom::SymbolIterator)
        .expect("Symbol.iterator");

    assert_type_error(
        context.call(&symbol_conversion, &[symbol], ExecutionLimits::default()),
        "cannot convert symbol to number",
    );
    assert_type_error(
        context.call(&wrong_object, &[], ExecutionLimits::default()),
        "not a number",
    );
    assert_type_error(
        context.call(&wrong_primitive, &[], ExecutionLimits::default()),
        "not a number",
    );
}

#[test]
fn number_prototype_methods_have_exact_nonconstructor_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let construct_value_of =
        compile_function(&mut context, &[], "return new Number.prototype.valueOf();");
    let construct_to_string =
        compile_function(&mut context, &[], "return new Number.prototype.toString();");

    assert_type_error(
        context.call(&construct_value_of, &[], ExecutionLimits::default()),
        "valueOf is not a constructor",
    );
    assert_type_error(
        context.call(&construct_to_string, &[], ExecutionLimits::default()),
        "toString is not a constructor",
    );
}

#[test]
fn number_primitive_lookup_and_sloppy_receiver_boxing_use_the_intrinsic() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "Number.prototype.marker=17;\
         Number.prototype.sloppy=function sloppyNumberReceiver(){\
             let first=this;\
             return typeof first===\"object\"\
                 && first===this\
                 && first.valueOf()===1;\
         };\
         Number.prototype.strict=function strictNumberReceiver(){\
             \"use strict\";\
             return typeof this===\"number\" && this===1;\
         };\
         return (1).marker===17\
             && (1).valueOf()===1\
             && (1).toString()===\"1\"\
             && (1).sloppy()\
             && (1).strict();",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("primitive Number property lookup and receiver binding");
    assert!(boolean(&result));
}

#[test]
fn number_wrappers_keep_identity_and_participate_in_conversion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let first=new Number(2);\
         let second=new Number(2);\
         first.marker=1;\
         return first!==second\
             && first.marker===1\
             && second.marker===void 0\
             && first+1===3\
             && new Number(2)==2;",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Number wrapper semantics");
    assert!(boolean(&result));
}

#[test]
fn object_prototype_tags_and_boxes_number_receivers() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let object={};\
         let boxed=object.valueOf.call(1);\
         let second=object.valueOf.call(1);\
         return object.toString.call(1)===\"[object Number]\"\
             && object.toString.call(boxed)===\"[object Number]\"\
             && boxed!==second\
             && typeof boxed===\"object\"\
             && boxed.valueOf()===1;",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Object.prototype Number boxing");
    assert!(boolean(&result));
}

#[test]
fn object_prototype_to_string_observes_number_to_string_tag() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["toStringTag"],
        "Number.prototype[toStringTag]=\"Tagged\";\
         let object={};\
         let stringTag=object.toString.call(1)===\"[object Tagged]\"\
             && object.toString.call(new Number(2))===\"[object Tagged]\";\
         Number.prototype[toStringTag]=7;\
         return stringTag\
             && object.toString.call(1)===\"[object Number]\"\
             && object.toString.call(new Number(2))===\"[object Number]\";",
    );
    let to_string_tag = context
        .well_known_symbol(PredefinedAtom::SymbolToStringTag)
        .expect("Symbol.toStringTag");

    let result = context
        .call(&run, &[to_string_tag], ExecutionLimits::default())
        .expect("Number @@toStringTag");
    assert!(boolean(&result));
}

#[test]
fn number_boxing_and_primitive_lookup_use_the_callee_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let home_realm = runtime.create_realm().expect("callee realm");
    let invoking_realm = runtime.create_realm().expect("caller realm");
    let target = {
        let mut context = runtime.context(&home_realm).expect("callee context");
        let setup = compile_function(
            &mut context,
            &[],
            "Number.prototype.realmMarker=11;return true;",
        );
        let setup_result = context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("callee realm setup");
        assert!(boolean(&setup_result));
        compile_function(&mut context, &[], "return this.realmMarker;")
    };
    let mut context = runtime.context(&invoking_realm).expect("caller context");
    let setup = compile_function(
        &mut context,
        &[],
        "Number.prototype.realmMarker=22;return true;",
    );
    let setup_result = context
        .call(&setup, &[], ExecutionLimits::default())
        .expect("caller realm setup");
    assert!(boolean(&setup_result));
    let bridge = compile_function(
        &mut context,
        &["target"],
        "return target.call(1)===11 && (1).realmMarker===22;",
    );

    let result = context
        .call(&bridge, &[target.as_value()], ExecutionLimits::default())
        .expect("cross-realm Number boxing");
    assert!(boolean(&result));
}

#[test]
fn strict_number_primitive_write_keeps_the_exact_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "\"use strict\";(1).marker=1;return (1).marker;",
    );

    assert_type_error(
        context.call(&run, &[], ExecutionLimits::default()),
        "not an object",
    );
}
