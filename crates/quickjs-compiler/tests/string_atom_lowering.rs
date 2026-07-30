use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, Binary64Constant, CompilerConstantValue, CompilerString, FinalOpcode, Operands,
    VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledConstant, CompiledFunction, CompiledFunctionTree,
    CompiledLeafFunction,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_leaf(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("string lowering must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_tree(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("string tree lowering must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn instructions(function: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
    function
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn string_value(constant: &CompiledConstant) -> &CompilerString {
    let CompiledConstant::Value(CompilerConstantValue::String(value)) = constant else {
        panic!("expected an exact string value constant");
    };
    value
}

#[test]
fn ordinary_and_no_substitution_strings_share_deduplicated_atoms() {
    let compiled = compile_leaf("function f(){ return (\"hello\", `hello`, \"\"); }", "f");

    assert!(compiled.constants().is_empty());
    assert_eq!(compiled.atoms().len(), 2);
    assert_eq!(
        compiled.atoms()[0]
            .string()
            .code_units()
            .collect::<Vec<_>>(),
        "hello".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        compiled.atoms()[1]
            .string()
            .code_units()
            .collect::<Vec<_>>(),
        ['f' as u16]
    );
    assert_eq!(
        instructions(&compiled),
        [
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Drop, Operands::None),
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::PushEmptyString, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn canonical_decimal_string_boundary_matches_quickjs_pool_routing() {
    let compiled = compile_leaf(
        "function f(){ return (\"0\", `0`, \"2147483647\", \"2147483648\", \"00\"); }",
        "f",
    );

    assert_eq!(compiled.constants().len(), 3);
    assert_eq!(
        compiled
            .constants()
            .iter()
            .map(string_value)
            .map(|value| value.code_units().collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [
            vec!['0' as u16],
            vec!['0' as u16],
            "2147483647".encode_utf16().collect::<Vec<_>>(),
        ]
    );
    assert_eq!(compiled.atoms().len(), 3);
    assert_eq!(
        compiled
            .atoms()
            .iter()
            .map(|atom| atom.string().code_units().collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [
            "2147483648".encode_utf16().collect::<Vec<_>>(),
            "00".encode_utf16().collect::<Vec<_>>(),
            vec!['f' as u16],
        ]
    );
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::PushConst8, Operands::Const8(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::PushConst8, Operands::Const8(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::PushConst8, Operands::Const8(2)),
            (FinalOpcode::Drop, Operands::None),
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Drop, Operands::None),
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn atoms_preserve_latin1_wide_astral_and_lone_surrogate_code_units() {
    let compiled = compile_leaf(
        "function f(){ return (\"\\u00ff\", \"\\u0100\", \"\\uD800\", \"\\uDC00\", \"😀\", \"\\uD83D\\uDE00\", \"�\"); }",
        "f",
    );

    assert!(compiled.constants().is_empty());
    assert_eq!(compiled.atoms().len(), 7);
    assert_eq!(
        compiled.atoms()[0].string().latin1_units(),
        Some(&[0xff][..])
    );
    assert_eq!(
        compiled.atoms()[1].string().utf16_units(),
        Some(&[0x0100][..])
    );
    assert_eq!(
        compiled.atoms()[2].string().utf16_units(),
        Some(&[0xd800][..])
    );
    assert_eq!(
        compiled.atoms()[3].string().utf16_units(),
        Some(&[0xdc00][..])
    );
    assert_eq!(
        compiled.atoms()[4].string().utf16_units(),
        Some(&[0xd83d, 0xde00][..])
    );
    assert_eq!(
        compiled.atoms()[5].string().utf16_units(),
        Some(&[0xfffd][..])
    );
    assert_eq!(
        compiled.atoms()[6]
            .string()
            .code_units()
            .collect::<Vec<_>>(),
        ['f' as u16]
    );

    let atom_pushes = instructions(&compiled)
        .into_iter()
        .filter(|(opcode, _)| *opcode == FinalOpcode::PushAtomValue)
        .collect::<Vec<_>>();
    assert_eq!(atom_pushes.len(), 7);
    assert_eq!(
        atom_pushes[4],
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(4))
        )
    );
    assert_eq!(atom_pushes[5], atom_pushes[4]);
}

#[test]
fn string_number_and_function_constants_keep_one_source_order_domain() {
    let tree = compile_tree(
        "function outer(){ return (\"0\", function(){}, 1.5, \"1\", \"x\"); }",
        "outer",
    );
    let outer = tree.root();

    assert_eq!(outer.constants().len(), 4);
    assert_eq!(
        string_value(&outer.constants()[0])
            .code_units()
            .collect::<Vec<_>>(),
        ['0' as u16]
    );
    assert!(matches!(
        outer.constants()[1],
        CompiledConstant::Function(_)
    ));
    assert_eq!(
        outer.constants()[2],
        CompiledConstant::Value(CompilerConstantValue::Number(Binary64Constant::from_f64(
            1.5
        )))
    );
    assert_eq!(
        string_value(&outer.constants()[3])
            .code_units()
            .collect::<Vec<_>>(),
        ['1' as u16]
    );
    assert_eq!(outer.atoms().len(), 2);
    assert_eq!(
        outer.atoms()[0].string().code_units().collect::<Vec<_>>(),
        ['x' as u16]
    );
    assert_eq!(
        outer.atoms()[1].string().code_units().collect::<Vec<_>>(),
        "outer".encode_utf16().collect::<Vec<_>>()
    );
}

#[test]
fn parent_and_child_functions_own_independent_atom_index_domains() {
    let tree = compile_tree(
        "function outer(){ return (\"shared\", function(){ return \"shared\"; }); }",
        "outer",
    );
    let outer = tree.root();
    let child_constant = outer.constants()[0]
        .function()
        .expect("nested function constant");
    let child = tree
        .function(child_constant.executable())
        .expect("nested function");

    assert_eq!(outer.atoms().len(), 2);
    assert_eq!(child.atoms().len(), 1);
    assert_eq!(outer.atoms()[0], child.atoms()[0]);
    assert_eq!(
        outer.atoms()[1].string().code_units().collect::<Vec<_>>(),
        "outer".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(tree.function_graph().root().atoms(), outer.atoms());
    assert_eq!(
        tree.function_graph()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("verified child")
            .atoms(),
        child.atoms()
    );
    assert!(instructions(outer).contains(&(
        FinalOpcode::PushAtomValue,
        Operands::Atom(AtomPoolIndex::new(0))
    )));
    assert_eq!(
        instructions(child),
        [
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0))
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn string_constants_cross_the_compact_boundary_and_atoms_do_not_shift_it() {
    let mut source = String::from("function outer(){ return (");
    for index in 0..256 {
        if index != 0 {
            source.push(',');
        }
        source.push_str("\"0\"");
    }
    source.push_str(",\"atom\",function(){}); }");

    let tree = compile_tree(&source, "outer");
    let outer = tree.root();
    assert_eq!(outer.constants().len(), 257);
    assert_eq!(outer.atoms().len(), 2);
    assert!(
        outer.constants()[..256]
            .iter()
            .all(|constant| string_value(constant).code_units().eq(['0' as u16]))
    );
    assert!(matches!(
        outer.constants()[256],
        CompiledConstant::Function(_)
    ));

    let pool_instructions = instructions(outer)
        .into_iter()
        .filter(|(opcode, _)| {
            matches!(
                opcode,
                FinalOpcode::PushConst8
                    | FinalOpcode::PushConst
                    | FinalOpcode::PushAtomValue
                    | FinalOpcode::FClosure8
                    | FinalOpcode::FClosure
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pool_instructions[255],
        (FinalOpcode::PushConst8, Operands::Const8(u8::MAX))
    );
    assert_eq!(
        pool_instructions[256],
        (
            FinalOpcode::PushAtomValue,
            Operands::Atom(AtomPoolIndex::new(0))
        )
    );
    assert_eq!(
        pool_instructions[257],
        (FinalOpcode::FClosure, Operands::Const(256))
    );
}

#[test]
fn directives_do_not_leave_dead_string_payloads_in_compiler_artifacts() {
    let compiled = compile_leaf(
        "function f(){ \"0\"; \"use strict\"; return \"value\"; }",
        "f",
    );

    assert!(compiled.control_flow().function_header().mode().is_strict());
    assert!(compiled.constants().is_empty());
    assert_eq!(compiled.atoms().len(), 2);
    assert_eq!(
        compiled.atoms()[0]
            .string()
            .code_units()
            .collect::<Vec<_>>(),
        "value".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        compiled.atoms()[1]
            .string()
            .code_units()
            .collect::<Vec<_>>(),
        ['f' as u16]
    );

    let escaped = compile_leaf("function g(){ \"use\\x20strict\"; return \"value\"; }", "g");
    assert!(!escaped.control_flow().function_header().mode().is_strict());
    assert!(escaped.constants().is_empty());
    assert_eq!(escaped.atoms().len(), 2);
}

#[test]
fn compiler_string_constructor_accepts_arc_owned_code_units() {
    let units: Arc<[u16]> = Arc::from([0xd800, 0x0061]);
    let value = CompilerString::try_from_code_units(Arc::clone(&units))
        .expect("fixture string fits the compatible length");
    assert_eq!(
        value.code_units().collect::<Vec<_>>(),
        units.iter().copied().collect::<Vec<_>>()
    );
}
