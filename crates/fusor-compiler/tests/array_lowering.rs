use fusor_bytecode::{
    BytecodePc, ExecutionRequirement, FinalOpcode, Operands, VerificationLimits,
};
use fusor_compiler::{CompilationContext, CompiledFunctionTree, LeafCompilationError};
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

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
                .expect("array lowering and whole-graph verification must succeed")
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

fn atom_text(tree: &CompiledFunctionTree, index: u32) -> String {
    tree.root().atoms()[index as usize]
        .string()
        .code_units()
        .map(|unit| char::from_u32(u32::from(unit)).expect("ASCII test atom"))
        .collect()
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
fn sparse_array_uses_static_indices_and_sets_length_only_for_a_trailing_elision() {
    let source = "function make(){return [1,,3,,];}";
    let tree = compile(source);

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 },),
            (FinalOpcode::Push3, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::Push4, Operands::NoneInt),
            (
                FinalOpcode::PutField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(atom_text(&tree, 0), "2");
    assert_eq!(atom_text(&tree, 1), "length");
    assert!(tree.root().atoms()[0].is_static_property_only());
    assert!(!tree.root().atoms()[1].is_static_property_only());

    let sites = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let decoded = instruction.decoded();
            (
                decoded.instruction().opcode(),
                source_slice_at(&tree, source, decoded.pc()),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(sites[1], (FinalOpcode::ArrayFrom, "[1,,3,,]"));
    assert_eq!(sites[3], (FinalOpcode::DefineField, "3"));
    assert_eq!(sites[4], (FinalOpcode::Dup, ","));
    assert_eq!(sites[5], (FinalOpcode::Push4, ","));
    assert_eq!(sites[6], (FinalOpcode::PutField, ","));
}

#[test]
fn holes_only_allocate_no_element_properties_and_preserve_the_final_length() {
    let source = "function make(){return [,,,];}";
    let tree = compile(source);

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 },),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::Push3, Operands::NoneInt),
            (
                FinalOpcode::PutField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(atom_text(&tree, 0), "length");
    assert!(
        instructions(&tree)
            .iter()
            .all(|(opcode, _)| *opcode != FinalOpcode::Undefined)
    );
}

#[test]
fn sparse_array_property_sites_share_content_interned_atoms_without_sharing_lookup_identity() {
    let tree = compile("function make(){return [[,1],[,2],{1:3},[,,]];}");
    let definitions = instructions(&tree)
        .into_iter()
        .filter_map(|(opcode, operands)| (opcode == FinalOpcode::DefineField).then_some(operands))
        .collect::<Vec<_>>();

    assert_eq!(definitions.len(), 3);
    assert!(
        definitions.iter().all(|operands| {
            *operands == Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0))
        })
    );
    assert_eq!(atom_text(&tree, 0), "1");
    assert_eq!(atom_text(&tree, 1), "length");
    assert_eq!(atom_text(&tree, 2), "make");
    assert_eq!(tree.root().atoms().len(), 3);
}

#[test]
fn sparse_array_expressions_remain_left_to_right_around_initial_allocation() {
    let tree = compile("function make(first,second){return [first(),,second()];}");
    let opcodes = instructions(&tree)
        .into_iter()
        .map(|(opcode, _)| opcode)
        .collect::<Vec<_>>();
    let calls = opcodes
        .iter()
        .enumerate()
        .filter_map(|(index, opcode)| matches!(opcode, FinalOpcode::Call0).then_some(index))
        .collect::<Vec<_>>();
    let array = opcodes
        .iter()
        .position(|opcode| *opcode == FinalOpcode::ArrayFrom)
        .expect("sparse array allocation");
    let definition = opcodes
        .iter()
        .position(|opcode| *opcode == FinalOpcode::DefineField)
        .expect("post-hole element definition");

    assert_eq!(calls.len(), 2);
    assert!(calls[0] < array);
    assert!(array < calls[1]);
    assert!(calls[1] < definition);
}

