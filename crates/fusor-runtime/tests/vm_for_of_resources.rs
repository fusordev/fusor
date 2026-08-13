use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{
    CompilerExecutableKind, FunctionTemplateId, VerificationLimits, VerifiedBytecode,
};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsNumber,
    JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime,
    RuntimeLimits, RuntimeResource,
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
        let parameter_text = source
            .parameters()
            .iter()
            .map(JsString::to_utf8_lossy)
            .collect::<Result<Vec<_>, _>>()
            .map_err(engine_failure)?;
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let parameters = parameter_text
            .iter()
            .map(|parameter| SourceFragment::new(parameter.as_str()))
            .collect::<Vec<_>>();
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &parameters,
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(dynamic_source, FrontendLimits::default(), |unit, _| {
            let context = CompilationContext::new_with_source_name(
                unit,
                Arc::from("<runtime for-of resources>"),
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

fn authority(parameters: &[&str], body: &str) -> Arc<VerifiedBytecode> {
    let parameters = parameters
        .iter()
        .map(|parameter| JsString::from_utf8(parameter).expect("parameter"))
        .collect::<Vec<_>>();
    TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from(parameters),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority")
}

fn ordinary_function_template(authority: &VerifiedBytecode) -> FunctionTemplateId {
    let index = authority
        .functions()
        .position(|function| {
            function.metadata().executable_kind() == CompilerExecutableKind::OrdinaryFunction
        })
        .expect("ordinary dynamic function");
    FunctionTemplateId::new(u32::try_from(index).expect("small template index"))
}

fn template_with_source(authority: &VerifiedBytecode, source: &str) -> FunctionTemplateId {
    let mut matches = authority
        .functions()
        .enumerate()
        .filter(|(_, function)| function.metadata().source().function_source() == source);
    let (index, _) = matches.next().expect("target callback template");
    assert!(matches.next().is_none(), "target callback source is unique");
    FunctionTemplateId::new(u32::try_from(index).expect("small template index"))
}

fn reserved_frame_values(authority: &VerifiedBytecode, function: FunctionTemplateId) -> u64 {
    let control_flow = authority
        .function(function)
        .expect("function")
        .function()
        .control_flow();
    let domains = control_flow.domains();
    u64::from(domains.argument_count())
        + u64::from(domains.local_count())
        + u64::from(control_flow.computed_stack_size())
        + 1
}

fn dynamic_function(context: &mut Context<'_>, authority: Arc<VerifiedBytecode>) -> Function {
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn assert_number(value: &fusor_runtime::JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

struct AdmissionCase {
    name: &'static str,
    body: &'static str,
    callback_source: &'static str,
    retained_values: u64,
}

const ADMISSION_CASES: &[AdmissionCase] = &[
    AdmissionCase {
        name: "ForOfStart Symbol.iterator getter",
        body: "let iterator={next:Function.prototype.valueOf,done:true,get [Symbol.iterator](){state.hits=state.hits+1;return Function.prototype.valueOf;}};for(let value of iterator){}return state.hits;",
        callback_source: "get [Symbol.iterator](){state.hits=state.hits+1;return Function.prototype.valueOf;}",
        retained_values: 1,
    },
    AdmissionCase {
        name: "ForOfStart next getter",
        body: "let iterator={[Symbol.iterator]:Function.prototype.valueOf,done:true,get next(){state.hits=state.hits+1;return Function.prototype.valueOf;}};for(let value of iterator){}return state.hits;",
        callback_source: "get next(){state.hits=state.hits+1;return Function.prototype.valueOf;}",
        retained_values: 2,
    },
    AdmissionCase {
        name: "ForOfNext result call",
        body: "let iterator={[Symbol.iterator]:Function.prototype.valueOf,done:true,next(){state.hits=state.hits+1;return iterator;}};for(let value of iterator){}return state.hits;",
        callback_source: "next(){state.hits=state.hits+1;return iterator;}",
        retained_values: 2,
    },
    AdmissionCase {
        name: "ForOfNext done getter",
        body: "let iterator={[Symbol.iterator]:Function.prototype.valueOf,next:Function.prototype.valueOf,get done(){state.hits=state.hits+1;return true;}};for(let value of iterator){}return state.hits;",
        callback_source: "get done(){state.hits=state.hits+1;return true;}",
        retained_values: 3,
    },
    AdmissionCase {
        name: "ForOfNext value getter",
        body: "let iterator={[Symbol.iterator]:Function.prototype.valueOf,next:Function.prototype.valueOf,done:false,get value(){state.hits=state.hits+1;return 7;}};for(let value of iterator){break;}return state.hits;",
        callback_source: "get value(){state.hits=state.hits+1;return 7;}",
        retained_values: 3,
    },
    AdmissionCase {
        name: "ForOfClose return getter",
        body: "let iterator={[Symbol.iterator]:Function.prototype.valueOf,next:Function.prototype.valueOf,done:false,value:7,get return(){state.hits=state.hits+1;return Function.prototype.valueOf;}};for(let value of iterator){break;}return state.hits;",
        callback_source: "get return(){state.hits=state.hits+1;return Function.prototype.valueOf;}",
        retained_values: 1,
    },
];

struct CompiledCase {
    name: &'static str,
    run: Arc<VerifiedBytecode>,
    setup: Arc<VerifiedBytecode>,
    read: Arc<VerifiedBytecode>,
    observed_frame_values: u64,
}

impl CompiledCase {
    fn new(case: &AdmissionCase) -> Self {
        let run = authority(&["state"], case.body);
        let setup = authority(&[], "return {hits:0};");
        let read = authority(&["state"], "return state.hits;");
        let run_values = reserved_frame_values(&run, ordinary_function_template(&run));
        let callback_values =
            reserved_frame_values(&run, template_with_source(&run, case.callback_source));
        let observed_frame_values = run_values
            .saturating_add(case.retained_values)
            .saturating_add(callback_values);
        assert!(
            observed_frame_values > run_values.saturating_add(case.retained_values),
            "{} callback owns a nonempty frame reservation",
            case.name
        );
        Self {
            name: case.name,
            run,
            setup,
            read,
            observed_frame_values,
        }
    }

    fn assert_support_authorities_fit(&self, limit: u64) {
        for authority in [&self.run, &self.setup, &self.read] {
            assert!(
                reserved_frame_values(authority, authority.root_id()) <= limit,
                "{} dynamic script wrapper fits the tested value limit",
                self.name
            );
        }
        for authority in [&self.setup, &self.read] {
            assert!(
                reserved_frame_values(authority, ordinary_function_template(authority)) <= limit,
                "{} support function fits the tested value limit",
                self.name
            );
        }
    }
}

fn rejected_call_is_atomic(case: &CompiledCase, limits: RuntimeLimits) -> ExecutionError {
    let mut runtime = Runtime::try_new(limits).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, Arc::clone(&case.run));
    let setup = dynamic_function(&mut context, Arc::clone(&case.setup));
    let read = dynamic_function(&mut context, Arc::clone(&case.read));
    let state = context
        .call(&setup, &[], ExecutionLimits::default())
        .expect("state setup");

    let error = context
        .call(
            &run,
            std::slice::from_ref(&state),
            ExecutionLimits::default(),
        )
        .expect_err("callback frame admission must fail");
    let hits = context
        .call(&read, &[state], ExecutionLimits::default())
        .expect("runtime remains reusable after rejected iterator callback admission");
    assert_number(&hits, 0);
    error
}

fn exact_limit_succeeds(case: &CompiledCase, limits: RuntimeLimits) {
    let mut runtime = Runtime::try_new(limits).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, Arc::clone(&case.run));
    let setup = dynamic_function(&mut context, Arc::clone(&case.setup));
    let state = context
        .call(&setup, &[], ExecutionLimits::default())
        .expect("state setup");
    let hits = context
        .call(&run, &[state], ExecutionLimits::default())
        .unwrap_or_else(|error| panic!("{} exact admission boundary: {error:?}", case.name));
    assert_number(&hits, 1);
}

#[test]
fn for_of_callback_frames_use_exact_admission_and_rejection_is_atomic() {
    for source in ADMISSION_CASES {
        let case = CompiledCase::new(source);
        let error =
            rejected_call_is_atomic(&case, RuntimeLimits::default().with_max_active_frames(2));
        assert!(
            matches!(
                error,
                ExecutionError::LimitExceeded {
                    resource: RuntimeResource::Frames,
                    limit: 2,
                    observed: 3,
                }
            ),
            "{} must count the caller, suspended continuation, and callback: {error:?}",
            case.name,
        );
        exact_limit_succeeds(&case, RuntimeLimits::default().with_max_active_frames(3));
    }
}

#[test]
fn for_of_callback_values_use_exact_admission_and_rejection_is_atomic() {
    for source in ADMISSION_CASES {
        let case = CompiledCase::new(source);
        let limit = case.observed_frame_values - 1;
        case.assert_support_authorities_fit(limit);
        let error = rejected_call_is_atomic(
            &case,
            RuntimeLimits::default().with_max_active_frame_values(limit),
        );
        assert!(
            matches!(
                error,
                ExecutionError::LimitExceeded {
                    resource: RuntimeResource::FrameValues,
                    limit: actual_limit,
                    observed,
                } if actual_limit == limit && observed == case.observed_frame_values
            ),
            "{} must count its exact retained values before the callback frame: {error:?}",
            case.name,
        );
        exact_limit_succeeds(
            &case,
            RuntimeLimits::default().with_max_active_frame_values(case.observed_frame_values),
        );
    }
}
