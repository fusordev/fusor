use quickjs_bytecode::{
    CompilerBindingKind, CompilerExecutableKind, CompilerInitializationPolicy, FinalOpcode,
    FunctionTemplateId, Operands, VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledFunction, CompiledFunctionTree, LeafCompilationError,
    UnsupportedLeafFeature,
};
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};

fn compile_dynamic_function(
    parameters: &[SourceFragment<'_>],
    body: SourceFragment<'_>,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    let source = DynamicFunctionSource::new(DynamicFunctionKind::Function, parameters, body);
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context.compile_dynamic_function_script(VerificationLimits::default())
    })
    .expect("dynamic frontend")
}

fn opcodes(function: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
    function
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let decoded = instruction.decoded().instruction();
            (decoded.opcode(), decoded.operands())
        })
        .collect()
}

#[test]
fn compiles_the_complete_ordinary_dynamic_function_wrapper() {
    let parameters = [SourceFragment::new("left"), SourceFragment::new("right")];
    let tree = compile_dynamic_function(&parameters, SourceFragment::new("return left + right;"))
        .expect("complete dynamic Function Script");

    assert_eq!(tree.root_executable().index(), 0);
    assert_eq!(tree.functions().len(), 2);
    assert_eq!(
        tree.verified_bytecode().root().metadata().executable_kind(),
        CompilerExecutableKind::DynamicFunctionScript
    );
    let root_header = tree.root().control_flow().function_header();
    assert_eq!(root_header.defined_argument_count(), 0);
    assert!(!root_header.flags().is_eval());
    assert!(tree.functions().iter().all(|function| {
        opcodes(function)
            .iter()
            .all(|(opcode, _)| !matches!(opcode, FinalOpcode::Eval | FinalOpcode::ApplyEval))
    }));
    assert_eq!(
        opcodes(tree.root()),
        [
            (FinalOpcode::FClosure8, Operands::Const8(0)),
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::GetLoc0, Operands::NoneLoc),
            (FinalOpcode::Return, Operands::None),
        ]
    );

    let wrapper = &tree.functions()[1];
    assert_eq!(
        wrapper
            .control_flow()
            .function_header()
            .defined_argument_count(),
        2
    );
    let self_binding = tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(1))
        .expect("verified wrapper")
        .metadata()
        .variables()
        .iter()
        .find(|definition| definition.policy().kind() == CompilerBindingKind::FunctionName)
        .expect("named wrapper self binding");
    assert_eq!(
        self_binding.policy().initialization(),
        CompilerInitializationPolicy::FunctionName
    );
}

#[test]
fn wrapper_escape_returns_the_complete_script_object_completion() {
    let tree = compile_dynamic_function(&[], SourceFragment::new("}), ({"))
        .expect("QuickJS-compatible wrapper escape");
    let root = opcodes(tree.root());

    assert_eq!(tree.functions().len(), 2);
    assert!(root.contains(&(FinalOpcode::Object, Operands::None)));
    assert_eq!(
        &root[root.len() - 3..],
        [
            (FinalOpcode::PutLoc0, Operands::NoneLoc),
            (FinalOpcode::GetLoc0, Operands::NoneLoc),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn wrapper_escape_can_read_the_dynamic_script_global_receiver() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}), (function(){}) ? this : (function(){"),
    )
    .expect("escaped Script this");

    assert!(
        opcodes(tree.root())
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::PushThis)
    );
}

#[test]
fn control_statement_resets_prior_script_completion_to_undefined() {
    let tree = compile_dynamic_function(&[], SourceFragment::new("}), 1; if (false) ({"))
        .expect("escaped wrapper with a control statement");
    let root = opcodes(tree.root());

    let reset = root
        .windows(2)
        .position(|pair| {
            pair == [
                (FinalOpcode::Undefined, Operands::None),
                (FinalOpcode::PutLoc0, Operands::NoneLoc),
            ]
        })
        .expect("if resets the Script completion");
    let final_read = root
        .iter()
        .rposition(|entry| *entry == (FinalOpcode::GetLoc0, Operands::NoneLoc))
        .expect("Script completion read");
    assert!(reset < final_read);
}

#[test]
fn do_while_resets_script_completion_on_every_iteration() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); do { if (false) 1; } while (true); ({"),
    )
    .expect("escaped wrapper with a do-while statement");
    let flow = tree.root().control_flow();
    let back_edge = flow
        .instructions()
        .iter()
        .find(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::IfTrue | FinalOpcode::IfTrue8
            )
        })
        .expect("do-while back edge");
    let reset_target = back_edge
        .successors()
        .branch_target()
        .expect("verified do-while iteration target");
    let reset = flow
        .instruction(reset_target)
        .expect("verified reset instruction");

    assert_eq!(
        reset.decoded().instruction().opcode(),
        FinalOpcode::Undefined,
        "the back edge must re-enter before the completion reset"
    );
    let reset_index = reset_target.get() as usize;
    assert_eq!(
        flow.instructions()[reset_index + 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::PutLoc0
    );
}

#[test]
fn named_wrapper_self_reference_is_compiled() {
    let tree = compile_dynamic_function(&[], SourceFragment::new("return anonymous;"))
        .expect("named Function expression");
    let wrapper = &tree.functions()[1];

    assert_eq!(
        wrapper
            .control_flow()
            .instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>(),
        [FinalOpcode::GetLoc0, FinalOpcode::Return]
    );
}

#[test]
fn strict_named_wrapper_retains_its_immutable_self_binding() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("\"use strict\"; return anonymous;"),
    )
    .expect("strict named Function expression");
    let wrapper = tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(1))
        .expect("verified wrapper");
    let self_binding = wrapper
        .metadata()
        .variables()
        .iter()
        .find(|definition| definition.policy().kind() == CompilerBindingKind::FunctionName)
        .expect("strict wrapper self binding");

    assert_eq!(
        self_binding.policy().writes(),
        quickjs_bytecode::CompilerWritePolicy::Immutable
    );
}

#[test]
fn ordinary_tree_entry_cannot_extract_the_wrapper_child() {
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &[],
        SourceFragment::new("return 1;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        let wrapper = context.executables().nth(1).expect("wrapper child");
        let error = context
            .compile_tree(&wrapper, VerificationLimits::default())
            .expect_err("dynamic child extraction must fail closed");

        assert!(matches!(
            error,
            LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                ..
            }
        ));

        let error = context
            .compile_leaf(&wrapper, VerificationLimits::default())
            .expect_err("dynamic leaf extraction must fail closed");
        assert!(matches!(
            error,
            LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                ..
            }
        ));
    })
    .expect("dynamic frontend");
}

#[test]
fn program_globals_and_unresolved_references_remain_fail_closed() {
    let global = compile_dynamic_function(&[], SourceFragment::new("}); var escaped; ({"))
        .expect_err("Program global declaration needs a realm environment");
    assert!(matches!(
        global,
        LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::GlobalEnvironment,
            ..
        }
    ));

    let root_reference = compile_dynamic_function(&[], SourceFragment::new("}), missing, ({"))
        .expect_err("Program global lookup needs a realm environment");
    assert!(matches!(
        root_reference,
        LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::UnresolvedReference,
            ..
        }
    ));

    let child_reference = compile_dynamic_function(&[], SourceFragment::new("return missing;"))
        .expect_err("wrapper global lookup needs a realm environment");
    assert!(matches!(
        child_reference,
        LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::UnresolvedReference,
            ..
        }
    ));
}
