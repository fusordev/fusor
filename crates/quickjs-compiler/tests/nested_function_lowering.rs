use quickjs_bytecode::{
    CompilerCapturedBinding, CompilerClosureSource as VerifiedClosureSource, CompilerConstant,
    CompilerConstantKind, FinalOpcode, FunctionGraphVerificationErrorKind,
    FunctionGraphVerificationLimits, FunctionTemplateId, Operands, VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledClosureSource, CompiledConstant, CompiledFunction,
    CompiledFunctionTree, ExecutableId, LeafCompilationError, UnsupportedLeafFeature,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_tree(source: &str, root_name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("named root function");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect("function tree must compile")
        },
    )
    .expect("front-end acceptance")
}

fn opcodes(function: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
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

fn function_executable(constant: CompiledConstant) -> ExecutableId {
    constant
        .function()
        .expect("expected a function-template constant")
        .executable()
}

fn assert_three_level_function_graph(tree: &CompiledFunctionTree) {
    let graph = tree.function_graph();
    assert_eq!(graph.root_id(), FunctionTemplateId::new(0));
    assert_ne!(tree.root_executable().index(), 0);
    assert_eq!(graph.functions().len(), tree.functions().len());
    assert_eq!(
        graph.root().constants(),
        [CompilerConstant::Function(FunctionTemplateId::new(1))]
    );
    assert_eq!(
        graph
            .function(FunctionTemplateId::new(1))
            .expect("middle graph function")
            .closure_sources(),
        [VerifiedClosureSource::ParentVariableReference(0)]
    );
    assert_eq!(
        graph
            .function(FunctionTemplateId::new(2))
            .expect("inner graph function")
            .closure_sources(),
        [VerifiedClosureSource::ParentClosure(0)]
    );
    let middle = tree
        .function_by_template(FunctionTemplateId::new(1))
        .expect("middle compiler function");
    assert_ne!(middle.executable().index(), 1);
}

#[test]
fn nested_function_constants_connect_forwarded_capture_cells() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledFunction>();
    assert_send_sync::<CompiledFunctionTree>();

    let tree = compile_tree(
        "function outer(argument){ \
             return function(){ \
                 return function(){ return argument; }; \
             }; \
         }",
        "outer",
    );
    assert_eq!(tree.functions().len(), 3);
    assert_three_level_function_graph(&tree);

    let outer = tree.root();
    assert_eq!(outer.executable(), tree.root_executable());
    assert!(outer.closure_variables().is_empty());
    assert_eq!(outer.constants().len(), 1);
    assert_eq!(
        outer
            .control_flow()
            .compiler_constant_layout()
            .expect("tree function has explicit constant typing")
            .kinds(),
        [CompilerConstantKind::Function]
    );
    assert_eq!(
        opcodes(outer),
        [
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        outer
            .control_flow()
            .compiler_capture_layout()
            .expect("compiler capture layout")
            .bindings(),
        [CompilerCapturedBinding::Argument(0)]
    );

    let middle_id = function_executable(outer.constants()[0]);
    let middle = tree.function(middle_id).expect("middle function");
    assert_eq!(
        tree.function_by_template(FunctionTemplateId::new(1)),
        Some(middle)
    );
    assert_eq!(middle.constants().len(), 1);
    assert_eq!(middle.closure_variables().len(), 1);
    assert_eq!(middle.closure_variables()[0].slot().index(), 0);
    assert_eq!(
        middle.closure_variables()[0].source(),
        CompiledClosureSource::ParentVariableReference(0)
    );
    assert_eq!(
        opcodes(middle),
        [
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::Return, Operands::None),
        ]
    );

    let inner_id = function_executable(middle.constants()[0]);
    let inner = tree.function(inner_id).expect("inner function");
    assert!(inner.constants().is_empty());
    assert_eq!(inner.closure_variables().len(), 1);
    assert_eq!(
        inner.closure_variables()[0].binding(),
        middle.closure_variables()[0].binding()
    );
    assert_eq!(
        inner.closure_variables()[0].source(),
        CompiledClosureSource::ParentClosure(0)
    );
    assert_eq!(
        opcodes(inner),
        [
            (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
            (FinalOpcode::Return, Operands::None),
        ]
    );

    assert_eq!(
        tree.functions()
            .iter()
            .map(CompiledFunction::executable)
            .collect::<Vec<_>>(),
        [outer.executable(), middle.executable(), inner.executable()]
    );
}

#[test]
fn public_tree_compilation_preserves_captured_classic_for_rotation() {
    let tree = compile_tree(
        "function outer(){ \
             for (let iteration=0; iteration<1; iteration++) { \
                 (function(){ return iteration; }); \
             } \
         }",
        "outer",
    );
    assert_eq!(tree.functions().len(), 2);
    let outer = tree.root();
    let child = tree
        .function(function_executable(outer.constants()[0]))
        .expect("loop closure child");

    let capture_layout = outer
        .control_flow()
        .compiler_capture_layout()
        .expect("captured loop local layout");
    let [CompilerCapturedBinding::ScopedLocal(loop_local)] = capture_layout.bindings() else {
        panic!("loop head must own one scoped capture");
    };
    assert_eq!(child.closure_variables().len(), 1);
    assert_eq!(
        child.closure_variables()[0].source(),
        CompiledClosureSource::ParentVariableReference(0)
    );

    let outer_instructions = opcodes(outer);
    assert!(outer_instructions.contains(&(FinalOpcode::FClosure8, Operands::Const8(0))));
    assert!(outer_instructions.contains(&(
        FinalOpcode::CloseLoc,
        Operands::Loc(u16::try_from(*loop_local).expect("verified local fits u16")),
    )));
    assert_eq!(
        opcodes(child),
        [
            (FinalOpcode::GetVarRefCheck, Operands::VarRef(0)),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn compile_leaf_remains_an_explicit_nested_function_free_boundary() {
    let error = with_parsed_program(
        "function outer(){ return function(){ return 1; }; }",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let outer = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("outer"))
                .expect("outer function");
            context
                .compile_leaf(&outer, VerificationLimits::default())
                .expect_err("leaf API must keep rejecting nested constants")
        },
    )
    .expect("front-end acceptance");
    assert!(matches!(
        error,
        LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::NestedExecutable,
            ..
        }
    ));
}

#[test]
fn anonymous_functions_in_inferred_name_contexts_fail_closed() {
    for source in [
        "function outer(){ let inferred = function(){}; }",
        "function outer(){ let inferred; inferred = (function(){}); }",
    ] {
        let error = with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage planning must succeed");
                let outer = context
                    .executables()
                    .find(|executable| executable.metadata().name() == Some("outer"))
                    .expect("outer function");
                context
                    .compile_tree(&outer, VerificationLimits::default())
                    .expect_err("inferred names need exact name-setting bytecode")
            },
        )
        .expect("front-end acceptance");
        assert!(
            matches!(
                error,
                LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::InferredFunctionName,
                    ..
                }
            ),
            "{source}"
        );
    }
}