#[test]
fn spread_array_uses_array_from_checked_cursor_and_append_exactly() {
    let source = "function make(items){return [1,...items,3];}";
    let tree = compile(source);

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Push3, Operands::NoneInt),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(tree.root().control_flow().computed_stack_size(), 3);
    let append_pc = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .find_map(|instruction| {
            let decoded = instruction.decoded();
            (decoded.instruction().opcode() == FinalOpcode::Append).then_some(decoded.pc())
        })
        .expect("one append instruction");
    assert_eq!(source_slice_at(&tree, source, append_pc), "...items");
}

#[test]
fn holes_before_and_after_spread_use_static_then_dynamic_indices_and_final_length() {
    let tree = compile("function make(items){return [1,,2,...items,,4,,];}");

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
            (FinalOpcode::Push2, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Push3, Operands::NoneInt),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::Push4, Operands::NoneInt),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::Dup1, Operands::None),
            (
                FinalOpcode::PutField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(atom_text(&tree, 0), "2");
    assert_eq!(atom_text(&tree, 1), "length");
}

#[test]
fn hole_immediately_before_final_spread_preserves_the_dynamic_length_update() {
    let tree = compile("function make(items){return [1,,...items];}");

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Dup1, Operands::None),
            (
                FinalOpcode::PutField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(atom_text(&tree, 0), "length");
}

#[test]
fn multiple_spreads_each_emit_append_and_keep_one_dynamic_cursor() {
    let tree = compile("function make(first,second){return [...first,2,...second];}");

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 }),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn nested_spread_expressions_preserve_observable_left_to_right_evaluation() {
    let tree = compile(
        "function make(prefix,outer,inner,suffix){return [prefix(),...outer(inner()),suffix()];}",
    );

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 1 }),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::GetArg2, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::Call1, Operands::NPopX),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::GetArg3, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Inc, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn spread_after_quickjs_stack_prefix_uses_static_index_and_checked_dynamic_cursor() {
    let prefix = (0..33).map(|_| "0").collect::<Vec<_>>().join(",");
    let source = format!("function make(items){{return [{prefix},...items];}}");
    let tree = compile(&source);
    let instructions = instructions(&tree);
    let array_from = instructions
        .iter()
        .position(|(opcode, _)| *opcode == FinalOpcode::ArrayFrom)
        .expect("one stack-prefix allocation");

    assert_eq!(array_from, 32);
    assert_eq!(
        instructions[array_from],
        (
            FinalOpcode::ArrayFrom,
            Operands::NPop { argument_count: 32 }
        )
    );
    assert_eq!(
        &instructions[array_from + 1..],
        [
            (FinalOpcode::Push0, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::PushI8, Operands::I8(33)),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Append, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(atom_text(&tree, 0), "32");
}

#[test]
fn dense_array_literals_compile_without_host_stack_growth() {
    let elements = (0..128)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("function make(){{return [{elements}];}}");

    let tree = compile(&source);
    assert_eq!(tree.root().control_flow().computed_stack_size(), 128);
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

#[test]
fn sparse_total_length_is_not_limited_by_the_dense_array_from_operand() {
    let holes = ",".repeat(usize::from(u16::MAX) + 1);
    let source = format!("function make(){{return [{holes}];}}");
    let tree = compile(&source);

    assert_eq!(
        instructions(&tree),
        [
            (FinalOpcode::ArrayFrom, Operands::NPop { argument_count: 0 },),
            (FinalOpcode::Dup, Operands::None),
            (FinalOpcode::PushI32, Operands::I32(65_536)),
            (
                FinalOpcode::PutField,
                Operands::Atom(fusor_bytecode::AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn sparse_dense_prefix_beyond_u16_has_its_own_capacity_domain() {
    let elements = (0..=u16::MAX).map(|_| "0").collect::<Vec<_>>().join(",");
    let source = format!("function make(){{return [{elements},,];}}");

    assert_eq!(
        compile_error(&source),
        LeafCompilationError::CapacityExceeded {
            domain: "array literal dense prefix",
        }
    );
}
