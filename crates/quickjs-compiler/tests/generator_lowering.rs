use quickjs_bytecode::{FinalOpcode, FunctionKind, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledLeafFunction};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named generator");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("generator lowering")
        },
    )
    .expect("frontend")
}

fn compile_last(source: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context.executables().last().expect("last executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("generator lowering")
        },
    )
    .expect("frontend")
}

#[test]
fn generator_yield_resume_and_return_use_the_verified_suspension_program() {
    let compiled = compile("function* g(a) { const b = yield a; return b + 1; }", "g");
    let flow = compiled.control_flow();
    assert_eq!(flow.function_header().kind(), FunctionKind::Generator);
    assert!(!flow.function_header().flags().has_prototype());
    assert_eq!(
        flow.instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>(),
        [
            FinalOpcode::SetLocUninitialized,
            FinalOpcode::InitialYield,
            FinalOpcode::GetArg0,
            FinalOpcode::Yield,
            FinalOpcode::IfFalse8,
            FinalOpcode::ReturnAsync,
            FinalOpcode::PutLoc0,
            FinalOpcode::GetLocCheck,
            FinalOpcode::Push1,
            FinalOpcode::Add,
            FinalOpcode::ReturnAsync,
        ]
    );
}

#[test]
fn nested_yield_spreads_retain_the_enclosing_expression_stack_across_suspension() {
    for source in [
        "function* values(){yield [...yield];}",
        "function* values(){yield {...yield};}",
    ] {
        let compiled = compile(source, "values");
        assert!(
            compiled
                .control_flow()
                .instructions()
                .iter()
                .any(|instruction| {
                    instruction.decoded().instruction().opcode() == FinalOpcode::ReturnAsync
                        && instruction
                            .entry_stack_depth()
                            .is_some_and(|depth| depth > 1)
                })
        );
    }
}

#[test]
fn yield_return_closes_iterators_active_in_assignment_destructuring() {
    for source in [
        "let x; function* g(values) { [x = yield] = values; }",
        "let x; function* g(values) { for ({x = yield} of values) {} }",
        "let x = {}; function* g(values) { [...[x[yield]]] = values; }",
    ] {
        let compiled = compile(source, "g");
        let opcodes = compiled
            .control_flow()
            .instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>();

        assert!(
            opcodes.windows(5).any(|window| {
                window
                    == [
                        FinalOpcode::NipCatch,
                        FinalOpcode::Rot3r,
                        FinalOpcode::Undefined,
                        FinalOpcode::IteratorClose,
                        FinalOpcode::ReturnAsync,
                    ]
            }),
            "yield return did not close the active iterator: {opcodes:?}"
        );
    }
}

#[test]
fn disconnected_generator_resume_cleanup_uses_only_local_markers() {
    let compiled = compile(
        "function* g(){try{yield 1;throw 2;try{yield 3;}catch(error){yield error;}}finally{yield 4;}}",
        "g",
    );
    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.entry_stack_depth().is_none()),
        "the nested try remains structurally disconnected after the source throw"
    );
}

#[test]
fn empty_generator_has_an_explicit_undefined_async_return() {
    let compiled = compile("function* empty() {}", "empty");
    assert_eq!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>(),
        [
            FinalOpcode::InitialYield,
            FinalOpcode::Undefined,
            FinalOpcode::ReturnAsync,
        ]
    );
}

#[test]
fn delegated_yield_uses_the_verified_iterator_protocol_loop() {
    let compiled = compile(
        "function* outer(iterable) { return yield* iterable; }",
        "outer",
    );
    let opcodes = compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert_eq!(
        compiled.control_flow().function_header().kind(),
        FunctionKind::Generator
    );
    for required in [
        FinalOpcode::ForOfStart,
        FinalOpcode::IteratorNext,
        FinalOpcode::IteratorCheckObject,
        FinalOpcode::YieldStar,
        FinalOpcode::IteratorCall,
    ] {
        assert!(
            opcodes.contains(&required),
            "delegated yield did not emit {required:?}: {opcodes:?}"
        );
    }
}

#[test]
fn async_generator_awaits_yields_and_explicit_returns() {
    let compiled = compile(
        "async function* values(input) { yield input; return input + 1; }",
        "values",
    );
    let flow = compiled.control_flow();

    assert_eq!(flow.function_header().kind(), FunctionKind::AsyncGenerator);
    assert_eq!(
        flow.instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>(),
        [
            FinalOpcode::InitialYield,
            FinalOpcode::GetArg0,
            FinalOpcode::Await,
            FinalOpcode::Yield,
            FinalOpcode::IfFalse8,
            FinalOpcode::ReturnAsync,
            FinalOpcode::Drop,
            FinalOpcode::GetArg0,
            FinalOpcode::Push1,
            FinalOpcode::Add,
            FinalOpcode::Await,
            FinalOpcode::ReturnAsync,
        ]
    );
}

#[test]
fn async_generator_method_uses_the_async_generator_header() {
    let compiled = compile_last("const holder = { async *values() { yield 1; } };");

    assert_eq!(
        compiled.control_flow().function_header().kind(),
        FunctionKind::AsyncGenerator
    );
    assert!(
        !compiled
            .control_flow()
            .function_header()
            .flags()
            .has_prototype()
    );
}

#[test]
fn async_generator_delegation_uses_the_verified_async_iterator_protocol_loop() {
    let compiled = compile(
        "async function* outer(iterable) { return yield* iterable; }",
        "outer",
    );
    let opcodes = compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert_eq!(
        compiled.control_flow().function_header().kind(),
        FunctionKind::AsyncGenerator
    );
    for required in [
        FinalOpcode::ForAwaitOfStart,
        FinalOpcode::IteratorNext,
        FinalOpcode::Await,
        FinalOpcode::IteratorCheckObject,
        FinalOpcode::AsyncYieldStar,
        FinalOpcode::IteratorCall,
    ] {
        assert!(
            opcodes.contains(&required),
            "delegated async yield did not emit {required:?}: {opcodes:?}"
        );
    }
    let async_yield = opcodes
        .iter()
        .position(|opcode| *opcode == FinalOpcode::AsyncYieldStar)
        .expect("delegated async yield opcode");
    assert_eq!(
        opcodes.get(async_yield.checked_sub(1).expect("preceding value read")),
        Some(&FinalOpcode::GetField),
        "the typed verifier requires async yield* to extract value before suspension"
    );
}
