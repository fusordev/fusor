use fusor_bytecode::{AtomPoolIndex, BytecodePc, FinalOpcode, Operands, VerificationLimits};
use fusor_compiler::{
    CompilationContext, CompiledFunction, CompiledFunctionTree, CompiledLeafFunction,
};
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("ordinary object lowering must succeed")
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
                .expect("named function executable");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("ordinary object method tree must compile")
        },
    )
    .expect("front-end acceptance")
}

fn instructions(compiled: &CompiledLeafFunction) -> Vec<(FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn tree_instructions(compiled: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn inferred_names(function: &CompiledFunction) -> Vec<String> {
    let instructions = tree_instructions(function);
    instructions
        .iter()
        .enumerate()
        .filter_map(|(index, (opcode, operands))| {
            (*opcode == FinalOpcode::SetName).then_some((index, *operands))
        })
        .map(|(index, operands)| {
            assert!(matches!(
                instructions[index - 1],
                (FinalOpcode::FClosure | FinalOpcode::FClosure8, _)
            ));
            let Operands::Atom(atom) = operands else {
                panic!("set_name must carry one atom operand");
            };
            String::from_utf16(
                &function.atoms()[atom.get() as usize]
                    .string()
                    .code_units()
                    .collect::<Vec<_>>(),
            )
            .expect("static property atom is valid UTF-16")
        })
        .collect()
}

fn source_slice_at<'source>(
    compiled: &CompiledLeafFunction,
    source: &'source str,
    pc: BytecodePc,
) -> &'source str {
    let span = compiled
        .source_instructions()
        .iter()
        .find(|entry| entry.pc() == pc)
        .expect("source entry at final instruction PC")
        .span();
    &source[span.start as usize..span.end as usize]
}

fn atom_code_units(compiled: &CompiledLeafFunction, index: u32) -> Vec<u16> {
    compiled.atoms()[index as usize]
        .string()
        .code_units()
        .collect()
}

