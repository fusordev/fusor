use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsNumber, JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
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
        let body = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body),
        );
        with_dynamic_function_source(dynamic_source, FrontendLimits::default(), |unit, _| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("<runtime for-of>"))
                    .map_err(engine_failure)?;
            context
                .compile_dynamic_function_script(VerificationLimits::default())
                .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                .map_err(engine_failure)
        })
        .map_err(engine_failure)?
    }
}

fn engine_failure(error: impl Error + Send + Sync + 'static) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(TestCompileError(error.to_string())),
    }
}

fn compile(source: &str, root_name: &str) -> Arc<VerifiedBytecode> {
    let body = format!("{source};return {root_name}();");
    TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(&body).expect("body"),
        ))
        .expect("dynamic Function authority")
}

fn dynamic_function(context: &mut Context<'_>, authority: Arc<VerifiedBytecode>) -> Function {
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn call_string(source: &str, root_name: &str) -> String {
    let authority = compile(source, root_name);
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(&mut context, authority);
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .unwrap_or_else(|error| match &error {
            ExecutionError::Exception(exception) => panic!(
                "for-of execution: {:?}: {}",
                exception.kind(),
                exception
                    .message()
                    .and_then(|message| message.to_utf8_lossy().ok())
                    .unwrap_or_default()
            ),
            _ => panic!("for-of execution: {error:?}"),
        });
    result
        .as_string()
        .expect("live value")
        .expect("string")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn arrays_strings_targets_and_captured_lexicals_use_the_generic_for_of_protocol() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(const value of [1,2]) output=output+value;\
            let target={key:0};\
            for(target.key of [3]) output=output+target.key;\
            let property=\"key\";\
            for(target[property] of [4]) output=output+target[property];\
            let first;let second;let index=0;\
            for(let value of [5,6]){\
                if(index===0) first=function firstCapture(){return value;};\
                else second=function secondCapture(){return value;};\
                index++;\
            }\
            let text=\"\";\
            for(const character of \"A😀\") text=text+character;\
            return output+\"|\"+first()+second()+\"|\"+text;\
        }",
        "run",
    );

    assert_eq!(result, "1234|56|A😀");
}

#[test]
fn for_of_array_pattern_declaration_heads_destructure_each_value() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(let [a, b] of [[1, 2], [3, 4]]) output=output+a+b;\
            return output;\
        }",
        "run",
    );
    assert_eq!(result, "1234");
}

#[test]
fn for_of_array_rest_and_nested_pattern_heads() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(let [a, ...rest] of [[1, 2, 3]]) output=output+a+rest[0]+rest[1];\
            for(let [a, [b, c]] of [[4, [5, 6]]]) output=output+a+b+c;\
            return output;\
        }",
        "run",
    );
    assert_eq!(result, "123456");
}

#[test]
fn for_of_object_pattern_heads_with_rest_and_defaults() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(let {x, y} of [{x:1, y:2}, {x:3, y:4}]) output=output+x+y;\
            for(let {x, ...rest} of [{x:1, y:2, z:3}]) output=output+x+rest.y+rest.z;\
            for(let {x = 9} of [{}, {x: 5}]) output=output+x;\
            return output;\
        }",
        "run",
    );
    assert_eq!(result, "123412395");
}

#[test]
fn for_of_assignment_pattern_heads_destructure_without_declaring() {
    let result = call_string(
        "function run(){\
            let output=\"\";let a=0;let b=0;\
            for([a, b] of [[7, 8]]) output=output+a+b;\
            let o={x:0,y:0};\
            for({x: o.x, y: o.y} of [{x: 9, y: 1}]) output=output+o.x+o.y;\
            return output;\
        }",
        "run",
    );
    assert_eq!(result, "7891");
}

#[test]
fn for_of_const_destructuring_heads_reinitialize_each_iteration() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(const [a, b] of [[1, 2], [3, 4]]) output=output+a+b;\
            for(const {x} of [{x:5}, {x:6}]) output=output+x;\
            for(const {x, ...rest} of [{x:7, y:8}]) output=output+x+rest.y;\
            return output;\
        }",
        "run",
    );
    assert_eq!(result, "12345678");
}

