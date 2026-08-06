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