#[test]
fn empty_object_literal_uses_the_ordinary_object_opcode() {
    let compiled = compile("function make(){return {};}", "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn static_data_properties_are_defined_in_source_order() {
    let compiled = compile("function make(){return {alpha:1,beta:2};}", "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Push2, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "alpha".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        atom_code_units(&compiled, 1),
        "beta".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
}

#[test]
fn shorthand_data_properties_read_bindings_and_define_static_keys() {
    let source = "function make(alpha,beta){return {alpha,beta};}";
    let compiled = compile(source, "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (
                FinalOpcode::DefineField,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (
                FinalOpcode::DefineField,
                Operands::Atom(AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "alpha".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        atom_code_units(&compiled, 1),
        "beta".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);

    let definitions = compiled
        .control_flow()
        .instructions()
        .iter()
        .filter_map(|instruction| {
            let decoded = instruction.decoded();
            (decoded.instruction().opcode() == FinalOpcode::DefineField).then_some(decoded.pc())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        definitions
            .iter()
            .map(|pc| source_slice_at(&compiled, source, *pc))
            .collect::<Vec<_>>(),
        ["alpha", "beta"],
        "shorthand mappings retain the exact source property spelling"
    );
}

#[test]
fn quoted_data_keys_use_exact_cooked_utf16_and_raw_source_spans() {
    let source = r#"function make(){return {"\u0061":1,"a":2,"\uD800":3,"":4,"0":5,0:6};}"#;
    let compiled = compile(source, "make");
    let definitions = compiled
        .control_flow()
        .instructions()
        .iter()
        .filter_map(|instruction| {
            let decoded = instruction.decoded();
            (decoded.instruction().opcode() == FinalOpcode::DefineField)
                .then_some((decoded.pc(), decoded.instruction().operands()))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        definitions
            .iter()
            .map(|(_, operands)| *operands)
            .collect::<Vec<_>>(),
        [
            Operands::Atom(AtomPoolIndex::new(0)),
            Operands::Atom(AtomPoolIndex::new(0)),
            Operands::Atom(AtomPoolIndex::new(1)),
            Operands::Atom(AtomPoolIndex::new(2)),
            Operands::Atom(AtomPoolIndex::new(3)),
            Operands::Atom(AtomPoolIndex::new(3)),
        ],
        "cooked-equivalent strings and numeric zero share canonical property atoms"
    );
    assert_eq!(atom_code_units(&compiled, 0), vec![u16::from(b'a')]);
    assert_eq!(atom_code_units(&compiled, 1), vec![0xd800]);
    assert_eq!(atom_code_units(&compiled, 2), Vec::<u16>::new());
    assert_eq!(atom_code_units(&compiled, 3), vec![u16::from(b'0')]);
    assert!(!compiled.atoms()[0].is_static_property_only());
    assert!(!compiled.atoms()[1].is_static_property_only());
    assert!(compiled.atoms()[2].is_static_property_only());
    assert!(compiled.atoms()[3].is_static_property_only());
    assert_eq!(
        definitions
            .iter()
            .map(|(pc, _)| source_slice_at(&compiled, source, *pc))
            .collect::<Vec<_>>(),
        [
            r#""\u0061":1"#,
            r#""a":2"#,
            r#""\uD800":3"#,
            r#""":4"#,
            r#""0":5"#,
            "0:6",
        ],
        "source mappings retain each raw property definition"
    );
    assert_eq!(
        compile_tree(source, "make").functions().len(),
        1,
        "empty and tagged-index keys survive complete graph verification"
    );
}

#[test]
fn numeric_and_bigint_data_keys_use_quickjs_canonical_spelling() {
    let source = r#"function make(){return {1:0,1.0:0,1e0:0,0x1:0,0b1:0,0o1:0,"1":0,1n:0,1e-6:0,1e-7:0,1e20:0,1e21:0,1e400:0,9007199254740993:0,4294967294:0,4294967295:0,0x10n:0,0b1_0000n:0,9_007_199_254_740_993n:0};}"#;
    let compiled = compile(source, "make");
    assert!(
        compiled.constants().is_empty(),
        "static numeric keys do not become runtime Number or BigInt constants"
    );
    let definitions = compiled
        .control_flow()
        .instructions()
        .iter()
        .filter_map(|instruction| {
            let decoded = instruction.decoded();
            (decoded.instruction().opcode() == FinalOpcode::DefineField)
                .then_some((decoded.pc(), decoded.instruction().operands()))
        })
        .collect::<Vec<_>>();
    let atom_indices = definitions
        .iter()
        .map(|(_, operands)| match operands {
            Operands::Atom(atom) => atom.get(),
            _ => panic!("DefineField has one static property atom"),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        atom_indices,
        [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 10],
        "Number, BigInt, and quoted spellings collide only after canonical conversion"
    );
    for (index, expected) in [
        "1",
        "0.000001",
        "1e-7",
        "100000000000000000000",
        "1e+21",
        "Infinity",
        "9007199254740992",
        "4294967294",
        "4294967295",
        "16",
        "9007199254740993",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            atom_code_units(&compiled, u32::try_from(index).expect("atom index")),
            expected.encode_utf16().collect::<Vec<_>>()
        );
    }
    assert!(compiled.atoms()[0].is_static_property_only());
    assert!(compiled.atoms()[9].is_static_property_only());
    assert!(
        !compiled.atoms()[7].is_static_property_only(),
        "array-index canonicalization beyond QuickJS's tagged-i32 atom range occurs at runtime"
    );
    assert!(!compiled.atoms()[10].is_static_property_only());
    assert_eq!(
        definitions[16..]
            .iter()
            .map(|(pc, _)| source_slice_at(&compiled, source, *pc))
            .collect::<Vec<_>>(),
        ["0x10n:0", "0b1_0000n:0", "9_007_199_254_740_993n:0"],
        "BigInt source mappings retain bases and separators"
    );
    assert_eq!(
        compile_tree(source, "make").functions().len(),
        1,
        "canonical Number and BigInt keys survive complete graph verification"
    );
}

#[test]
fn static_methods_and_accessors_use_typed_closures_and_exact_enumerable_flags() {
    let tree = compile_tree(
        "function make(){return {method(){return 1;},get value(){return 2;},set value(next){next;}};}",
        "make",
    );
    let root = tree.root();

    assert_eq!(
        tree_instructions(root),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(0),
                    value: 4,
                },
            ),
            (FinalOpcode::FClosure8, Operands::Const8(1)),
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(1),
                    value: 5,
                },
            ),
            (FinalOpcode::FClosure8, Operands::Const8(2)),
            (
                FinalOpcode::DefineMethod,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(1),
                    value: 6,
                },
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(tree.functions().len(), 4);
    assert_eq!(
        atom_code_units(root, 0),
        "method".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        atom_code_units(root, 1),
        "value".encode_utf16().collect::<Vec<_>>()
    );
    for ((child, verified), expected_source) in tree.functions()[1..]
        .iter()
        .zip(tree.verified_bytecode().functions().skip(1))
        .zip([
            "method(){return 1;}",
            "get value(){return 2;}",
            "set value(next){next;}",
        ])
    {
        let header = child.control_flow().function_header();
        assert!(!header.flags().has_prototype());
        assert!(!header.flags().needs_home_object());
        assert_eq!(
            verified.metadata().function_name(),
            None,
            "DefineMethod assigns the observable property-derived name"
        );
        assert_eq!(
            verified.metadata().source().function_source(),
            expected_source,
            "the retained method source includes its property key and accessor prefix"
        );
    }
}

#[test]
fn quoted_and_numeric_methods_preserve_canonical_names_and_raw_function_sources() {
    let source = r#"function make(){return {"\u0061 b"(){return 1;},2(){return 2;},get "\u0062 c"(){return 3;},get 3(){return 4;},set '\x64 e'(next){next;},set 4(next){next;}};}"#;
    let tree = compile_tree(source, "make");
    let root = tree.root();
    let definitions = tree_instructions(root)
        .into_iter()
        .filter_map(|(opcode, operands)| (opcode == FinalOpcode::DefineMethod).then_some(operands))
        .collect::<Vec<_>>();

    assert_eq!(
        definitions,
        [
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 4,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 4,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(2),
                value: 5,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(3),
                value: 5,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(4),
                value: 6,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(5),
                value: 6,
            },
        ]
    );
    for (index, expected) in ["a b", "2", "b c", "3", "d e", "4"].iter().enumerate() {
        assert_eq!(
            atom_code_units(root, u32::try_from(index).expect("atom index")),
            expected.encode_utf16().collect::<Vec<_>>()
        );
    }
    for index in [1_usize, 3, 5] {
        assert!(root.atoms()[index].is_static_property_only());
    }
    for (verified, expected_source) in tree.verified_bytecode().functions().skip(1).zip([
        r#""\u0061 b"(){return 1;}"#,
        "2(){return 2;}",
        r#"get "\u0062 c"(){return 3;}"#,
        "get 3(){return 4;}",
        r"set '\x64 e'(next){next;}",
        "set 4(next){next;}",
    ]) {
        assert_eq!(
            verified.metadata().function_name(),
            None,
            "DefineMethod owns the canonical inferred method/accessor name"
        );
        assert_eq!(
            verified.metadata().source().function_source(),
            expected_source,
            "retained source preserves the exact raw property spelling"
        );
    }
}

#[test]
fn bigint_methods_and_accessors_keep_exact_values_names_and_raw_sources() {
    let source = r"function make(){return {0x10n(){return 1;},get 0b10n(){return 2;},set 0o3n(next){next;},9_007_199_254_740_993n(){return 3;}};}";
    let tree = compile_tree(source, "make");
    let root = tree.root();
    let definitions = tree_instructions(root)
        .into_iter()
        .filter_map(|(opcode, operands)| (opcode == FinalOpcode::DefineMethod).then_some(operands))
        .collect::<Vec<_>>();

    assert_eq!(
        definitions,
        [
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 4,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(1),
                value: 5,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(2),
                value: 6,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(3),
                value: 4,
            },
        ]
    );
    for (index, expected) in ["16", "2", "3", "9007199254740993"].iter().enumerate() {
        assert_eq!(
            atom_code_units(root, u32::try_from(index).expect("atom index")),
            expected.encode_utf16().collect::<Vec<_>>()
        );
    }
    assert_eq!(
        tree.verified_bytecode()
            .functions()
            .skip(1)
            .map(|function| function.metadata().source().function_source())
            .collect::<Vec<_>>(),
        [
            "0x10n(){return 1;}",
            "get 0b10n(){return 2;}",
            "set 0o3n(next){next;}",
            "9_007_199_254_740_993n(){return 3;}",
        ]
    );
}

/// A `__proto__` method or accessor is an ordinary own property.
#[test]
fn quoted_proto_methods_and_accessors_stay_ordinary_own_properties() {
    let tree = compile_tree(
        r#"function make(){return {"__proto__"(){return 1;},get "__proto__"(){return 2;},set "__proto__"(next){next;}};}"#,
        "make",
    );
    let definitions = tree_instructions(tree.root())
        .into_iter()
        .filter_map(|(opcode, operands)| (opcode == FinalOpcode::DefineMethod).then_some(operands))
        .collect::<Vec<_>>();

    assert_eq!(
        definitions,
        [
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 4,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 5,
            },
            Operands::AtomU8 {
                atom: AtomPoolIndex::new(0),
                value: 6,
            },
        ]
    );
    assert_eq!(
        atom_code_units(tree.root(), 0),
        "__proto__".encode_utf16().collect::<Vec<_>>()
    );
}

