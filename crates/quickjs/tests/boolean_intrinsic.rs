use quickjs::{DynamicFunctionLimits, construct_dynamic_function};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{
    Context, ExceptionKind, ExecutionError, ExecutionLimits, JsException, JsNumber, JsValue,
    PredefinedAtom, Runtime, RuntimeLimits,
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
fn boolean_call_construct_and_intrinsic_metadata_are_complete() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["value"],
        "let primitive=Boolean(value);\
         let boxed=new Boolean(value);\
         return primitive===value\
             && typeof boxed===\"object\"\
             && boxed.valueOf()===value\
             && boxed.toString()===(value?\"true\":\"false\")\
             && new Boolean().valueOf()===false\
             && Boolean.prototype.valueOf()===false\
             && Boolean.prototype.constructor===Boolean\
             && Boolean.name===\"Boolean\"\
             && Boolean.length===1\
             && Boolean.prototype.valueOf.name===\"valueOf\"\
             && Boolean.prototype.valueOf.length===0\
             && Boolean.prototype.toString.name===\"toString\"\
             && Boolean.prototype.toString.length===0\
             && Boolean.toString()===\"function Boolean() {\\n    [native code]\\n}\"\
             && Boolean.prototype.valueOf.toString()\
                 ===\"function valueOf() {\\n    [native code]\\n}\";",
    );

    for value in [false, true] {
        let argument = context.boolean(value);
        let result = context
            .call(&run, &[argument], ExecutionLimits::default())
            .expect("Boolean call and construction");
        assert!(boolean(&result), "{value}");
    }
}

#[test]
fn boolean_uses_javascript_truthiness_without_observable_conversion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["symbol"],
        "let object={valueOf(){throw 1;},toString(){throw 2;}};\
         return Boolean()===false\
             && Boolean(void 0)===false\
             && Boolean(null)===false\
             && Boolean(0)===false\
             && Boolean(-0)===false\
             && Boolean(0/0)===false\
             && Boolean(\"\")===false\
             && Boolean(\"0\")===true\
             && Boolean(object)===true\
             && Boolean(symbol)===true;",
    );
    let symbol = context
        .well_known_symbol(PredefinedAtom::SymbolIterator)
        .expect("Symbol.iterator");

    let result = context
        .call(&run, &[symbol], ExecutionLimits::default())
        .expect("Boolean truthiness");
    assert!(boolean(&result));
}

#[test]
fn boolean_prototype_lookup_preserves_primitive_method_receivers() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "Boolean.prototype.marker=17;\
         true.marker=99;\
         return true.marker===17\
             && true.valueOf()===true\
             && false.toString()===\"false\";",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("primitive Boolean property lookup");
    assert!(boolean(&result));
}

#[test]
fn boolean_brand_is_an_internal_slot_with_exact_receiver_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let wrong_object = compile_function(
        &mut context,
        &[],
        "return Boolean.prototype.valueOf.call({});",
    );
    let wrong_primitive = compile_function(
        &mut context,
        &[],
        "return Boolean.prototype.toString.call(0);",
    );

    assert_type_error(
        context.call(&wrong_object, &[], ExecutionLimits::default()),
        "not a boolean",
    );
    assert_type_error(
        context.call(&wrong_primitive, &[], ExecutionLimits::default()),
        "not a boolean",
    );
}

#[test]
fn boolean_prototype_methods_have_exact_nonconstructor_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let construct_value_of =
        compile_function(&mut context, &[], "return new Boolean.prototype.valueOf();");
    let construct_to_string = compile_function(
        &mut context,
        &[],
        "return new Boolean.prototype.toString();",
    );

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
fn host_calling_an_unbound_boolean_method_throws_the_javascript_receiver_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let extract_value_of = compile_function(&mut context, &[], "return Boolean.prototype.valueOf;");
    let extract_to_string =
        compile_function(&mut context, &[], "return Boolean.prototype.toString;");
    let value_of = context
        .call(&extract_value_of, &[], ExecutionLimits::default())
        .expect("extract valueOf")
        .into_function()
        .expect("valueOf function");
    let to_string = context
        .call(&extract_to_string, &[], ExecutionLimits::default())
        .expect("extract toString")
        .into_function()
        .expect("toString function");

    assert_type_error(
        context.call(&value_of, &[], ExecutionLimits::default()),
        "not a boolean",
    );
    assert_type_error(
        context.call(&to_string, &[], ExecutionLimits::default()),
        "not a boolean",
    );
}

#[test]
fn object_prototype_tags_and_boxes_boolean_receivers() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let object={};\
         let boxed=object.valueOf.call(true);\
         let second=object.valueOf.call(true);\
         return object.toString.call(true)===\"[object Boolean]\"\
             && object.toString.call(boxed)===\"[object Boolean]\"\
             && boxed!==second\
             && typeof boxed===\"object\"\
             && boxed.valueOf()===true;",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Object.prototype Boolean boxing");
    assert!(boolean(&result));
}

