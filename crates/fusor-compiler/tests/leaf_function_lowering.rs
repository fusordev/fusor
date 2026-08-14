use fusor_bytecode::{
    BytecodePc, FinalOpcode, FunctionIndexDomains, FunctionKind, Operands, VerificationLimits,
};
use fusor_compiler::{
    CompilationContext, CompilationExecutable, CompiledLeafFunction, LeafCompilationError,
    UnsupportedLeafFeature,
};
use fusor_frontend::{
    CompilationGoal, FrontendOptions, GlobalScriptGoal, Span, with_parsed_program,
};

const SOURCE: &str = "function f(arg) { let local = arg; return local; }";

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
                .expect("leaf compilation must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
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
                .expect_err("unsupported leaf must fail closed")
        },
    )
    .expect("front-end acceptance")
}

#[test]
fn lexical_identifier_leaf_matches_the_quickjs_final_opcode_oracle() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompilationExecutable>();
    assert_send_sync::<CompiledLeafFunction>();

    let compiled = compile(SOURCE, "f");
    let flow = compiled.control_flow();
    let instructions = flow
        .instructions()
        .iter()
        .map(|instruction| {
            let decoded = instruction.decoded();
            (
                decoded.pc(),
                decoded.instruction().opcode(),
                decoded.instruction().operands(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instructions,
        [
            (
                BytecodePc::new(0),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(3), FinalOpcode::GetArg0, Operands::NoneArg,),
            (BytecodePc::new(4), FinalOpcode::PutLoc0, Operands::NoneLoc,),
            (
                BytecodePc::new(5),
                FinalOpcode::GetLocCheck,
                Operands::Loc(0),
            ),
            (BytecodePc::new(8), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(flow.domains(), FunctionIndexDomains::new(3, 0, 1, 1, 0));
    assert_eq!(flow.computed_stack_size(), 1);

    let header = flow.function_header();
    assert_eq!(header.kind(), FunctionKind::Normal);
    assert_eq!(header.flags().bits(), 0x0643);
    assert_eq!(header.defined_argument_count(), 1);
    assert_eq!(header.variable_reference_count(), 0);
    assert!(!header.mode().is_strict());

    assert_eq!(compiled.executable().index(), 1);
    assert_eq!(compiled.storage_plan().executables()[1].name(), Some("f"));
    assert_eq!(compiled.source_text(), SOURCE);
    assert_eq!(compiled.locals().len(), 1);
    assert_eq!(compiled.locals()[0].slot().index(), 0);
    assert_ne!(
        compiled.locals()[0].binding().index(),
        usize::from(compiled.locals()[0].slot().index()),
        "unit-global binding identity must not be encoded as a local slot"
    );
    assert_eq!(
        compiled
            .source_instructions()
            .iter()
            .map(|entry| entry.pc())
            .collect::<Vec<_>>(),
        [
            BytecodePc::new(0),
            BytecodePc::new(3),
            BytecodePc::new(4),
            BytecodePc::new(5),
            BytecodePc::new(8),
        ]
    );
}

#[test]
fn new_target_uses_the_typed_special_object_selector() {
    let compiled = compile("function f(){return new.target;}", "f");
    let instructions = compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instructions,
        [
            (FinalOpcode::SpecialObject, Operands::U8(3)),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert!(
        compiled
            .control_flow()
            .function_header()
            .flags()
            .new_target_allowed()
    );
}

#[test]
fn sloppy_duplicate_parameters_authorize_only_the_last_formal_positions() {
    let compiled = compile("function f(a,a,b){return arguments;}", "f");
    let flow = compiled.control_flow();
    let layout = flow
        .compiler_capture_layout()
        .expect("compiler arguments certificate");

    assert_eq!(layout.mapped_arguments(), Some([1, 2].as_slice()));
    assert!(flow.instructions().iter().any(|instruction| {
        let instruction = instruction.decoded().instruction();
        instruction.opcode() == FinalOpcode::SpecialObject
            && instruction.operands() == Operands::U8(1)
    }));
}

#[test]
fn expression_free_parameter_patterns_have_an_unmapped_entry_prologue() {
    let compiled = compile(
        "function f(keep,{value},[head,...tail]){\
            return keep+value+head+tail.length+arguments.length;}",
        "f",
    );
    let flow = compiled.control_flow();
    assert_eq!(flow.domains().argument_count(), 3);
    assert!(!flow.function_header().flags().has_simple_parameter_list());
    assert_eq!(
        flow.compiler_capture_layout()
            .expect("compiler arguments certificate")
            .mapped_arguments(),
        None
    );

    let instructions = flow
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();
    assert!(instructions.contains(&(FinalOpcode::SpecialObject, Operands::U8(0))));
    assert!(!instructions.contains(&(FinalOpcode::SpecialObject, Operands::U8(1))));
    assert!(instructions.contains(&(FinalOpcode::GetArg1, Operands::NoneArg)));
    assert!(instructions.contains(&(FinalOpcode::GetArg2, Operands::NoneArg)));
    assert!(
        instructions
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
    );
}

#[test]
fn formal_rest_starts_after_fixed_arguments_and_uses_the_unmapped_prologue() {
    let compiled = compile(
        "function f(keep,...[head,...tail]){\
            return keep+head+tail.length+arguments.length;}",
        "f",
    );
    let flow = compiled.control_flow();
    assert_eq!(flow.domains().argument_count(), 1);
    assert_eq!(flow.function_header().defined_argument_count(), 1);
    assert!(!flow.function_header().flags().has_simple_parameter_list());

    let instructions = flow
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();
    let arguments_site = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::SpecialObject, Operands::U8(0)))
        .expect("unmapped arguments object");
    let rest_site = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::Rest, Operands::U16(1)))
        .expect("formal rest allocation");
    assert!(arguments_site < rest_site);
    assert!(
        instructions[rest_site + 1..]
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::ForOfStart)
    );
}

#[test]
fn parameter_expression_prologue_activates_tdz_bindings_before_arguments_and_values() {
    let compiled = compile(
        "function f(first=1,{[first]:value=2}={},...[tail=3]){\
            return first+value+tail+arguments.length;}",
        "f",
    );
    let flow = compiled.control_flow();
    assert_eq!(flow.domains().argument_count(), 2);
    assert_eq!(flow.function_header().defined_argument_count(), 0);
    assert!(!flow.function_header().flags().has_simple_parameter_list());

    let instructions = flow
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();
    let activations = instructions
        .iter()
        .take_while(|(opcode, _)| *opcode == FinalOpcode::SetLocUninitialized)
        .count();
    assert_eq!(activations, 3);
    assert_eq!(
        instructions[activations],
        (FinalOpcode::SpecialObject, Operands::U8(0))
    );
    let first_value = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::GetArg0, Operands::NoneArg))
        .expect("first raw argument read");
    assert!(activations < first_value);
    assert!(instructions.contains(&(FinalOpcode::StrictEq, Operands::None)));
    assert!(instructions.contains(&(FinalOpcode::GetLocCheck, Operands::Loc(0))));
    assert!(instructions.contains(&(FinalOpcode::Rest, Operands::U16(2))));
}

