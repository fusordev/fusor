use std::sync::Arc;

use quickjs_bytecode::{
    BytecodeBuilder, BytecodeGraphVerificationLimits, CompilerCaptureLayout,
    CompilerConstantLayout, CompilerExecutableKind, CompilerSource, FinalOpcode,
    FunctionGraphVerificationLimits, FunctionIndexDomains, FunctionTemplateId, Operands,
    PcSourceSpan, SourceByteSpan, UnverifiedCompilerBytecodeGraph, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader,
    UnverifiedFunctionMetadata, VerificationLimits, VerifiedBytecode,
    verify_compiler_bytecode_graph, verify_compiler_control_flow, verify_compiler_function_graph,
};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};
use quickjs_runtime::{
    DynamicFunctionScriptError, ExecutionLimits, InstallError, JsNumber, Runtime, RuntimeLimits,
    RuntimeResource, ValueKind,
};

fn compile_dynamic(body: &str) -> Arc<VerifiedBytecode> {
    let parameters = [];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(body),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context
            .compile_dynamic_function_script(VerificationLimits::default())
            .map(|tree| Arc::new(tree.verified_bytecode().clone()))
    })
    .expect("dynamic frontend")
    .expect("dynamic compiler")
}

fn compile_ordinary(source: &str, root_name: &str) -> Arc<VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, VerificationLimits::default())
                .expect("ordinary compiler");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("ordinary frontend")
}

fn dynamic_push_this_authority() -> Arc<VerifiedBytecode> {
    let mut builder = BytecodeBuilder::new();
    builder
        .push(FinalOpcode::PushThis, Operands::None)
        .expect("PushThis");
    builder
        .push(FinalOpcode::Return, Operands::None)
        .expect("Return");
    let flow = Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                builder.into_bytes(),
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::dynamic_function_script(0),
            )
            .with_capture_layout(CompilerCaptureLayout::new(Arc::from([])))
            .with_constant_layout(CompilerConstantLayout::new(Arc::from([]))),
            VerificationLimits::default(),
        )
        .expect("verified PushThis Script"),
    );
    let graph = Arc::new(
        verify_compiler_function_graph(
            UnverifiedCompilerFunctionGraph::new(
                FunctionTemplateId::new(0),
                Arc::from([UnverifiedCompilerFunction::new(
                    Arc::clone(&flow),
                    Arc::from([]),
                    Arc::from([]),
                )]),
            ),
            FunctionGraphVerificationLimits::default(),
        )
        .expect("verified PushThis graph"),
    );
    let text: Arc<str> = Arc::from("this");
    let span = SourceByteSpan::new(0, 4);
    let mappings = Arc::from(
        flow.instructions()
            .iter()
            .map(|instruction| PcSourceSpan::new(instruction.decoded().pc(), span))
            .collect::<Vec<_>>(),
    );
    let metadata = UnverifiedFunctionMetadata::new(
        None,
        Arc::from([]),
        Arc::from([]),
        CompilerSource::new(Arc::from("dynamic-this.js"), text, span, None, mappings),
    )
    .with_executable_kind(CompilerExecutableKind::DynamicFunctionScript);
    Arc::new(
        verify_compiler_bytecode_graph(
            UnverifiedCompilerBytecodeGraph::new(graph, Arc::from([metadata])),
            BytecodeGraphVerificationLimits::default(),
        )
        .expect("verified PushThis authority"),
    )
}

fn runtime(limits: RuntimeLimits) -> Runtime {
    Runtime::try_new(limits).expect("runtime")
}

