use quickjs_bytecode::{CompilerExecutableKind, FinalOpcode, VerificationLimits};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, LeafCompilationError, UnsupportedLeafFeature,
};
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

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
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
                .expect_err("deferred class form must fail closed")
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
fn a_class_without_an_explicit_constructor_remains_fail_closed() {
    let source = "function make(){class Box{}}";
    let LeafCompilationError::Unsupported { feature, span } = compile_error(source, "make") else {
        panic!("a default class constructor must remain deferred");
    };
    assert_eq!(feature, UnsupportedLeafFeature::UnsupportedDeclaration);
    assert!(
        span.start < span.end,
        "diagnostic must retain a source range"
    );
}