#[test]
fn parameter_expression_body_var_copies_into_a_distinct_local() {
    let compiled = compile("function f(value=1){var value;return value;}", "f");
    let instructions = compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instructions.first(),
        Some(&(FinalOpcode::SetLocUninitialized, Operands::Loc(0)))
    );
    let parameter_read = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::GetLocCheck, Operands::Loc(0)))
        .expect("initialized parameter cell is copied");
    let body_write = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::PutLoc1, Operands::NoneLoc))
        .expect("body variable receives its copy");
    let body_read = instructions
        .iter()
        .rposition(|instruction| *instruction == (FinalOpcode::GetLoc1, Operands::NoneLoc))
        .expect("body reads select the copied variable cell");
    assert!(parameter_read < body_write && body_write < body_read);
}

#[test]
fn deepest_leaf_reads_forwarded_parent_cells_through_capture_slots() {
    let compiled = compile(
        "function outer(arg){ let local=1; function middle(){ function inner(){ return arg+local; } } }",
        "inner",
    );
    let flow = compiled.control_flow();
    let instructions = flow
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instructions,
        [
            (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
            (FinalOpcode::GetVarRefCheck, Operands::VarRef(1),),
            (FinalOpcode::Add, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(flow.domains(), FunctionIndexDomains::new(3, 0, 0, 0, 2));
    assert_eq!(flow.function_header().variable_reference_count(), 0);
    assert_eq!(
        flow.compiler_capture_layout()
            .expect("compiler capture layout")
            .bindings(),
        []
    );
}

#[test]
fn deepest_leaf_checked_capture_writes_keep_assignment_and_postfix_stack_order() {
    let compiled = compile(
        "function outer(){ let value=0; function inner(){ value=1; value+=2; return value++; } }",
        "inner",
    );
    let flow = compiled.control_flow();
    let instructions = flow
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instructions
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [
            FinalOpcode::MakeVarRefRef,
            FinalOpcode::Push1,
            FinalOpcode::Insert3,
            FinalOpcode::PutRefValue,
            FinalOpcode::Drop,
            FinalOpcode::MakeVarRefRef,
            FinalOpcode::GetRefValue,
            FinalOpcode::Push2,
            FinalOpcode::Add,
            FinalOpcode::Insert3,
            FinalOpcode::PutRefValue,
            FinalOpcode::Drop,
            FinalOpcode::GetVarRefCheck,
            FinalOpcode::PostInc,
            FinalOpcode::PutVarRefCheck,
            FinalOpcode::Return,
        ]
    );
    // Both var-ref creations name the captured `value` atom (pool index 0);
    // the checked postfix pair reads and writes the same variable reference
    // slot.
    assert_eq!(
        instructions[0].1.atom_pool_index().map(|index| index.get()),
        Some(0)
    );
    assert_eq!(
        instructions[5].1.atom_pool_index().map(|index| index.get()),
        Some(0)
    );
    assert_eq!(instructions[12].1, Operands::VarRef(0));
    assert_eq!(instructions[14].1, Operands::VarRef(0));
    assert_eq!(flow.domains(), FunctionIndexDomains::new(2, 0, 0, 0, 1));
    assert_eq!(flow.computed_stack_size(), 4);
}

#[test]
fn deepest_leaf_non_tdz_capture_postfix_uses_the_unchecked_put() {
    let compiled = compile(
        "function outer(){ var value=0; function inner(){ value=1; return value++; } }",
        "inner",
    );
    let instructions = compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instructions
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [
            FinalOpcode::MakeVarRefRef,
            FinalOpcode::Push1,
            FinalOpcode::Insert3,
            FinalOpcode::PutRefValue,
            FinalOpcode::Drop,
            FinalOpcode::GetVarRef0,
            FinalOpcode::PostInc,
            FinalOpcode::PutVarRef0,
            FinalOpcode::Return,
        ]
    );
    // The var-ref creation names the captured `value` atom (pool index 0);
    // the non-TDZ postfix pair reads and writes the unboxed variable
    // reference slot.
    assert_eq!(
        instructions[0].1.atom_pool_index().map(|index| index.get()),
        Some(0)
    );
    assert_eq!(
        compiled.control_flow().domains(),
        FunctionIndexDomains::new(2, 0, 0, 0, 1)
    );
}

#[test]
fn oxc_reference_identity_selects_the_exact_argument_slot() {
    let compiled = compile(
        "let unrelated; function f(first, selected) { let local = selected; return local; }",
        "f",
    );
    let instructions = compiled.control_flow().instructions();

    assert_eq!(
        instructions[1].decoded().instruction().opcode(),
        FinalOpcode::GetArg1
    );
    assert_eq!(
        instructions[1].decoded().instruction().operands(),
        Operands::NoneArg
    );
    assert_eq!(compiled.control_flow().domains().argument_count(), 2);
    assert_eq!(compiled.locals()[0].slot().index(), 0);
}

#[test]
fn strictness_is_retained_without_debug_or_eval_header_bits() {
    let compiled = compile(
        "function f(arg) { \"use strict\"; let local = arg; return local; }",
        "f",
    );
    let header = compiled.control_flow().function_header();

    assert!(header.mode().is_strict());
    assert!(header.flags().has_debug());
    assert!(!header.flags().is_eval());
}

#[test]
fn unsupported_leaf_shapes_fail_closed_at_source_spans() {
    let cases = [
        (
            "function f(arg) { function nested() {} let local = arg; return local; }",
            UnsupportedLeafFeature::NestedExecutable,
            "function nested() {}",
        ),
        (
            "function f(arg) { class A {} return arg; }",
            UnsupportedLeafFeature::NestedExecutable,
            "class A {}",
        ),
    ];

    for (source, expected_feature, expected_source) in cases {
        let error = compile_error(source, "f");
        let LeafCompilationError::Unsupported { feature, span } = error else {
            panic!("expected unsupported feature for {source}");
        };

        assert_eq!(feature, expected_feature, "{source}");
        assert_eq!(
            &source[span.start as usize..span.end as usize],
            expected_source,
            "{source}"
        );
    }
}

#[test]
fn anonymous_ordinary_function_expression_uses_the_same_owned_boundary() {
    let source = "const holder = function (arg) { const local = arg; return local; };";
    let compiled = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .nth(1)
                .expect("anonymous function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("anonymous ordinary leaf")
        },
    )
    .expect("front-end acceptance");

    assert_eq!(compiled.source_text(), source);
    assert_eq!(
        compiled.control_flow().function_header().flags().bits(),
        0x0643
    );
}

