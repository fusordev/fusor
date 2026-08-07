use quickjs_bytecode::{
    CompilerExecutableKind, FinalOpcode, FunctionKind, FunctionTemplateId, VerificationLimits,
};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree, CompiledLeafFunction};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_leaf(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named async function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("async function lowering")
        },
    )
    .expect("frontend")
}

fn compile_tree(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named root function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("async method tree lowering")
        },
    )
    .expect("frontend")
}

fn opcodes(compiled: &CompiledLeafFunction) -> Vec<FinalOpcode> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect()
}

#[test]
fn async_function_await_and_return_use_the_verified_suspension_program() {
    let compiled = compile_leaf(
        "async function f(value) { const resumed = await value; return resumed; }",
        "f",
    );
    let flow = compiled.control_flow();

    assert_eq!(flow.function_header().kind(), FunctionKind::Async);
    assert!(!flow.function_header().flags().has_prototype());
    assert_eq!(flow.function_header().flags().bits(), 0x0662);
    assert_eq!(
        opcodes(&compiled),
        [
            FinalOpcode::SetLocUninitialized,
            FinalOpcode::GetArg0,
            FinalOpcode::Await,
            FinalOpcode::PutLoc0,
            FinalOpcode::GetLocCheck,
            FinalOpcode::ReturnAsync,
        ]
    );
}

#[test]
fn empty_async_function_has_an_explicit_undefined_async_return() {
    let compiled = compile_leaf("async function empty() {}", "empty");

    assert_eq!(
        opcodes(&compiled),
        [FinalOpcode::Undefined, FinalOpcode::ReturnAsync]
    );
}

#[test]
fn async_object_method_uses_the_nonconstructable_method_profile() {
    let tree = compile_tree(
        "function make() { return { async \"method\"(value) { return await value; } }; }",
        "make",
    );
    let method = tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(1))
        .expect("verified async method");

    assert_eq!(
        method.metadata().executable_kind(),
        CompilerExecutableKind::AsyncMethod
    );
    assert_eq!(
        method.function().control_flow().function_header().kind(),
        FunctionKind::Async
    );
    assert_eq!(
        method
            .function()
            .control_flow()
            .function_header()
            .flags()
            .bits(),
        0x0762
    );
}