#[test]
fn object_methods_capture_outer_cells_and_lower_their_frontend_bodies() {
    let tree = compile_tree(
        "function make(value){return {read(){return value;},get current(){return this;},set current(next){value=next;}};}",
        "make",
    );
    let children = &tree.functions()[1..];

    assert_eq!(children.len(), 3);
    assert!(tree_instructions(&children[0]).iter().any(|instruction| {
        matches!(
            instruction,
            (FinalOpcode::GetVarRef0, Operands::NoneVarRef)
                | (FinalOpcode::GetVarRef, Operands::VarRef(0))
        )
    }));
    assert!(tree_instructions(&children[1]).contains(&(FinalOpcode::PushThis, Operands::None)));
    let setter_instructions = tree_instructions(&children[2]);
    // The setter writes the captured `value` cell through a var-ref pair
    // (`MakeVarRefRef` ... `PutRefValue`), not the direct slot family.
    assert!(
        setter_instructions
            .iter()
            .any(|instruction| matches!(instruction, (FinalOpcode::MakeVarRefRef, _))),
        "{setter_instructions:?}"
    );
    assert!(
        setter_instructions
            .iter()
            .any(|instruction| matches!(instruction, (FinalOpcode::PutRefValue, Operands::None))),
        "{setter_instructions:?}"
    );
}