#[test]
fn for_of_destructuring_heads_rotate_captured_lexicals() {
    let result = call_string(
        "function run(){\
            let first;let second;let index=0;\
            for(let [value] of [[1], [2]]){\
                if(index===0) first=function firstCapture(){return value;};\
                else second=function secondCapture(){return value;};\
                index++;\
            }\
            let obj;let i=0;\
            for(let {x} of [{x:3}, {x:4}]){\
                if(i===0) obj=function objCapture(){return x;};\
                i++;\
            }\
            return \"\"+first()+second()+\"|\"+obj();\
        }",
        "run",
    );
    assert_eq!(result, "12|3");
}

#[test]
fn for_of_destructuring_heads_honor_break_and_continue() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(let [a] of [[1], [2], [3]]){\
                if(a===2) continue;\
                if(a===3) break;\
                output=output+a;\
            }\
            for(let {x} of [{x:4}, {x:5}]){\
                if(x===5) break;\
                output=output+x;\
            }\
            return output;\
        }",
        "run",
    );
    assert_eq!(result, "14");
}

#[test]
fn for_of_var_pattern_heads_share_the_function_scope() {
    let result = call_string(
        "function run(){\
            let output=\"\";\
            for(var [a] of [[5]]) output=output+a;\
            return output+a;\
        }",
        "run",
    );
    assert_eq!(result, "55");
}

#[test]
fn for_of_reads_iterator_and_next_once_then_done_before_value() {
    let result = call_string(
        "function run(){\
            let log=\"\";let step=0;\
            let iterator={\
                get next(){\
                    log=log+\"n\";\
                    return function retainedNext(){\
                        log=log+\"c\";\
                        return {\
                            get done(){log=log+\"d\";return step++>0;},\
                            get value(){log=log+\"v\";return 7;}\
                        };\
                    };\
                },\
                return(){log=log+\"r\";return {};}\
            };\
            let iterable={\
                get [Symbol.iterator](){\
                    log=log+\"i\";\
                    return function iteratorMethod(){log=log+\"m\";return iterator;};\
                }\
            };\
            for(const value of iterable) log=log+value;\
            return log;\
        }",
        "run",
    );

    assert_eq!(result, "imncdv7cd");
}

#[test]
fn continue_break_return_and_labeled_transfer_close_exact_iterators() {
    let result = call_string(
        "function run(){\
            let log=\"\";\
            function iterable(name,count){\
                return {\
                    [Symbol.iterator](){\
                        let index=0;\
                        return {\
                            next(){\
                                if(index<count) return {value:index++,done:false};\
                                return {value:void 0,done:true};\
                            },\
                            return(){log=log+name;return {};}\
                        };\
                    }\
                };\
            }\
            for(const value of iterable(\"B\",2)){if(value===0)continue;break;}\
            function leave(){for(const value of iterable(\"R\",1))return 9;return 0;}\
            let returned=leave();\
            outer:for(const left of iterable(\"O\",1)){\
                for(const right of iterable(\"I\",1))break outer;\
            }\
            outerContinue:for(const left of iterable(\"U\",1)){\
                for(const right of iterable(\"V\",1))continue outerContinue;\
            }\
            for(const value of iterable(\"C\",1)){try{break;}finally{log=log+\"F\";}}\
            return log+\"|\"+returned;\
        }",
        "run",
    );

    assert_eq!(result, "BRIOVFC|9");
}

