//! `Array.prototype.flat`, `flatMap`, species creation, and `FlattenIntoArray`.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits,
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
        let parameter_texts = source
            .parameters()
            .iter()
            .map(|parameter| parameter.to_utf8_lossy().map_err(engine_failure))
            .collect::<Result<Vec<_>, _>>()?;
        let parameters = parameter_texts
            .iter()
            .map(|parameter| SourceFragment::new(parameter.as_str()))
            .collect::<Vec<_>>();
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime Array flattening>"),
                )
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
    dynamic_function_with_parameters(context, &[], body)
}

fn dynamic_function_with_parameters(
    context: &mut Context<'_>,
    parameters: &[&str],
    body: &str,
) -> Function {
    let parameters = parameters
        .iter()
        .map(|parameter| JsString::from_utf8(parameter).expect("parameter"))
        .collect::<Vec<_>>();
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from(parameters),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
}

fn rendered(expression: &str) -> String {
    evaluate(&format!("return String({expression});"), |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn thrown(body: &str) -> (ExceptionKind, String) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        let kind = exception.kind().expect("engine exception kind");
        let message = exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8");
        (kind, message)
    })
}

fn assert_throws(body: &str, kind: ExceptionKind, message: &str) {
    assert_eq!((kind, message.to_owned()), thrown(body), "{body}");
}

fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

#[test]
fn flat_uses_depth_first_array_only_flattening_and_skips_holes() {
    assert_all(&[
        ("JSON.stringify([1,[2,,[3]],,4].flat(2))", "[1,2,3,4]"),
        ("JSON.stringify([1,,[2]].flat(0))", "[1,[2]]"),
        ("JSON.stringify([1,[2,[3]]].flat(-1))", "[1,[2,[3]]]"),
        ("JSON.stringify([1,[2,[3]]].flat(Infinity))", "[1,2,3]"),
        (
            "(function(){\
                const array=[1];array[Symbol.isConcatSpreadable]=false;\
                const object={0:2,length:1,[Symbol.isConcatSpreadable]:true};\
                const result=[array,object].flat();\
                return result.length+'|'+result[0]+'|'+(result[1]===object);\
            })()",
            "2|1|true",
        ),
        (
            "(function(){let log='';const nested=new Proxy([2,3],{\
                has(target,key){log+='h'+key+',';return Reflect.has(target,key);},\
                get(target,key,receiver){log+='g'+key+',';\
                  return Reflect.get(target,key,receiver);}\
              });const result=[1,nested].flat();\
              return JSON.stringify(result)+'|'+log;})()",
            "[1,2,3]|glength,h0,g0,h1,g1,",
        ),
    ]);
}

#[test]
fn flat_map_calls_only_present_root_elements_and_flattens_one_level() {
    assert_all(&[
        (
            "(function(){\
                let log='';const source=[1,,2];const receiver={tag:'this'};\
                const result=source.flatMap(function(value,index,object){\
                    log+=value+','+index+','+(object===source)+','+(this===receiver)+'|';\
                    return index?[value,[9]]:[value,,value+10];\
                },receiver);\
                return log+'#'+JSON.stringify(result);\
            })()",
            "1,0,true,true|2,2,true,true|#[1,11,2,[9]]",
        ),
        (
            "(function(){const object={0:'x',length:1};const result=[1].flatMap(function(){return object;});return result.length+'|'+(result[0]===object);})()",
            "1|true",
        ),
    ]);
}

#[test]
fn conversions_callback_validation_and_species_follow_specification_order() {
    assert_all(&[
        (
            "(function(){\
                let log='';const source={\
                    get length(){log+='length|';return 1;},\
                    get 0(){log+='get0';return 1;}\
                };\
                Array.prototype.flat.call(source,{valueOf(){log+='depth|';return 1;}});\
                return log;\
            })()",
            "length|depth|get0",
        ),
        (
            "(function(){\
                let log='';const source=[];source.length=1;\
                Object.defineProperty(source,0,{get(){log+='get0';return 1;}});\
                function Species(length){log+='construct:'+length+'|';return {};};\
                Object.defineProperty(source,'constructor',{get(){\
                    log+='constructor|';return {get [Symbol.species](){log+='species|';return Species;}};\
                }});\
                source.flat({valueOf(){log+='depth|';return 1;}});return log;\
            })()",
            "depth|constructor|species|construct:0|get0",
        ),
        (
            "(function(){\
                let log='';const source={get length(){log+='length|';return 1;}};\
                try{Array.prototype.flatMap.call(source,1);}catch(error){log+=error.name;}\
                return log;\
            })()",
            "length|TypeError",
        ),
        (
            "(function(){\
                let log='';const source=[1];\
                Object.defineProperty(source,'constructor',{get(){log+='constructor';return Array;}});\
                try{source.flatMap(1);}catch(error){}return log;\
            })()",
            "",
        ),
    ]);
}