#[test]
fn object_function_value_and_method_use_distinct_exact_headers() {
    let value_source = "const object = { f: function (arg) { let local = arg; return local; } };";
    let compiled = with_parsed_program(
        value_source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .nth(1)
                .expect("function value executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("ordinary object-property function value")
        },
    )
    .expect("front-end acceptance");
    assert_eq!(
        compiled.control_flow().function_header().flags().bits(),
        0x0643
    );

    let method_source = "const object = { f(arg) { let local = arg; return local; } };";
    let compiled = with_parsed_program(
        method_source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .nth(1)
                .expect("object method executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("ordinary object method")
        },
    )
    .expect("front-end acceptance");
    let header = compiled.control_flow().function_header();
    assert_eq!(header.flags().bits(), 0x0742);
    assert!(!header.flags().has_prototype());
    assert!(!header.flags().needs_home_object());
}

#[test]
fn module_function_compiles_at_the_leaf_boundary() {
    let source = "export function f(arg) { let local = arg; return local; }";
    let compiled = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("f"))
                .expect("module function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("module lowering slice admits Module goals")
        },
    )
    .expect("front-end acceptance");
    let header = compiled.control_flow().function_header();
    assert!(header.mode().is_strict());
    assert!(header.flags().has_prototype());
    assert!(!header.flags().is_eval());
}