#[test]
fn object_methods_and_accessors_lower_super_through_the_home_object() {
    let tree = compile_tree(
        "function make(){return {read(){return super.value;},call(){return super.method();},write(next){return super.value=next;},add(next){return super['value']+=next;},assign(next){return super.value||=next;},pre(){return ++super.value;},post(){return super['value']++;},get current(){return super.value;},set current(next){super.value=next;}};}",
        "make",
    );
    let opcodes = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::GetSuper));
    assert!(opcodes.contains(&FinalOpcode::GetSuperValue));
    assert!(opcodes.contains(&FinalOpcode::PutSuperValue));
    assert!(opcodes.contains(&FinalOpcode::Dup3));
    assert!(opcodes.contains(&FinalOpcode::Insert4));
    assert!(opcodes.contains(&FinalOpcode::Perm5));
    assert!(tree.functions().iter().any(|function| {
        function
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                matches!(
                    instruction.decoded().instruction().operands(),
                    Operands::U8(5)
                )
            })
    }));
}

#[test]
fn computed_super_assignment_converts_its_key_after_the_rhs() {
    let tree = compile_tree(
        "function make(){return {write(key,value){return super[key]=value;}};}",
        "make",
    );
    let instructions = tree_instructions(&tree.functions()[1]);
    let rhs = instructions
        .iter()
        .position(|instruction| instruction == &(FinalOpcode::GetArg1, Operands::NoneArg))
        .expect("assignment RHS read");
    let conversion = instructions
        .iter()
        .position(|instruction| instruction == &(FinalOpcode::ToPropKey, Operands::None))
        .expect("computed super key conversion");

    assert!(rhs < conversion, "{instructions:?}");
    assert!(instructions.windows(5).any(|window| {
        window
            == [
                (FinalOpcode::Swap, Operands::None),
                (FinalOpcode::ToPropKey, Operands::None),
                (FinalOpcode::Swap, Operands::None),
                (FinalOpcode::Insert4, Operands::None),
                (FinalOpcode::PutSuperValue, Operands::None),
            ]
    }));
}

