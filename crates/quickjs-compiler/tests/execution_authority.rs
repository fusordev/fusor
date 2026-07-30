use std::sync::Arc;

use quickjs_bytecode::{
    CompilerBindingKind, CompilerCapturedBinding, CompilerClosureSource, CompilerConstant,
    CompilerInitializationPolicy, CompilerWritePolicy, ExecutionRequirement, FinalOpcode,
    FunctionTemplateId, Operands, ScopeLink, VerificationLimits,
};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_tree(source: &str, root_name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("metadata.js"))
                .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect("verified function tree")
        },
    )
    .expect("frontend")
}

fn atom_text(function: quickjs_bytecode::VerifiedBytecodeFunction<'_>, index: u32) -> String {
    char::decode_utf16(
        function.function().atoms()[index as usize]
            .string()
            .code_units(),
    )
    .map(|character| character.expect("identifier atom is scalar"))
    .collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn compiler_freezes_complete_metadata_before_the_oxc_arena_drops() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledFunctionTree>();

    let source = "function outer(a){\
        var v;\
        let x=1;\
        const y=2;\
        function inner(){return a+x+y;}\
        return inner;\
    }";
    let tree = compile_tree(source, "outer");
    let verified = tree.verified_bytecode();
    assert_eq!(verified.root_id(), FunctionTemplateId::new(0));
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::Closures)
    );
    assert!(
        verified
            .requirements()
            .contains(&ExecutionRequirement::LexicalBindings)
    );

    let root = verified
        .function(FunctionTemplateId::new(0))
        .expect("root metadata");
    assert_eq!(root.metadata().source().display_name(), "metadata.js");
    assert_eq!(root.metadata().source().function_source(), source);
    assert_eq!(
        atom_text(root, root.metadata().function_name().expect("name").get()),
        "outer"
    );
    assert_eq!(root.metadata().variables().len(), 5);
    assert_eq!(
        root.metadata()
            .variables()
            .iter()
            .map(|definition| atom_text(root, definition.name().expect("vardef name").get()))
            .collect::<Vec<_>>(),
        ["a", "v", "x", "y", "inner"]
    );

    let argument = &root.metadata().variables()[0];
    assert_eq!(argument.policy().kind(), CompilerBindingKind::Parameter);
    assert_eq!(
        argument.policy().initialization(),
        CompilerInitializationPolicy::Argument
    );
    assert_eq!(argument.policy().writes(), CompilerWritePolicy::Mutable);
    assert_eq!(argument.variable_reference(), Some(0));
    assert_eq!(argument.scope_next(), ScopeLink::End);

    let lexical_x = &root.metadata().variables()[2];
    assert_eq!(lexical_x.policy().kind(), CompilerBindingKind::Let);
    assert!(lexical_x.policy().has_temporal_dead_zone());
    assert!(lexical_x.has_scope());
    assert_eq!(lexical_x.variable_reference(), Some(1));
    assert_eq!(lexical_x.scope_next(), ScopeLink::End);
    let lexical_y = &root.metadata().variables()[3];
    assert_eq!(lexical_y.policy().kind(), CompilerBindingKind::Const);
    assert_eq!(lexical_y.policy().writes(), CompilerWritePolicy::Immutable);
    assert_eq!(lexical_y.variable_reference(), Some(2));
    assert_eq!(lexical_y.scope_next(), ScopeLink::Local(1));
    assert_eq!(
        root.function()
            .control_flow()
            .compiler_capture_layout()
            .expect("own captures")
            .bindings(),
        [
            CompilerCapturedBinding::Argument(0),
            CompilerCapturedBinding::ScopedLocal(1),
            CompilerCapturedBinding::ScopedLocal(2),
        ]
    );

    let child = verified
        .function(FunctionTemplateId::new(1))
        .expect("child metadata");
    assert_eq!(
        child
            .metadata()
            .closures()
            .iter()
            .map(|closure| (
                atom_text(child, closure.name().expect("closure name").get()),
                closure.source(),
                closure.policy().kind(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                "a".to_owned(),
                CompilerClosureSource::ParentVariableReference(0),
                CompilerBindingKind::Parameter,
            ),
            (
                "x".to_owned(),
                CompilerClosureSource::ParentVariableReference(1),
                CompilerBindingKind::Let,
            ),
            (
                "y".to_owned(),
                CompilerClosureSource::ParentVariableReference(2),
                CompilerBindingKind::Const,
            ),
        ]
    );
    assert_eq!(
        child.metadata().source().function_source(),
        "function inner(){return a+x+y;}"
    );
    assert_eq!(
        child.metadata().source().mappings().len(),
        child.function().control_flow().instructions().len()
    );
}