#[test]
fn owned_argument_and_local_cells_keep_distinct_parent_reference_indices() {
    let tree = compile_tree(
        "function outer(argument){ \
             let local=1; \
             return function(){ return argument+local; }; \
         }",
        "outer",
    );
    let outer = tree.root();
    assert_eq!(
        outer
            .control_flow()
            .compiler_capture_layout()
            .expect("two owned capture cells")
            .bindings(),
        [
            CompilerCapturedBinding::Argument(0),
            CompilerCapturedBinding::FunctionLocal(0),
        ]
    );
    let child = tree
        .function(function_executable(outer.constants()[0]))
        .expect("child function");
    assert_eq!(
        child
            .closure_variables()
            .iter()
            .map(|capture| capture.source())
            .collect::<Vec<_>>(),
        [
            CompiledClosureSource::ParentVariableReference(0),
            CompiledClosureSource::ParentVariableReference(1),
        ]
    );
}

#[test]
fn direct_child_constant_indices_cross_the_compact_boundary_exactly() {
    let mut source = String::from("function outer(){ return (");
    for index in 0..257 {
        if index != 0 {
            source.push(',');
        }
        source.push_str("function(){}");
    }
    source.push_str("); }");

    let tree = compile_tree(&source, "outer");
    let outer = tree.root();
    assert_eq!(outer.constants().len(), 257);
    assert_eq!(tree.functions().len(), 258);
    let closure_instructions = opcodes(outer)
        .into_iter()
        .filter(|(opcode, _)| matches!(opcode, FinalOpcode::FClosure | FinalOpcode::FClosure8))
        .collect::<Vec<_>>();
    assert_eq!(closure_instructions.len(), 257);
    assert_eq!(
        closure_instructions[0],
        (FinalOpcode::FClosure8, Operands::Const8(0))
    );
    assert_eq!(
        closure_instructions[255],
        (FinalOpcode::FClosure8, Operands::Const8(u8::MAX))
    );
    assert_eq!(
        closure_instructions[256],
        (FinalOpcode::FClosure, Operands::Const(256))
    );
    for &constant in outer.constants() {
        assert!(tree.function(function_executable(constant)).is_some());
    }
}