#[test]
fn static_property_read_uses_the_function_local_atom() {
    let compiled = compile("function read(object){return object.value;}", "read");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetField, Operands::Atom(AtomPoolIndex::new(0)),),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "value".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn static_property_assignment_preserves_and_returns_the_rhs() {
    let source = "function write(object,value){return object.field=value;}";
    let compiled = compile(source, "write");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Insert2, Operands::None),
            (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(0)),),
            (FinalOpcode::Return, Operands::None),
        ],
        "Insert2 retains the RHS below the object/value pair consumed by PutField"
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "field".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 3);

    let put = compiled
        .control_flow()
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::PutField)
        .expect("PutField");
    assert!(
        source_slice_at(&compiled, source, put.decoded().pc()).contains("object.field"),
        "the property write source entry points at the static assignment target"
    );
}

#[test]
fn static_method_call_keeps_the_base_object_as_receiver() {
    let source = "function invoke(object){return object.method();}";
    let compiled = compile(source, "invoke");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (
                FinalOpcode::GetField2,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count: 0 },
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "method".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);

    let call = compiled
        .control_flow()
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::CallMethod)
        .expect("CallMethod");
    assert_eq!(
        source_slice_at(&compiled, source, call.decoded().pc()),
        "object.method()"
    );
}

#[test]
fn parentheses_preserve_a_member_reference_but_sequences_do_not() {
    let parenthesized = compile(
        "function invoke(object){return ((object.method))();}",
        "invoke",
    );
    assert_eq!(
        instructions(&parenthesized),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (
                FinalOpcode::GetField2,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count: 0 },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        "parentheses do not erase a JavaScript reference or its receiver"
    );

    let sequence = compile(
        "function invoke(object){return (0,object.method)();}",
        "invoke",
    );
    assert_eq!(
        instructions(&sequence),
        [
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetField, Operands::Atom(AtomPoolIndex::new(0)),),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::Return, Operands::None),
        ],
        "a sequence expression yields a value rather than a bound reference"
    );
}

#[test]
fn strict_this_expression_uses_push_this() {
    let source = "function current(){\"use strict\";return this;}";
    let compiled = compile(source, "current");

    assert!(compiled.control_flow().function_header().mode().is_strict());
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::PushThis, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(0)),
        "this"
    );
}

#[test]
fn strict_direct_call_remains_receiverless() {
    let compiled = compile(
        "function invoke(callback){\"use strict\";return callback();}",
        "invoke",
    );

    assert!(compiled.control_flow().function_header().mode().is_strict());
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (
                FinalOpcode::TailCall,
                Operands::NPop {
                    argument_count: 0
                }
            ),
        ],
        "a strict direct call must not synthesize a base-object receiver"
    );
}

#[test]
fn computed_member_reads_and_simple_assignments_use_quickjs_stack_shapes() {
    let source = "function access(object,key,value){object[key]=value;return object[key];}";
    let compiled = compile(source, "access");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::GetArg2, Operands::NoneArg),
            (FinalOpcode::Insert3, Operands::None),
            (FinalOpcode::PutArrayEl, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::GetArrayEl, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 4);

    let computed_operations = compiled
        .control_flow()
        .instructions()
        .iter()
        .filter(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::PutArrayEl | FinalOpcode::GetArrayEl
            )
        })
        .map(|instruction| source_slice_at(&compiled, source, instruction.decoded().pc()))
        .collect::<Vec<_>>();
    assert_eq!(computed_operations, ["object[key]", "object[key]"]);
}