#[test]
fn metadata_names_append_after_operand_atoms_without_shifting_bytecode_indices() {
    let tree = compile_tree(
        "function f(argument){let local=\"runtime\";return local;}",
        "f",
    );
    let verified = tree.verified_bytecode();
    let function = verified
        .function(FunctionTemplateId::new(0))
        .expect("function");
    let instructions = function.function().control_flow().instructions();
    let pushed = instructions
        .iter()
        .find_map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode() == FinalOpcode::PushAtomValue).then_some(instruction.operands())
        })
        .expect("runtime string atom");
    assert_eq!(
        pushed,
        Operands::Atom(quickjs_bytecode::AtomPoolIndex::new(0))
    );
    assert_eq!(atom_text(function, 0), "runtime");
    assert_eq!(
        atom_text(
            function,
            function.metadata().function_name().expect("name").get()
        ),
        "f"
    );
}

#[test]
fn execution_requirements_are_sorted_deduplicated_and_conservative() {
    let tree = compile_tree(
        "function f(a){\
            let x=1n;\
            function g(){return x;}\
            return (1.5,typeof a,a in a,a+x,g);\
        }",
        "f",
    );

    assert_eq!(
        tree.verified_bytecode().requirements(),
        [
            ExecutionRequirement::CoreValues,
            ExecutionRequirement::Numbers,
            ExecutionRequirement::Strings,
            ExecutionRequirement::BigInts,
            ExecutionRequirement::Closures,
            ExecutionRequirement::LexicalBindings,
            ExecutionRequirement::ObjectOperators,
            ExecutionRequirement::DynamicOperators,
        ]
    );
}

#[test]
fn strict_block_function_initializers_freeze_exact_children_and_preactivate_captures() {
    let source = "function outer(){\"use strict\";let out;{function inner(){return 1}function leaf(){return inner}out=leaf;}return out;}";
    let tree = compile_tree(source, "outer");
    let verified = tree.verified_bytecode();
    let root = verified
        .function(FunctionTemplateId::new(0))
        .expect("root function");
    let variables = root.metadata().variables();
    let named_definition = |name: &str| {
        variables
            .iter()
            .enumerate()
            .find(|(_, definition)| {
                atom_text(root, definition.name().expect("binding name").get()) == name
            })
            .expect("named definition")
    };
    let (inner_definition_index, inner) = named_definition("inner");
    let (_, leaf) = named_definition("leaf");
    assert_eq!(
        inner.policy().initialization(),
        CompilerInitializationPolicy::FunctionAtScopeEntry
    );
    assert_eq!(
        leaf.policy().initialization(),
        CompilerInitializationPolicy::FunctionAtScopeEntry
    );
    assert_eq!(inner.variable_reference(), Some(0));
    let inner_constant = inner.function_initializer().expect("inner child constant");
    let leaf_constant = leaf.function_initializer().expect("leaf child constant");
    let CompilerConstant::Function(inner_id) =
        &root.function().constants()[inner_constant as usize]
    else {
        panic!("inner initializer must name a child template");
    };
    let CompilerConstant::Function(leaf_id) = &root.function().constants()[leaf_constant as usize]
    else {
        panic!("leaf initializer must name a child template");
    };
    assert_eq!(
        atom_text(
            verified.function(*inner_id).expect("inner child"),
            verified
                .function(*inner_id)
                .expect("inner child")
                .metadata()
                .function_name()
                .expect("inner name")
                .get(),
        ),
        "inner"
    );
    let leaf_function = verified.function(*leaf_id).expect("leaf child");
    assert_eq!(
        leaf_function.metadata().closures()[0].source(),
        CompilerClosureSource::ParentVariableReference(0)
    );

    let local = u16::try_from(inner_definition_index).expect("local index");
    let instructions = root.function().control_flow().instructions();
    let activation = instructions
        .iter()
        .position(|verified| {
            let instruction = verified.decoded().instruction();
            instruction.opcode() == FinalOpcode::SetLocUninitialized
                && instruction.operands() == Operands::Loc(local)
        })
        .expect("captured block function activation");
    let first_declaration_closure = instructions
        .iter()
        .position(|verified| {
            matches!(
                verified.decoded().instruction().opcode(),
                FinalOpcode::FClosure | FinalOpcode::FClosure8
            )
        })
        .expect("declaration closure");
    assert!(activation < first_declaration_closure);
    assert!(instructions.iter().any(|verified| {
        let instruction = verified.decoded().instruction();
        instruction.opcode() == FinalOpcode::CloseLoc
            && instruction.operands() == Operands::Loc(local)
    }));
}

#[test]
fn mutually_capturing_block_functions_and_mixed_hoists_share_atomic_preludes() {
    let tree = compile_tree(
        "function outer(replaced){\
            \"use strict\";\
            function local(){return replaced}\
            function replaced(){return local}\
            {function left(){return right}function right(){return left}return left}\
        }",
        "outer",
    );
    let root = tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(0))
        .expect("root function");
    let initializer_count = root
        .metadata()
        .variables()
        .iter()
        .filter(|definition| definition.function_initializer().is_some())
        .count();
    assert_eq!(initializer_count, 4);
    assert!(root.metadata().variables().iter().any(|definition| {
        definition.policy().kind() == CompilerBindingKind::Parameter
            && definition.function_initializer().is_some()
    }));
}