#[test]
fn deeply_nested_function_templates_compile_with_an_explicit_work_stack() {
    let depth = 256_usize;
    let mut source = String::from("function outer(){");
    for _ in 0..depth {
        source.push_str("return function(){");
    }
    source.push_str("return 0;");
    for _ in 0..depth {
        source.push('}');
    }
    source.push('}');

    let tree = with_parsed_program(
        &source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let outer = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("outer"))
                .expect("outer function");
            context
                .compile_tree_with_graph_limits(
                    &outer,
                    VerificationLimits::default(),
                    FunctionGraphVerificationLimits::default().with_max_nesting_depth(257),
                )
                .expect("explicit graph limit accepts the iterative fixture")
        },
    )
    .expect("front-end acceptance");
    assert_eq!(tree.functions().len(), depth + 1);
    assert!(
        tree.functions()
            .iter()
            .take(depth)
            .all(|function| function.constants().len() == 1)
    );
    assert!(
        tree.functions()
            .last()
            .is_some_and(|function| function.constants().is_empty())
    );
}

#[test]
fn default_graph_limit_rejects_function_depth_257() {
    let depth = 256_usize;
    let mut source = String::from("function outer(){");
    for _ in 0..depth {
        source.push_str("return function(){");
    }
    source.push_str("return 0;");
    for _ in 0..depth {
        source.push('}');
    }
    source.push('}');

    let error = with_parsed_program(
        &source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let outer = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("outer"))
                .expect("outer function");
            context
                .compile_tree(&outer, VerificationLimits::default())
                .expect_err("default graph depth is bounded")
        },
    )
    .expect("front-end acceptance");
    assert!(matches!(
        error,
        LeafCompilationError::FunctionGraphVerification { source, .. } if matches!(
            source.kind(),
            FunctionGraphVerificationErrorKind::LimitExceeded {
                resource: quickjs_bytecode::FunctionGraphResource::NestingDepth,
                limit: 256,
                observed: 257,
            }
        )
    ));
}

#[test]
fn selected_nested_root_with_imports_requires_an_explicit_environment() {
    let error = with_parsed_program(
        "function outer(argument){ \
             return function(){ return argument; }; \
         }",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let outer = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("outer"))
                .expect("outer function");
            let inner = context
                .executables()
                .find(|executable| executable.metadata().parent() == Some(outer.id()))
                .expect("anonymous inner function");
            context
                .compile_tree(&inner, VerificationLimits::default())
                .expect_err("standalone graph cannot import an omitted parent")
        },
    )
    .expect("front-end acceptance");
    assert!(matches!(
        error,
        LeafCompilationError::FunctionGraphVerification { source, .. }
            if source.kind()
                == &FunctionGraphVerificationErrorKind::RootRequiresEnvironment {
                    closure_variables: 1,
                }
    ));
}