#[test]
fn computed_data_properties_convert_keys_before_values_and_keep_proto_ordinary() {
    let source = r#"function make(key,value){return {[key]:value,["__proto__"]:1};}"#;
    let compiled = compile(source, "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::ToPropKey, Operands::None),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (
                FinalOpcode::PushAtomValue,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::ToPropKey, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "__proto__".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 3);
}

#[test]
fn computed_data_key_anchor_survives_complex_rhs_control_flow_and_assignment_shuffles() {
    let source = "function make(key,condition,target,other,one,two){\
        return {[key]:condition?(target[other]=one):(target[other]=two)};\
    }";
    let tree = compile_tree(source, "make");
    let opcodes = tree_instructions(tree.root())
        .into_iter()
        .map(|(opcode, _)| opcode)
        .collect::<Vec<_>>();

    let key_conversion = opcodes
        .iter()
        .position(|opcode| *opcode == FinalOpcode::ToPropKey)
        .expect("computed key conversion");
    let branch = opcodes
        .iter()
        .position(|opcode| matches!(opcode, FinalOpcode::IfFalse | FinalOpcode::IfFalse8))
        .expect("conditional RHS branch");
    let definition = opcodes
        .iter()
        .position(|opcode| *opcode == FinalOpcode::DefineArrayEl)
        .expect("computed data definition");

    assert!(key_conversion < branch);
    assert!(branch < definition);
    assert_eq!(
        opcodes
            .iter()
            .filter(|opcode| **opcode == FinalOpcode::Insert3)
            .count(),
        2
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|opcode| **opcode == FinalOpcode::PutArrayEl)
            .count(),
        2
    );
}

#[test]
fn computed_methods_getters_and_setters_use_typed_computed_definitions() {
    let source = "function make(key){return {\
        [key](){return 1;},\
        get [key](){return 2;},\
        set [key](value){}\
    };}";
    let tree = compile_tree(source, "make");
    let root = tree.root();

    assert_eq!(
        tree_instructions(root),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::DefineMethodComputed, Operands::U8(4)),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::FClosure8, Operands::Const8(1)),
            (FinalOpcode::DefineMethodComputed, Operands::U8(5)),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::FClosure8, Operands::Const8(2)),
            (FinalOpcode::DefineMethodComputed, Operands::U8(6)),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(root.control_flow().computed_stack_size(), 3);
}

#[test]
fn static_anonymous_function_data_properties_exclude_the_proto_setter_from_name_inference() {
    let tree = compile_tree(
        r#"function make(){return {
            identifier:function(){},
            "quoted":(function(){}),
            1:function(){},
            1n:function(){},
            "__proto__":function(){}
        };}"#,
        "make",
    );
    let root = tree.root();

    assert_eq!(inferred_names(root), ["identifier", "quoted", "1", "1"]);
    assert_eq!(
        root.constants()
            .iter()
            .filter(|constant| constant.function().is_some())
            .count(),
        5,
        "each static property still evaluates its anonymous function"
    );
    assert_eq!(
        tree_instructions(root)
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::SetProto)
            .count(),
        1,
        "the non-computed __proto__ form is a prototype setter and does not use NamedEvaluation"
    );
}

#[test]
fn computed_anonymous_function_data_properties_use_the_exact_name_definition_sequence() {
    let tree = compile_tree(
        "function make(first,second){return {\
            [first]:function(){},\
            [second]:(function(){})\
        };}",
        "make",
    );
    let root = tree.root();

    assert_eq!(
        tree_instructions(root),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::ToPropKey, Operands::None),
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::SetNameComputed, Operands::None),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::ToPropKey, Operands::None),
            (FinalOpcode::FClosure8, Operands::Const8(1)),
            (FinalOpcode::SetNameComputed, Operands::None),
            (FinalOpcode::DefineArrayEl, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(root.control_flow().computed_stack_size(), 3);
}

