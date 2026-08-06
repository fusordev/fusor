use quickjs_bytecode::{CompilerExecutableKind, FinalOpcode, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree, WritePolicy};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("root function");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect("base class tree")
        },
    )
    .expect("frontend")
}

#[test]
fn explicit_base_class_constructor_and_public_methods_lower_to_typed_class_bytecode() {
    let tree = compile(
        "function make(){class Box{constructor(value){this.value=value;}get doubled(){return this.value*2;}static answer(){return 7;}}return Box;}",
        "make",
    );
    let root = tree.root();
    let opcodes = root
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.windows(2).any(|pair| {
        matches!(
            pair,
            [
                FinalOpcode::FClosure8 | FinalOpcode::FClosure,
                FinalOpcode::DefineClass
            ]
        )
    }));
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::DefineMethod)
            .count(),
        2
    );
    assert_eq!(tree.functions().len(), 4);
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
    assert!(
        !tree.functions()[1]
            .control_flow()
            .function_header()
            .flags()
            .has_prototype(),
        "define_class owns the public prototype rather than the closure header"
    );
    assert!(
        tree.functions().iter().skip(1).all(|function| function
            .control_flow()
            .function_header()
            .mode()
            .is_strict()),
        "class constructors and methods are strict irrespective of the enclosing function"
    );
}

#[test]
fn a_base_class_without_a_constructor_uses_a_synthesized_typed_template() {
    let tree = compile(
        "function make(){class Box{static answer(){return 7;}}return Box;}",
        "make",
    );
    assert_eq!(
        tree.functions().len(),
        3,
        "one synthesized constructor and one method template"
    );
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("synthesized class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
}

#[test]
fn named_base_class_members_capture_a_distinct_immutable_class_name_cell() {
    let tree = compile(
        "function make(){class Box{constructor(){}static self(){return Box;}}}",
        "make",
    );
    let class_bindings = tree
        .root()
        .storage_plan()
        .bindings()
        .iter()
        .filter(|binding| binding.name() == "Box")
        .collect::<Vec<_>>();
    assert_eq!(
        class_bindings.len(),
        2,
        "outer and inner class names differ"
    );
    assert!(
        class_bindings.iter().any(|binding| {
            binding.is_frame_captured() && binding.policy().writes() == WritePolicy::Immutable
        }),
        "the method must capture the immutable synthetic class-name cell"
    );
    let opcodes = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::CloseLoc));
}