#[test]
fn selected_closed_nested_root_has_a_standalone_graph_certificate() {
    let tree = with_parsed_program(
        "function outer(){ return function(){ return 1; }; }",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let outer = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("outer"))
                .expect("outer function");
            let inner = context
                .executables()
                .find(|executable| executable.metadata().parent() == Some(outer.id()))
                .expect("anonymous inner function");
            context
                .compile_tree(&inner, VerificationLimits::default())
                .expect("capture-free nested root is self-contained")
        },
    )
    .expect("front-end acceptance");

    assert_eq!(tree.functions().len(), 1);
    assert_eq!(tree.function_graph().root_id(), FunctionTemplateId::new(0));
    assert_eq!(
        tree.function_by_template(FunctionTemplateId::new(0)),
        Some(tree.root())
    );
    assert!(
        tree.function_by_template(FunctionTemplateId::new(1))
            .is_none()
    );
}

#[test]
fn function_body_declarations_are_instantiated_before_the_first_statement() {
    let tree = compile_tree(
        "function outer(){ \
             return declared; \
             function declared(){ return 1; } \
         }",
        "outer",
    );
    let outer = tree.root();
    assert_eq!(outer.constants().len(), 1);
    assert_eq!(
        opcodes(outer),
        [
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::GetLoc0, Operands::NoneLoc),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn duplicate_function_declarations_keep_all_children_but_instantiate_the_last() {
    let tree = compile_tree(
        "function outer(){ \
             return duplicate; \
             function duplicate(){ return 1; } \
             function duplicate(){ return 2; } \
         }",
        "outer",
    );
    let outer = tree.root();
    assert_eq!(outer.constants().len(), 2);
    let closure_operands = opcodes(outer)
        .into_iter()
        .filter_map(|(opcode, operands)| {
            matches!(opcode, FinalOpcode::FClosure | FinalOpcode::FClosure8).then_some(operands)
        })
        .collect::<Vec<_>>();
    assert_eq!(closure_operands, [Operands::Const8(1)]);
    for &constant in outer.constants() {
        assert!(tree.function(function_executable(constant)).is_some());
    }
}

#[test]
fn function_declaration_redeclaration_initializes_the_argument_slot() {
    let tree = compile_tree(
        "function outer(replaced){ \
             return replaced; \
             function replaced(){ return 1; } \
         }",
        "outer",
    );
    assert_eq!(
        opcodes(tree.root()),
        [
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::PutArg0, Operands::NoneArg),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn strict_block_function_declarations_reinstantiate_on_loop_scope_entry() {
    let tree = compile_tree(
        "function outer(){ \
             \"use strict\"; \
             for (let iteration=0; iteration<1; iteration++) { \
                 current; \
                 function current(){ return iteration; } \
             } \
         }",
        "outer",
    );
    let outer = tree.root();
    let child = tree
        .function(function_executable(outer.constants()[0]))
        .expect("block declaration child");
    assert_eq!(
        child.closure_variables()[0].source(),
        CompiledClosureSource::ParentVariableReference(0)
    );
    let instructions = opcodes(outer);
    let closure_index = instructions
        .iter()
        .position(|instruction| *instruction == (FinalOpcode::FClosure8, Operands::Const8(0)))
        .expect("scope entry creates the block function");
    let read_index = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                (
                    FinalOpcode::GetLoc0
                        | FinalOpcode::GetLoc1
                        | FinalOpcode::GetLoc2
                        | FinalOpcode::GetLoc3,
                    Operands::NoneLoc,
                ) | (FinalOpcode::GetLoc, Operands::Loc(_))
            )
        })
        .expect("body reads the initialized declaration");
    assert!(closure_index < read_index);
    assert!(
        instructions
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::CloseLoc)
    );
}
