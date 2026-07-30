use quickjs_bytecode::{
    CompilerBindingKind, CompilerClosureBinding, CompilerClosureSource, CompilerExecutableKind,
    CompilerInitializationPolicy, FinalOpcode, FunctionTemplateId, Operands, VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledFunction, CompiledFunctionTree, CompiledRealmGlobalSource,
    LeafCompilationError, UnsupportedLeafFeature,
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
fn unresolved_names_lower_through_constructor_realm_global_slots() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("realmWrite = 1; return realmRead;"),
    )
    .expect("constructor-realm global references");
    let root = tree.root();
    let wrapper = &tree.functions()[1];
    let wrapper_opcodes = opcodes(wrapper);

    assert!(root.closure_variables().is_empty());
    assert!(wrapper.closure_variables().is_empty());
    assert_eq!(root.realm_globals().len(), 2);
    assert_eq!(wrapper.realm_globals().len(), 2);
    assert!(
        root.realm_globals()
            .iter()
            .all(|global| { global.source() == CompiledRealmGlobalSource::ConstructorRealm })
    );
    assert_eq!(
        wrapper
            .realm_globals()
            .iter()
            .map(quickjs_compiler::CompiledRealmGlobal::source)
            .collect::<Vec<_>>(),
        [
            CompiledRealmGlobalSource::ParentClosure(0),
            CompiledRealmGlobalSource::ParentClosure(1),
        ]
    );
    assert!(
        tree.verified_bytecode()
            .root()
            .metadata()
            .closures()
            .iter()
            .all(|closure| matches!(closure.binding(), CompilerClosureBinding::RealmGlobal(_)))
    );
    assert!(
        tree.verified_bytecode()
            .root()
            .metadata()
            .closures()
            .iter()
            .all(|closure| matches!(
                closure.source(),
                CompilerClosureSource::ConstructorRealmGlobal(_)
            ))
    );
    assert!(
        wrapper_opcodes
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::PutVar)
    );
    assert!(
        wrapper_opcodes
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::GetVar)
    );
    assert!(tree.functions().iter().all(|function| {
        opcodes(function)
            .iter()
            .all(|(opcode, _)| !matches!(opcode, FinalOpcode::Eval | FinalOpcode::ApplyEval))
    }));
}

#[test]
fn typeof_an_absent_constructor_realm_name_uses_get_var_undef() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("return typeof (((absentRealmName)));"),
    )
    .expect("typeof unresolved realm global");
    let wrapper = &tree.functions()[1];

    assert!(
        opcodes(wrapper).windows(2).any(|pair| {
            pair[0].0 == FinalOpcode::GetVarUndef && pair[1].0 == FinalOpcode::Typeof
        })
    );
}

#[test]
fn unresolved_global_mutation_forms_remain_whole_function_verified() {
    for body in [
        "realmValue = 1; return realmValue;",
        "realmValue += 1; return realmValue;",
        "realmValue ||= 1; return realmValue;",
        "return realmValue++;",
        "return ++realmValue;",
    ] {
        let tree = compile_dynamic_function(&[], SourceFragment::new(body))
            .expect("verified constructor-realm global mutation");
        let wrapper = &tree.functions()[1];

        assert!(
            opcodes(wrapper)
                .iter()
                .any(|(opcode, _)| *opcode == FinalOpcode::PutVar)
        );
    }
}

#[test]
fn nested_functions_forward_constructor_realm_globals_without_frame_capture() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("return function nested(){ return realmValue; };"),
    )
    .expect("forwarded constructor-realm global");

    assert_eq!(tree.functions().len(), 3);
    assert_eq!(tree.root().realm_globals().len(), 1);
    assert_eq!(tree.functions()[1].realm_globals().len(), 1);
    assert_eq!(tree.functions()[2].realm_globals().len(), 1);
    assert!(
        opcodes(&tree.functions()[2])
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::GetVar)
    );
}

#[test]
fn realm_globals_follow_frame_captures_in_the_shared_closure_slot_domain() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new(
            "let captured = 1; return function nested(){ return captured + realmValue; };",
        ),
    )
    .expect("mixed frame capture and constructor-realm global");
    let wrapper = &tree.functions()[1];
    let nested = &tree.functions()[2];

    assert!(wrapper.closure_variables().is_empty());
    assert_eq!(wrapper.realm_globals()[0].slot(), 0);
    assert_eq!(nested.closure_variables().len(), 1);
    assert_eq!(nested.realm_globals()[0].slot(), 1);
    assert_eq!(
        nested.realm_globals()[0].source(),
        CompiledRealmGlobalSource::ParentClosure(0)
    );
    assert!(opcodes(nested).iter().any(|(opcode, operands)| {
        *opcode == FinalOpcode::GetVar && *operands == Operands::VarRef(1)
    }));
}

#[test]
fn sloppy_dynamic_function_this_is_compiled_as_a_receiver_read() {
    let tree = compile_dynamic_function(&[], SourceFragment::new("return this;"))
        .expect("sloppy dynamic Function this");

    assert!(
        opcodes(&tree.functions()[1])
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::PushThis)
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
fn escaped_program_global_declarations_remain_fail_closed() {
    let global = compile_dynamic_function(&[], SourceFragment::new("}); var escaped; ({"))
        .expect_err("Program global declaration needs a realm environment");
    assert!(matches!(
        global,
        LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::GlobalEnvironment,
            ..
        }
    ));
}
