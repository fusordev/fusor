use std::sync::Arc;

use fusor_bytecode::{
    BytecodeBuilder, BytecodeGraphVerificationLimits, CompilerCaptureLayout,
    CompilerConstantLayout, CompilerExecutableKind, CompilerSource, FinalOpcode,
    FunctionGraphVerificationLimits, FunctionIndexDomains, FunctionTemplateId, Operands,
    PcSourceSpan, SourceByteSpan, UnverifiedCompilerBytecodeGraph, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader,
    UnverifiedFunctionMetadata, VerificationLimits, VerifiedBytecode,
    verify_compiler_bytecode_graph, verify_compiler_control_flow, verify_compiler_function_graph,
};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};
use fusor_runtime::{
    DynamicFunctionScriptError, ExceptionKind, ExecutionError, ExecutionLimits, InstallError,
    JsNumber, Runtime, RuntimeLimits, RuntimeResource, ValueKind,
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
        assert_eq!(
            live.heap_objects(),
            baseline.heap_objects() + 2,
            "escaped object plus the discarded function's prototype await collection"
        );
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
fn sloppy_dynamic_function_direct_call_uses_the_constructor_realm_global_object() {
    let authority = compile_dynamic("return this;");
    let mut runtime = runtime(RuntimeLimits::default());
    let constructor_realm = runtime.create_realm().expect("constructor realm");
    let caller_realm = runtime.create_realm().expect("caller realm");
    let (function, constructor_global) = {
        let mut context = runtime
            .context(&constructor_realm)
            .expect("constructor context");
        let function = context
            .execute_dynamic_function_script(authority, ExecutionLimits::default())
            .expect("dynamic Function Script")
            .into_function()
            .expect("ordinary dynamic function");
        let global = context
            .execute_dynamic_function_script(
                dynamic_push_this_authority(),
                ExecutionLimits::default(),
            )
            .expect("constructor-realm global")
            .into_object()
            .expect("global object");
        (function, global)
    };
    let mut context = runtime.context(&caller_realm).expect("caller context");
    let receiver = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("cross-realm direct sloppy call")
        .into_object()
        .expect("constructor-realm global receiver");
    let caller_global = context
        .execute_dynamic_function_script(dynamic_push_this_authority(), ExecutionLimits::default())
        .expect("caller-realm global")
        .into_object()
        .expect("caller global object");

    assert!(
        receiver
            .same_identity(&constructor_global)
            .expect("same constructor realm")
    );
    assert!(
        !receiver
            .same_identity(&caller_global)
            .expect("distinct realms")
    );
}

#[test]
fn unresolved_dynamic_function_globals_are_property_backed_and_realm_local() {
    let setter_authority = compile_dynamic("realmMarker = 41; return this.realmMarker;");
    let getter_authority = compile_dynamic("return realmMarker;");
    let mut runtime = runtime(RuntimeLimits::default());
    let first_realm = runtime.create_realm().expect("first realm");
    let second_realm = runtime.create_realm().expect("second realm");

    let first_getter = {
        let mut context = runtime.context(&first_realm).expect("first context");
        let setter = context
            .execute_dynamic_function_script(setter_authority, ExecutionLimits::default())
            .expect("setter Script")
            .into_function()
            .expect("setter function");
        let value = context
            .call(&setter, &[], ExecutionLimits::default())
            .expect("sloppy global write");
        assert_eq!(
            value
                .as_number()
                .expect("live result")
                .map(JsNumber::as_f64),
            Some(41.0)
        );

        let getter = context
            .execute_dynamic_function_script(
                Arc::clone(&getter_authority),
                ExecutionLimits::default(),
            )
            .expect("getter Script")
            .into_function()
            .expect("getter function");
        let value = context
            .call(&getter, &[], ExecutionLimits::default())
            .expect("same-realm global read");
        assert_eq!(
            value
                .as_number()
                .expect("live result")
                .map(JsNumber::as_f64),
            Some(41.0)
        );
        getter
    };

    let mut context = runtime.context(&second_realm).expect("second context");
    let value = context
        .call(&first_getter, &[], ExecutionLimits::default())
        .expect("constructor-realm global read from another context");
    assert_eq!(
        value
            .as_number()
            .expect("live result")
            .map(JsNumber::as_f64),
        Some(41.0)
    );
    let getter = context
        .execute_dynamic_function_script(getter_authority, ExecutionLimits::default())
        .expect("second-realm getter Script")
        .into_function()
        .expect("second-realm getter");
    let error = context
        .call(&getter, &[], ExecutionLimits::default())
        .expect_err("another realm has no marker");
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        exception.to_string(),
        "ReferenceError: 'realmMarker' is not defined"
    );
}

#[test]
fn typeof_an_absent_dynamic_function_global_returns_undefined() {
    let authority = compile_dynamic("return typeof realmMissing;");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("ordinary dynamic function");

    let value = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("typeof absent global");
    assert_eq!(
        value
            .as_string()
            .expect("live result")
            .expect("String result")
            .to_utf8_lossy()
            .expect("UTF-8 result"),
        "undefined"
    );
}

#[test]
fn reading_an_absent_dynamic_function_global_throws_exact_reference_error() {
    let authority = compile_dynamic("return realmMissing;");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("ordinary dynamic function");

    let error = context
        .call(&function, &[], ExecutionLimits::default())
        .expect_err("absent global");
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        exception.to_string(),
        "ReferenceError: 'realmMissing' is not defined"
    );
}

#[test]
fn realm_global_binding_limit_rejects_dynamic_installation_atomically() {
    let authority = compile_dynamic("return realmMarker;");
    let mut runtime = runtime(RuntimeLimits::default().with_max_realm_global_bindings(0));
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");

    let error = context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect_err("realm global binding limit");
    assert!(matches!(
        error,
        DynamicFunctionScriptError::Install(InstallError::LimitExceeded {
            resource: RuntimeResource::RealmGlobalBindings,
            limit: 0,
            observed: 1,
        })
    ));
    assert_eq!(context.runtime_usage(), baseline);
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
            DynamicFunctionScriptError::Execution(ExecutionError::LimitExceeded {
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