#[test]
fn array_species_create_honors_custom_null_and_invalid_species() {
    assert_all(&[
        ("Array[Symbol.species]===Array", "true"),
        (
            "(function(){\
                let target;function Species(length){target={created:length};return target;}\
                const source=[1,[2]];source.constructor={[Symbol.species]:Species};\
                const result=source.flat();\
                return (result===target)+'|'+result.created+'|'+result[0]+'|'+result[1]+'|'+result.length;\
            })()",
            "true|0|1|2|undefined",
        ),
        (
            "(function(){const source=[1];source.constructor={[Symbol.species]:null};return Array.isArray(source.flat());})()",
            "true",
        ),
        (
            "(function(){\
                let reads=0;Object.defineProperty(Array,Symbol.species,{get(){reads=reads+1;return null;},configurable:true});\
                const result=[1].flat();return reads+'|'+Array.isArray(result);\
            })()",
            "1|true",
        ),
        (
            "(function(){let read=false;const source={length:1,0:1,get constructor(){read=true;return null;}};Array.prototype.flat.call(source);return read;})()",
            "false",
        ),
    ]);
    assert_throws(
        "const source=[1];source.constructor={[Symbol.species]:Array.prototype.flat};return source.flat();",
        ExceptionKind::TypeError,
        "not a constructor",
    );
    assert_throws(
        "const source=[1];source.constructor=1;return source.flat();",
        ExceptionKind::TypeError,
        "not a constructor",
    );
}

#[test]
fn foreign_intrinsic_array_constructor_falls_back_to_the_current_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let source_realm = runtime.create_realm().expect("source realm");
    let method_realm = runtime.create_realm().expect("method realm");
    let source = {
        let mut context = runtime.context(&source_realm).expect("source context");
        let make = dynamic_function(&mut context, "return [1,[2]];");
        context
            .call(&make, &[], ExecutionLimits::default())
            .expect("foreign Array")
    };
    let mut context = runtime.context(&method_realm).expect("method context");
    let flatten = dynamic_function_with_parameters(
        &mut context,
        &["value"],
        "const result=Array.prototype.flat.call(value);\
         return Object.getPrototypeOf(result)===Array.prototype&&result.join(',')==='1,2';",
    );
    let result = context
        .call(&flatten, &[source], ExecutionLimits::default())
        .expect("cross-realm flat");
    assert_eq!(result.as_boolean().expect("live result"), Some(true));
}

#[test]
fn flattening_preserves_completed_target_writes_on_abrupt_completion() {
    assert_all(&[(
        "(function(){\
            let target;function Species(){target={};return target;}\
            const source=[1,2];source.constructor={[Symbol.species]:Species};\
            try{source.flatMap(function(value){if(value===2)throw new Error('stop');return [value];});}catch(error){}\
            return target[0]+'|'+Object.prototype.hasOwnProperty.call(target,1);\
        })()",
        "1|false",
    )]);
    assert_throws(
        "function Species(){return Object.preventExtensions({});}\
         const source=[1];source.constructor={[Symbol.species]:Species};return source.flat();",
        ExceptionKind::TypeError,
        "object is not extensible",
    );
}

#[test]
fn flattening_uses_an_explicit_stack_and_shared_instruction_fuel() {
    assert_all(&[(
        "(function(){let value=1;for(let index=0;index<2000;index=index+1)value=[value];return value.flat(Infinity)[0];})()",
        "1",
    )]);

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let scan = dynamic_function(
        &mut context,
        "return Array.prototype.flat.call({length:1000});",
    );
    let result = context.call(
        &scan,
        &[],
        ExecutionLimits::default().with_instruction_fuel(100),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}

#[test]
fn flattening_methods_box_primitives_reject_nullish_receivers_and_have_exact_shape() {
    assert_all(&[
        ("Array.prototype.flat.call('ab').join('')", "ab"),
        ("Array.prototype.flat.length", "0"),
        ("Array.prototype.flatMap.length", "1"),
        ("Array.prototype.flat.name", "flat"),
        ("Array.prototype.flatMap.name", "flatMap"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'flat').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'flat').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'flat').configurable",
            "true",
        ),
        (
            "Object.prototype.hasOwnProperty.call(Array.prototype.flat,'prototype')",
            "false",
        ),
        (
            "(function(){const descriptor=Object.getOwnPropertyDescriptor(Array,Symbol.species);return descriptor.get.name+'|'+descriptor.get.length+'|'+descriptor.enumerable+'|'+descriptor.configurable+'|'+(descriptor.set===undefined);})()",
            "get [Symbol.species]|0|false|true|true",
        ),
        (
            "Object.prototype.hasOwnProperty.call(Object.getOwnPropertyDescriptor(Array,Symbol.species).get,'prototype')",
            "false",
        ),
        (
            "(function(){try{new Array.prototype.flat();}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
    for method in ["flat", "flatMap"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!(
                    "return Array.prototype.{method}.call({receiver},function(value){{return value;}});"
                ),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}