#[test]
fn object_spread_retains_the_literal_target_and_discards_its_copy_operands() {
    let compiled = compile("function make(value){return {...value};}", "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Undefined, Operands::None),
            (FinalOpcode::CopyDataProperties, Operands::U8(0b0000_0110)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 3);
}

#[test]
fn anonymous_class_computed_data_properties_use_the_typed_computed_name_path() {
    let tree = compile_tree("function make(key){return {[key]:class {}};}", "make");
    let instructions = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(instructions.contains(&FinalOpcode::SetNameComputed));
    assert!(instructions.contains(&FinalOpcode::DefineArrayEl));
    assert_eq!(tree.functions().len(), 2);
}

/// `delete` lowers to the pinned `OP_delete` shape: the base, then the key,
/// then one `Delete`. `QuickJS` builds the same sequence by rewriting the
/// preceding member read into a key push (`quickjs.c:27395-27437`).
#[test]
fn computed_delete_lowers_to_base_key_then_delete() {
    let compiled = compile(
        "function make(object,key){return delete object[key];}",
        "make",
    );
    assert_eq!(
        instructions(&compiled),
        vec![
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Delete, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

/// A static `delete` pushes the property atom instead of evaluating a key
/// expression, which is the `OP_get_field` rewrite in the pinned compiler.
#[test]
fn static_delete_pushes_the_property_atom_before_delete() {
    let compiled = compile("function make(object){return delete object.field;}", "make");
    let lowered = instructions(&compiled);
    assert_eq!(lowered[0], (FinalOpcode::GetArg0, Operands::NoneArg));
    assert!(matches!(
        lowered[1],
        (FinalOpcode::PushAtomValue, Operands::Atom(_))
    ));
    assert_eq!(lowered[2], (FinalOpcode::Delete, Operands::None));
    assert_eq!(lowered[3], (FinalOpcode::Return, Operands::None));
}

/// `delete` of a non-reference still evaluates its operand and yields `true`.
/// The pinned oracle reports `delete (1 + 1)` as `true`.
#[test]
fn deleting_a_non_reference_drops_the_operand_and_pushes_true() {
    let compiled = compile("function make(value){return delete (value+1);}", "make");
    let lowered = instructions(&compiled);
    assert_eq!(lowered[0], (FinalOpcode::GetArg0, Operands::NoneArg));
    let tail = &lowered[lowered.len() - 3..];
    assert_eq!(tail[0], (FinalOpcode::Drop, Operands::None));
    assert_eq!(tail[1], (FinalOpcode::PushTrue, Operands::None));
    assert_eq!(tail[2], (FinalOpcode::Return, Operands::None));
}

/// A statically resolved declarative binding cannot be removed. The pinned
/// compiler folds this case to `push_false` rather than reading the binding.
#[test]
fn deleting_a_resolved_identifier_pushes_false_without_reading_it() {
    let compiled = compile("function make(value){return delete value;}", "make");
    assert_eq!(
        instructions(&compiled),
        vec![
            (FinalOpcode::PushFalse, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

/// The normative object-initializer `ProtoSetter` uses `set_proto` for every
/// static spelling, including escaped and quoted cooked property names.
#[test]
fn proto_setters_use_set_proto_in_every_static_spelling() {
    for source in [
        "function make(value){return {__proto__:value};}",
        "function make(value){return {\"__proto__\":value};}",
        r#"function make(value){return {"__pro\u0074o__":value};}"#,
    ] {
        let compiled = compile(source, "make");
        assert_eq!(
            instructions(&compiled),
            vec![
                (FinalOpcode::Object, Operands::None),
                (FinalOpcode::GetArg0, Operands::NoneArg),
                (FinalOpcode::SetProto, Operands::None),
                (FinalOpcode::Return, Operands::None),
            ],
            "{source}"
        );
    }
}

/// A computed `__proto__` key remains an ordinary own property and uses the
/// computed definition opcode rather than `ProtoSetter` semantics.
#[test]
fn a_computed_proto_key_still_defines_an_own_property() {
    let compiled = compile("function make(key,value){return {[key]:value};}", "make");
    let lowered = instructions(&compiled);
    assert!(
        lowered
            .iter()
            .all(|(opcode, _)| *opcode != FinalOpcode::SetProto),
        "a computed key must not use ProtoSetter semantics: {lowered:?}"
    );
    assert!(
        lowered
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::DefineArrayEl),
        "a computed key defines an own property: {lowered:?}"
    );
}