#[test]
fn sloppy_boolean_receivers_box_once_while_strict_receivers_stay_primitive() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "Boolean.prototype.sloppy=function sloppyBooleanReceiver(){\
             let first=this;\
             return typeof first===\"object\"\
                 && first===this\
                 && first.valueOf()===true;\
         };\
         Boolean.prototype.strict=function strictBooleanReceiver(){\
             \"use strict\";\
             return typeof this===\"boolean\" && this===true;\
         };\
         return true.sloppy() && true.strict();",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("strict and sloppy Boolean receiver binding");
    assert!(boolean(&result));
}

#[test]
fn boolean_wrappers_participate_in_primitive_conversion_and_keep_identity() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let first=new Boolean(false);\
         let second=new Boolean(false);\
         first.marker=1;\
         return first!==second\
             && first.marker===1\
             && second.marker===void 0\
             && first+1===1\
             && new Boolean(true)==true;",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Boolean wrapper semantics");
    assert!(boolean(&result));
}

#[test]
fn boolean_wrapper_conversion_observes_prototype_mutation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "let original=Boolean.prototype.valueOf;\
         Boolean.prototype.valueOf=function replacement(){return 7;};\
         let result=+new Boolean(false)===7;\
         Boolean.prototype.valueOf=original;\
         return result;",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Boolean wrapper conversion");
    assert!(boolean(&result));
}

#[test]
fn object_prototype_to_string_observes_boolean_to_string_tag_data_property() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["toStringTag"],
        "Boolean.prototype[toStringTag]=\"Tagged\";\
         let object={};\
         let stringTag=object.toString.call(true)===\"[object Tagged]\"\
             && object.toString.call(new Boolean(false))===\"[object Tagged]\";\
         Boolean.prototype[toStringTag]=7;\
         return stringTag\
             && object.toString.call(true)===\"[object Boolean]\"\
             && object.toString.call(new Boolean(false))===\"[object Boolean]\";",
    );
    let to_string_tag = context
        .well_known_symbol(PredefinedAtom::SymbolToStringTag)
        .expect("Symbol.toStringTag");

    let result = context
        .call(&run, &[to_string_tag], ExecutionLimits::default())
        .expect("Boolean @@toStringTag");
    assert!(boolean(&result));
}

#[test]
fn object_prototype_to_string_executes_symbol_tag_getters_once_and_falls_back() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["toStringTag"],
        "let reads=0;\
         let tagged={get [toStringTag](){reads=reads+1;return \"Tagged\";}};\
         let fallback={get [toStringTag](){reads=reads+1;return 7;}};\
         let object={};\
         return object.toString.call(tagged)===\"[object Tagged]\"\
             && object.toString.call(fallback)===\"[object Object]\"\
             && reads===2;",
    );
    let to_string_tag = context
        .well_known_symbol(PredefinedAtom::SymbolToStringTag)
        .expect("Symbol.toStringTag");

    let result = context
        .call(&run, &[to_string_tag], ExecutionLimits::default())
        .expect("accessor-backed @@toStringTag");
    assert!(boolean(&result));
}

#[test]
fn object_prototype_to_string_propagates_symbol_tag_getter_throw() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &["toStringTag"],
        "let tagged={get [toStringTag](){throw 37;}};\
         return ({}).toString.call(tagged);",
    );
    let to_string_tag = context
        .well_known_symbol(PredefinedAtom::SymbolToStringTag)
        .expect("Symbol.toStringTag");

    let exception =
        escaping_exception(context.call(&run, &[to_string_tag], ExecutionLimits::default()));
    assert_eq!(exception.kind(), None);
    let thrown = exception.thrown_value().expect("explicit getter throw");
    let number = thrown
        .as_number()
        .expect("live thrown value")
        .expect("number throw");
    assert!(number.strict_equals(JsNumber::from_i32(37)));
}

#[test]
fn boolean_boxing_and_primitive_lookup_use_the_callee_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let home_realm = runtime.create_realm().expect("callee realm");
    let invoking_realm = runtime.create_realm().expect("caller realm");
    let target = {
        let mut context = runtime.context(&home_realm).expect("callee context");
        let setup = compile_function(
            &mut context,
            &[],
            "Boolean.prototype.realmMarker=11;return true;",
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
        "Boolean.prototype.realmMarker=22;return true;",
    );
    let setup_result = context
        .call(&setup, &[], ExecutionLimits::default())
        .expect("caller realm setup");
    assert!(boolean(&setup_result));
    let bridge = compile_function(
        &mut context,
        &["target"],
        "return target.call(true)===11 && true.realmMarker===22;",
    );

    let result = context
        .call(&bridge, &[target.as_value()], ExecutionLimits::default())
        .expect("cross-realm Boolean boxing");
    assert!(boolean(&result));
}

#[test]
fn strict_boolean_primitive_write_keeps_the_exact_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = compile_function(
        &mut context,
        &[],
        "\"use strict\";true.marker=1;return true.marker;",
    );

    assert_type_error(
        context.call(&run, &[], ExecutionLimits::default()),
        "not an object",
    );
}
