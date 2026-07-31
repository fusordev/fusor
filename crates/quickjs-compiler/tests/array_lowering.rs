use quickjs_bytecode::{
    BytecodePc, ExecutionRequirement, FinalOpcode, Operands, VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, LeafCompilationError, UnsupportedLeafFeature,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("make"))
                .expect("named function executable");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("dense array lowering and whole-graph verification must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str) -> LeafCompilationError {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("make"))
                .expect("named function executable");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect_err("unsupported array form must fail closed")
        },
    )
    .expect("front-end acceptance")
}

fn instructions(tree: &CompiledFunctionTree) -> Vec<(FinalOpcode, Operands)> {
    tree.root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn source_slice_at<'source>(
    tree: &CompiledFunctionTree,
    source: &'source str,
    pc: BytecodePc,
) -> &'source str {
    let span = tree
        .root()
        .source_instructions()
        .iter()
        .find(|entry| entry.pc() == pc)
        .expect("source entry at final instruction PC")
        .span();
    &source[span.start as usize..span.end as usize]
}

#[test]
fn empty_array_uses_array_from_zero_and_gains_array_authority() {
    let tree = compile("function make(){return [];}");

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 },),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(tree.root().control_flow().computed_stack_size(), 1);
    assert_eq!(
        tree.verified_bytecode().requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Strings,
            ExecutionRequirement::Arrays,
        ]
    );
}

#[test]
fn dense_and_nested_arrays_evaluate_left_to_right_with_exact_u16_counts() {
    let source = "function make(){return [1,[2,3],4];}";
    let tree = compile(source);

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Push3, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 2 },),
            (FinalOpcode::Push4, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 3 },),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(tree.root().control_flow().computed_stack_size(), 3);
    let array_sites = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .filter_map(|instruction| {
            let decoded = instruction.decoded();
            (decoded.instruction().opcode() == FinalOpcode::ArrayFrom).then_some(decoded.pc())
        })
        .map(|pc| source_slice_at(&tree, source, pc))
        .collect::<Vec<_>>();
    assert_eq!(array_sites, ["[2,3]", "[1,[2,3],4]"]);
}

#[test]
fn array_from_retains_counts_wider_than_u8() {
    let elements = (0..257).map(|_| "0").collect::<Vec<_>>().join(",");
    let source = format!("function make(){{return [{elements}];}}");
    let tree = compile(&source);
    let array = instructions(&tree)
        .into_iter()
        .find(|(opcode, _)| *opcode == FinalOpcode::ArrayFrom)
        .expect("one array construction instruction");

    assert_eq!(
        array,
        (
            FinalOpcode::ArrayFrom,
            Operands::NPop {
                argument_count: 257,
            },
        )
    );
    assert_eq!(tree.root().control_flow().computed_stack_size(), 257);
}

#[test]
fn elisions_and_spread_fail_closed_at_the_exact_element_span() {
    for (source, expected) in [
        ("function make(){return [1,,3];}", ","),
        ("function make(items){return [1,...items,3];}", "...items"),
    ] {
        let error = compile_error(source);
        let LeafCompilationError::Unsupported { feature, span } = error else {
            panic!("expected exact-span unsupported error, got {error:?}");
        };
        assert_eq!(feature, UnsupportedLeafFeature::UnsupportedExpression);
        assert_eq!(&source[span.start as usize..span.end as usize], expected);
    }
}

#[test]
fn array_element_count_beyond_u16_fails_before_encoding() {
    let elements = (0..=u16::MAX).map(|_| "0").collect::<Vec<_>>().join(",");
    let source = format!("function make(){{return [{elements}];}}");

    assert_eq!(
        compile_error(&source),
        LeafCompilationError::CapacityExceeded {
            domain: "array literal elements",
        }
    );
}
