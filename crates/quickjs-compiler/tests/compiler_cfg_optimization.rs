use quickjs_bytecode::{FinalOpcode, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree, CompiledLeafFunction};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("verified compilation")
        },
    )
    .expect("frontend acceptance")
}

fn compile_tree(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("whole-graph verified compilation")
        },
    )
    .expect("frontend acceptance")
}

fn is_conditional(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 | FinalOpcode::IfTrue | FinalOpcode::IfTrue8
    )
}

#[test]
fn constant_pool_numbers_strings_and_bigints_fold_with_es_truthiness() {
    let source = r#"
        function choose(value) {
            if (0.5) value = 1; else value = 2;
            if ("truthy") value = 3; else value = 4;
            if (123456789012345678901234567890n) value = 5; else value = 6;
            return value;
        }
    "#;
    let compiled = compile(source, "choose");

    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .all(|verified| !is_conditional(verified.decoded().instruction().opcode()))
    );
    assert_eq!(
        compiled.source_instructions().len(),
        compiled.control_flow().instructions().len()
    );
    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .all(|verified| verified.entry_stack_depth().is_some())
    );
}

#[test]
fn unreachable_direct_eval_metadata_is_removed_and_live_metadata_is_relocated() {
    let source = r#"
        function invoke(object, eval) {
            if (true) object;
            else with (object) eval("dead");
            with (object) return eval("live");
        }
    "#;
    let compiled = compile_tree(source, "invoke");
    let root = compiled.root();
    let evals = root
        .control_flow()
        .instructions()
        .iter()
        .enumerate()
        .filter_map(|(index, verified)| {
            (verified.decoded().instruction().opcode() == FinalOpcode::Eval).then_some(index)
        })
        .collect::<Vec<_>>();

    assert_eq!(evals.len(), 1, "the unreachable direct eval is excised");
    let eval = u32::try_from(evals[0]).expect("eval instruction index");
    assert_eq!(root.eval_reference_call_instructions(), [eval]);
    assert_eq!(
        compiled
            .verified_bytecode()
            .root()
            .function()
            .eval_reference_call_instructions(),
        [eval]
    );
    assert_eq!(
        root.source_instructions().len(),
        root.control_flow().instructions().len()
    );
}

#[test]
fn disconnected_exception_regions_remain_whole_graph_verifiable() {
    let compiled = compile_tree(
        "function f(value){return value;try{value;}catch(error){error;}finally{value;}}",
        "f",
    );

    assert_eq!(
        compiled.root().source_instructions().len(),
        compiled.root().control_flow().instructions().len()
    );
    let opcodes = compiled
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|verified| verified.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::Catch));
    assert!(opcodes.contains(&FinalOpcode::Gosub));
    assert!(opcodes.contains(&FinalOpcode::Ret));
}

#[test]
fn disconnected_lexical_initialization_keeps_binding_authority() {
    let compiled = compile_tree("function f(){return 0;{let value=1;value;}}", "f");
    let root = compiled.root();

    assert!(root.control_flow().instructions().iter().any(|verified| {
        verified.decoded().instruction().opcode() == FinalOpcode::SetLocUninitialized
            && verified.entry_stack_depth().is_none()
    }));
    assert_eq!(
        root.control_flow()
            .instructions()
            .last()
            .expect("synthetic disconnected terminal")
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::ReturnUndef
    );

    let asynchronous = compile_tree(
        "async function asynchronous(){return 0;{let value=1;value;}}",
        "asynchronous",
    );
    let tail = asynchronous.root().control_flow().instructions();
    assert_eq!(
        tail[tail.len() - 2].decoded().instruction().opcode(),
        FinalOpcode::Undefined
    );
    assert_eq!(
        tail[tail.len() - 1].decoded().instruction().opcode(),
        FinalOpcode::ReturnAsync
    );
}

#[test]
fn disconnected_block_function_initializer_keeps_graph_authority() {
    let compiled = compile_tree(
        "function outer(){\"use strict\";return 0;{function hidden(){return 1;}hidden;}}",
        "outer",
    );

    assert!(
        compiled
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|verified| matches!(
                verified.decoded().instruction().opcode(),
                FinalOpcode::FClosure | FinalOpcode::FClosure8
            ) && verified.entry_stack_depth().is_none())
    );
}

#[test]
fn unknown_parameter_truthiness_remains_a_verified_two_edge_branch() {
    let compiled = compile(
        "function choose(value) { if (value) return 1; return 2; }",
        "choose",
    );

    assert!(
        compiled
            .control_flow()
            .instructions()
            .iter()
            .any(|verified| is_conditional(verified.decoded().instruction().opcode()))
    );
}
