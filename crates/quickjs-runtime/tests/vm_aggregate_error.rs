use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, JsString, JsValue,
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
        with_dynamic_function_source(dynamic_source, FrontendLimits::default(), |unit, _| {
            let context = CompilationContext::new_with_source_name(
                unit,
                Arc::from("<runtime AggregateError>"),
            )
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

fn authority(body: &str) -> Arc<VerifiedBytecode> {
    TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority")
}

fn call(body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(authority(body), ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function");
    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("AggregateError operation");
    string(&value)
}

fn string(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn aggregate_error_gets_iterator_once_retains_next_and_reads_done_before_value() {
    let result = call(
        "\
            let log='';let step=0;\
            let iterator={\
                get next(){\
                    log=log+'g';\
                    return function retainedNext(){\
                        log=log+'n';step=step+1;\
                        iterator.next=function reread(){throw 'next reread';};\
                        if(step===1)return {\
                            get done(){log=log+'d';return false;},\
                            get value(){log=log+'v';return 7;}\
                        };\
                        return {\
                            get done(){log=log+'D';return true;},\
                            get value(){log=log+'V';return 9;}\
                        };\
                    };\
                },\
                return(){log=log+'r';return {};}\
            };\
            let iterable={get [Symbol.iterator](){\
                log=log+'s';return function iteratorMethod(){log=log+'i';return iterator;};\
            }};\
            let error=AggregateError(iterable);\
            let empty={[Symbol.iterator](){return {next(){return {done:true};}};}};\
            let first=AggregateError(empty).errors;\
            let second=AggregateError(empty).errors;\
            return log+'|'+error.errors.length+'|'+error.errors[0]+'|'\
                +(error.name==='AggregateError')+'|'+Error.isError(error)+'|'\
                +(first!==second)+'|'+AggregateError.length;",
    );
    assert_eq!(result, "signdvnD|1|7|true|true|true|2");
}

#[test]
fn aggregate_error_converts_message_and_gets_cause_before_iteration() {
    let result = call(
        "\
            let log='';\
            let message={toString(){log=log+'m';return 'M';}};\
            let options={get cause(){log=log+'c';return 5;}};\
            let errors={get [Symbol.iterator](){\
                log=log+'s';\
                return function iteratorMethod(){\
                    log=log+'i';\
                    return {\
                        get next(){\
                            log=log+'g';\
                            return function next(){\
                                log=log+'n';\
                                return {get done(){log=log+'D';return true;}};\
                            };\
                        }\
                    };\
                };\
            }};\
            let error=AggregateError(errors,message,options);\
            return error.message+'|'+error.cause+'|'+error.errors.length+'|'+log;",
    );
    assert_eq!(result, "M|5|0|mcsignD");
}

#[test]
fn aggregate_error_acquisition_failures_do_not_close() {
    let result = call(
        "\
            let missing;\
            try{AggregateError();}catch(error){missing=error.name+':'+error.message;}\
            let iteratorLog='';let iteratorError;\
            try{AggregateError({get [Symbol.iterator](){iteratorLog=iteratorLog+'s';throw 'ITER';}});}\
            catch(error){iteratorError=error;}\
            let nextLog='';let nextError;\
            let errors={[Symbol.iterator](){\
                nextLog=nextLog+'i';\
                return {\
                    get next(){nextLog=nextLog+'g';throw 'NEXT';},\
                    return(){nextLog=nextLog+'r';return {};}\
                };\
            }};\
            try{AggregateError(errors);}catch(error){nextError=error;}\
            return missing+'|'+iteratorError+':'+iteratorLog+'|'+nextError+':'+nextLog;",
    );
    assert_eq!(
        result,
        "TypeError:cannot read property 'Symbol.iterator' of undefined|ITER:s|NEXT:ig"
    );
}

#[test]
fn aggregate_error_step_failures_close_and_preserve_the_original_completion() {
    let result = call(
        "\
            function run(mode){\
                let log='';\
                let iterator={\
                    next(){\
                        log=log+'n';\
                        if(mode===0)throw 'NEXT';\
                        if(mode===1)return {get done(){log=log+'d';throw 'DONE';}};\
                        return {done:false,get value(){log=log+'v';throw 'VALUE';}};\
                    },\
                    return(){log=log+'r';if(mode===1)throw 'CLOSE';return 1;}\
                };\
                let errors={[Symbol.iterator](){log=log+'i';return iterator;}};\
                try{AggregateError(errors);}\
                catch(error){return error+':'+log;}\
            }\
            return run(0)+'|'+run(1)+'|'+run(2);",
    );
    assert_eq!(result, "NEXT:inr|DONE:indr|VALUE:invr");
}

#[test]
fn aggregate_error_closes_for_noncallable_next_and_nonobject_step_results() {
    let result = call(
        "\
            function observe(errors,log){\
                try{AggregateError(errors);}\
                catch(error){return error.name+':'+error.message+':'+log.value;}\
            }\
            let firstLog={value:''};\
            let first={[Symbol.iterator](){\
                firstLog.value=firstLog.value+'i';\
                return {next:1,return(){firstLog.value=firstLog.value+'r';return {};}};\
            }};\
            let secondLog={value:''};\
            let second={[Symbol.iterator](){\
                secondLog.value=secondLog.value+'i';\
                return {\
                    next(){secondLog.value=secondLog.value+'n';return 1;},\
                    return(){secondLog.value=secondLog.value+'r';return {};}\
                };\
            }};\
            return observe(first,firstLog)+'|'+observe(second,secondLog);",
    );
    assert_eq!(
        result,
        "TypeError:not a function:ir|TypeError:iterator must return an object:inr"
    );
}

#[test]
fn infinite_aggregate_error_iteration_is_stopped_by_uncatchable_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context
        .execute_dynamic_function_script(
            authority(
                "\
                    let errors={[Symbol.iterator](){return {\
                        next(){return {done:false,value:1};}\
                    };}};\
                    try{return AggregateError(errors);}catch(error){return 'caught';}",
            ),
            ExecutionLimits::default(),
        )
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function");

    let error = context
        .call(
            &run,
            &[],
            ExecutionLimits::default().with_instruction_fuel(100),
        )
        .expect_err("infinite AggregateError iterator must exhaust fuel");
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded { limit: 100, .. }
    ));
}