#[test]
fn pending_close_preserves_body_and_assignment_errors_but_step_errors_do_not_close() {
    let result = call_string(
        "function run(){\
            let log=\"\";let observed=\"\";\
            let bodyIterator={\
                next(){return {value:1,done:false};},\
                get return(){log=log+\"r\";throw \"close\";}\
            };\
            let bodyIterable={[Symbol.iterator](){return bodyIterator;}};\
            try{for(const value of bodyIterable)throw \"body\";}catch(error){observed=observed+error;}\
            let target={set value(next){throw \"set\";}};\
            let assignmentIterator={\
                next(){return {value:2,done:false};},\
                return(){log=log+\"s\";return {};}\
            };\
            try{for(target.value of {[Symbol.iterator](){return assignmentIterator;}}){}}\
            catch(error){observed=observed+\"|\"+error;}\
            let nextIterator={next(){throw \"next\";},return(){log=log+\"x\";return {};}};\
            try{for(const value of {[Symbol.iterator](){return nextIterator;}}){}}\
            catch(error){observed=observed+\"|\"+error;}\
            let doneIterator={\
                next(){return {get done(){throw \"done\";}};},\
                return(){log=log+\"d\";return {};}\
            };\
            try{for(const value of {[Symbol.iterator](){return doneIterator;}}){}}\
            catch(error){observed=observed+\"|\"+error;}\
            let valueIterator={\
                next(){return {done:false,get value(){throw \"value\";}};},\
                return(){log=log+\"v\";return {};}\
            };\
            try{for(const value of {[Symbol.iterator](){return valueIterator;}}){}}\
            catch(error){observed=observed+\"|\"+error;}\
            return observed+\"|\"+log;\
        }",
        "run",
    );

    assert_eq!(result, "body|set|next|done|value|rs");
}

#[test]
fn nullish_for_of_names_symbol_iterator_in_the_exact_type_error() {
    for (expression, expected) in [
        ("null", "cannot read property 'Symbol.iterator' of null"),
        (
            "void 0",
            "cannot read property 'Symbol.iterator' of undefined",
        ),
    ] {
        let source = format!("function run(){{for(const value of {expression}){{}}}}");
        let authority = compile(&source, "run");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let function = dynamic_function(&mut context, authority);
        let error = context
            .call(&function, &[], ExecutionLimits::default())
            .expect_err("nullish value must not be iterable");

        assert!(matches!(
            error,
            ExecutionError::Exception(ref exception)
                if exception.kind() == Some(ExceptionKind::TypeError)
                    && exception.message().is_some_and(|message| {
                        message.to_utf8_lossy().is_ok_and(|message| message == expected)
                    })
        ));
    }
}

#[test]
fn primitive_normal_close_result_throws_type_error() {
    let authority = compile(
        "function run(){\
            let iterable={\
                [Symbol.iterator](){\
                    return {next(){return {value:1,done:false};},return(){return 1;}};\
                }\
            };\
            for(const value of iterable)break;\
            return 0;\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(&mut context, authority);
    let error = context
        .call(&function, &[], ExecutionLimits::default())
        .expect_err("primitive iterator return result");

    assert!(matches!(
        error,
        ExecutionError::Exception(ref exception)
            if exception.kind() == Some(ExceptionKind::TypeError)
                && exception.message().is_some_and(|message| {
                    message.to_utf8_lossy().is_ok_and(|message| message == "not an object")
                })
    ));
}

#[test]
fn infinite_for_of_is_bounded_by_shared_instruction_fuel() {
    let authority = compile(
        "function run(){\
            let iterable={[Symbol.iterator](){return {next(){return {value:1,done:false};}};}};\
            for(const value of iterable){}\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(&mut context, authority);

    assert!(matches!(
        context
            .call(
                &function,
                &[],
                ExecutionLimits::default().with_instruction_fuel(256),
            )
            .expect_err("infinite iterator must stop"),
        ExecutionError::InstructionLimitExceeded { .. }
    ));

    let one = JsNumber::from_i32(1);
    let recovery = compile("function recovery(){return 1;}", "recovery");
    let recovery = dynamic_function(&mut context, recovery);
    let value = context
        .call(&recovery, &[], ExecutionLimits::default())
        .expect("runtime remains reusable");
    assert!(
        value
            .as_number()
            .expect("live value")
            .is_some_and(|value| value.strict_equals(one))
    );
}
