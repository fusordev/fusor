use quickjs_bytecode::{
    CompilerBindingKind, CompilerExecutableKind, FinalOpcode, VerificationLimits,
};
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
fn a_named_base_class_expression_uses_the_same_typed_definition_path() {
    let tree = compile(
        "function make(){let Result=class Box{static self(){return Box;}};return Result;}",
        "make",
    );
    assert_eq!(tree.functions().len(), 3);
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("synthesized expression class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
}

#[test]
fn a_direct_anonymous_base_class_initializer_uses_its_binding_name() {
    let tree = compile(
        "function make(){let Result=class{static answer(){return 7;}};return Result;}",
        "make",
    );
    assert_eq!(tree.functions().len(), 3);
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("synthesized anonymous class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
    assert!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass),
        "the inferred name is supplied to define_class, not a post-closure SetName"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn a_direct_anonymous_base_class_assignment_uses_its_target_name() {
    let tree = compile(
        "function make(){let Result;return Result=class{static answer(){return 7;}};}",
        "make",
    );
    assert_eq!(tree.functions().len(), 3);
    assert!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass),
        "the inferred name is supplied to define_class, not a post-closure SetName"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn anonymous_base_class_binding_defaults_use_their_binding_names() {
    let tree = compile(
        "function make(){let [ArrayName=class{}]=[];let {value:ObjectName=class{}}={};return [ArrayName,ObjectName];}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        2,
        "both defaults receive their inferred name through define_class"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn anonymous_base_class_assignment_defaults_use_their_target_names() {
    let tree = compile(
        "function make(){let ArrayName;[ArrayName=class{}]=[];let ObjectName;({value:ObjectName=class{}}={});return [ArrayName,ObjectName];}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        2,
        "both defaults receive their inferred name through define_class"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn computed_anonymous_base_classes_use_the_typed_computed_name_path() {
    let tree = compile(
        "function make(key){let holder={[key]:class{value(){return 3;}}};class Box{static[key]=class{static value(){return 4;}}}return [holder,Box];}",
        "make",
    );
    let opcodes = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::SetNameComputed)
            .count(),
        2
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::DefineArrayEl)
            .count(),
        2
    );
}

#[test]
fn named_class_member_writes_retain_a_dedicated_immutable_class_name_capture() {
    let tree = compile(
        "function make(){class Box{static replace(){Box=0;}}return Box;}",
        "make",
    );
    let method = tree
        .verified_bytecode()
        .function(quickjs_bytecode::FunctionTemplateId::new(2))
        .expect("static method template");
    assert!(
        method
            .metadata()
            .closures()
            .iter()
            .any(|definition| { definition.policy().kind() == CompilerBindingKind::ClassName })
    );
}

#[test]
fn computed_public_class_methods_use_the_typed_computed_definition_path() {
    let tree = compile(
        "function make(key){class Box{[key](){return 3;}static[key+'Static'](){return 7;}}return Box;}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::DefineMethodComputed
            })
            .count(),
        2
    );
}

#[test]
fn async_generator_class_methods_are_owned_by_their_definition() {
    let tree = compile(
        "function make(){class Box{async *values(){yield 1;}}return Box;}",
        "make",
    );
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::AsyncGeneratorMethod
    }));
}

#[test]
fn static_class_fields_lower_to_the_typed_field_definition_path() {
    let tree = compile(
        "function make(seed){class Box{static answer=seed+1;static self=Box;static Nested=class{};static empty;}return Box;}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineField)
            .count(),
        4
    );
}

#[test]
fn computed_static_class_fields_use_the_typed_dynamic_definition_path() {
    let tree = compile(
        "function make(key){class Box{static[key]=1;static[key+'Fn']=function(){};}return Box;}",
        "make",
    );
    let opcodes = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::DefineArrayEl)
            .count(),
        2
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::SetNameComputed)
            .count(),
        1
    );
}

#[test]
fn static_property_class_assignments_stay_fail_closed_until_empty_name_authority_exists() {
    let error = with_parsed_program(
        "function make(holder){return holder.Result=class{};}",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("make"))
                .expect("root function");
            context.compile_tree(&root, VerificationLimits::default())
        },
    )
    .expect("frontend")
    .expect_err("an anonymous class property assignment needs empty-name authority");
    assert!(matches!(
        error,
        quickjs_compiler::LeafCompilationError::Unsupported {
            feature: quickjs_compiler::UnsupportedLeafFeature::InferredFunctionName,
            ..
        }
    ));
}

#[test]
fn static_field_initializers_requiring_a_class_receiver_stay_fail_closed() {
    let error = with_parsed_program(
        "function make(){class Box{static receiver=this;}return Box;}",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("make"))
                .expect("root function");
            context.compile_tree(&root, VerificationLimits::default())
        },
    )
    .expect("frontend")
    .expect_err("class-bound static receiver needs its dedicated execution contract");
    assert!(matches!(
        error,
        quickjs_compiler::LeafCompilationError::Unsupported {
            feature: quickjs_compiler::UnsupportedLeafFeature::UnsupportedExpression,
            ..
        }
    ));
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