#[test]
fn ordinary_instantiation_rejects_dynamic_function_script_authority_atomically() {
    let authority = compile_dynamic("return 1;");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");

    let error = context
        .instantiate(authority)
        .expect_err("Script authority is not an ordinary callable root");

    assert!(matches!(error, InstallError::AuthorityInvariant { .. }));
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn dynamic_function_script_execution_rejects_ordinary_authority_atomically() {
    let authority = compile_ordinary("function ordinary(){return 1;}", "ordinary");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");

    let error = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect_err("ordinary authority is not a Script root");

    assert!(matches!(
        error,
        DynamicFunctionScriptError::Install(InstallError::AuthorityInvariant { .. })
    ));
    assert_eq!(context.runtime_usage(), baseline);
}

#[test]
fn wrapper_escape_returns_the_exact_script_completion_and_retires_internal_root() {
    let authority = compile_dynamic("}), ({");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let completion = {
        let mut context = runtime.context(&realm).expect("context");
        let completion = context
            .execute_dynamic_function_script(authority, ExecutionLimits::default())
            .expect("Script completion");

        assert_eq!(
            completion.kind().expect("live completion"),
            ValueKind::Object
        );
        let live = context.runtime_usage();
        assert_eq!(live.installed_code(), baseline.installed_code() + 1);
        assert_eq!(
            live.heap_functions(),
            baseline.heap_functions() + 1,
            "only the discarded wrapper child awaits collection"
        );
        assert_eq!(live.heap_objects(), baseline.heap_objects() + 1);
        assert_eq!(live.public_roots(), baseline.public_roots() + 1);
        completion
    };

    let report = runtime
        .collect_cycles()
        .expect("collect discarded wrapper child");
    assert_eq!(report.functions(), 1);
    let rooted = runtime.usage();
    assert_eq!(rooted.installed_code(), baseline.installed_code());
    assert_eq!(rooted.heap_functions(), baseline.heap_functions());
    assert_eq!(rooted.heap_objects(), baseline.heap_objects() + 1);
    assert_eq!(rooted.public_roots(), baseline.public_roots() + 1);

    drop(completion);
    runtime.collect_cycles().expect("collect completion");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn returned_dynamic_function_survives_collection_without_a_temporary_root() {
    let authority = compile_dynamic("return 42;");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let function = {
        let mut context = runtime.context(&realm).expect("context");
        let function = context
            .execute_dynamic_function_script(authority, ExecutionLimits::default())
            .expect("wrapper completion")
            .into_function()
            .expect("wrapper function");
        let live = context.runtime_usage();
        assert_eq!(live.installed_code(), baseline.installed_code() + 1);
        assert_eq!(live.heap_functions(), baseline.heap_functions() + 1);
        assert_eq!(live.public_roots(), baseline.public_roots() + 1);
        function
    };

    let report = runtime.collect_cycles().expect("rooted function survives");
    assert_eq!(report.functions(), 0);

    {
        let mut context = runtime.context(&realm).expect("context");
        let result = context
            .call(&function, &[], ExecutionLimits::default())
            .expect("dynamic function call");
        assert_eq!(
            result
                .as_number()
                .expect("live result")
                .map(JsNumber::as_f64),
            Some(42.0)
        );
    }

    drop(function);
    runtime.collect_cycles().expect("collect dynamic function");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn named_wrapper_local_is_initialized_to_the_current_function_object() {
    let authority = compile_dynamic("return anonymous;");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let function = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("wrapper completion")
        .into_function()
        .expect("wrapper function");
    let self_value = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("self value")
        .into_function()
        .expect("self function");

    assert!(
        function
            .same_identity(&self_value)
            .expect("same runtime function identities")
    );
}

#[test]
fn captured_named_wrapper_self_binding_starts_with_the_current_function_object() {
    let authority = compile_dynamic("function readSelf(){return anonymous;} return readSelf;");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let function = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("wrapper completion")
        .into_function()
        .expect("wrapper function");
    let reader = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("reader value")
        .into_function()
        .expect("reader function");
    let captured_self = context
        .call(&reader, &[], ExecutionLimits::default())
        .expect("captured self value")
        .into_function()
        .expect("captured self function");

    assert!(
        function
            .same_identity(&captured_self)
            .expect("same runtime function identities")
    );
}

#[test]
fn script_receiver_is_the_constructor_realm_global_object() {
    let authority = dynamic_push_this_authority();
    let mut runtime = runtime(RuntimeLimits::default());
    let first_realm = runtime.create_realm().expect("first realm");
    let second_realm = runtime.create_realm().expect("second realm");

    let first = runtime
        .context(&first_realm)
        .expect("first context")
        .execute_dynamic_function_script(Arc::clone(&authority), ExecutionLimits::default())
        .expect("first global")
        .into_object()
        .expect("global object");
    let first_again = runtime
        .context(&first_realm)
        .expect("first context")
        .execute_dynamic_function_script(Arc::clone(&authority), ExecutionLimits::default())
        .expect("same global")
        .into_object()
        .expect("global object");
    let second = runtime
        .context(&second_realm)
        .expect("second context")
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("second global")
        .into_object()
        .expect("global object");

    assert!(
        first
            .same_identity(&first_again)
            .expect("same-realm object identities")
    );
    assert!(
        !first
            .same_identity(&second)
            .expect("same-runtime object identities")
    );
}

#[test]
fn public_root_failure_does_not_retain_the_internal_script_root() {
    let authority = compile_dynamic("return 1;");
    let mut runtime = runtime(RuntimeLimits::default().with_max_public_roots(0));
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = context
            .execute_dynamic_function_script(authority, ExecutionLimits::default())
            .expect_err("wrapper publication exceeds the public-root limit");

        assert!(matches!(
            error,
            DynamicFunctionScriptError::Execution(quickjs_runtime::ExecutionError::LimitExceeded {
                resource: RuntimeResource::PublicRoots,
                limit: 0,
                observed: 1,
            })
        ));
        assert_eq!(
            context.runtime_usage().heap_functions(),
            baseline.heap_functions() + 1,
            "only the unrooted wrapper child awaits cycle collection"
        );
    }

    runtime.collect_cycles().expect("collect unpublished child");
    assert_eq!(runtime.usage(), baseline);
}