#[test]
fn same_index_executable_from_another_context_is_rejected() {
    let foreign = with_parsed_program(
        SOURCE,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            CompilationContext::new(unit)
                .expect("foreign storage planning")
                .executables()
                .nth(1)
                .expect("foreign function executable")
        },
    )
    .expect("front-end acceptance");
    assert_eq!(foreign.id().index(), 1);

    let error = with_parsed_program(
        "function local(arg) { let value = arg; return value; }",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            assert_eq!(
                context
                    .executables()
                    .nth(1)
                    .expect("local function executable")
                    .id()
                    .index(),
                foreign.id().index()
            );
            context
                .compile_leaf(&foreign, VerificationLimits::default())
                .expect_err("same-index foreign executable must be rejected")
        },
    )
    .expect("front-end acceptance");

    assert_eq!(
        error,
        LeafCompilationError::ForeignExecutable {
            executable: foreign.id()
        }
    );
}

#[test]
fn source_instruction_spans_retain_owned_frontend_coordinates() {
    let compiled = compile(SOURCE, "f");
    let spans = compiled
        .source_instructions()
        .iter()
        .map(|entry| entry.span())
        .collect::<Vec<Span>>();

    assert_eq!(
        spans
            .iter()
            .map(|span| &SOURCE[span.start as usize..span.end as usize])
            .collect::<Vec<_>>(),
        ["local", "arg", "local", "local", "return local;"]
    );
}

#[test]
fn multibyte_prefix_and_repeated_lowering_keep_deterministic_owned_provenance() {
    let source = "const π = 0;\nfunction f(arg) { let local = arg; return local; }";
    let first = compile(source, "f");
    let second = compile(source, "f");

    assert_eq!(first, second);
    assert_eq!(first.source_text(), source);
    assert!(
        first
            .source_instructions()
            .iter()
            .all(|entry| first.control_flow().is_instruction_start(entry.pc()))
    );
    assert!(
        first
            .source_instructions()
            .windows(2)
            .all(|pair| pair[0].pc() < pair[1].pc())
    );
    assert_eq!(
        first
            .source_instructions()
            .iter()
            .map(|entry| {
                let span = entry.span();
                &source[span.start as usize..span.end as usize]
            })
            .collect::<Vec<_>>(),
        ["local", "arg", "local", "local", "return local;"]
    );
}
