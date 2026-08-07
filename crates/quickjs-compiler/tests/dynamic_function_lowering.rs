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
    compile_dynamic_function_kind(DynamicFunctionKind::Function, parameters, body)
}

fn compile_dynamic_function_kind(
    kind: DynamicFunctionKind,
    parameters: &[SourceFragment<'_>],
    body: SourceFragment<'_>,
) -> Result<CompiledFunctionTree, LeafCompilationError> {
    let source = DynamicFunctionSource::new(kind, parameters, body);
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context.compile_dynamic_function_script(VerificationLimits::default())
    })
    .expect("dynamic frontend")
}

#[test]
fn compiles_the_complete_dynamic_generator_function_wrapper() {
    let parameters = [SourceFragment::new("value")];
    let tree = compile_dynamic_function_kind(
        DynamicFunctionKind::GeneratorFunction,
        &parameters,
        SourceFragment::new("yield value; return 9;"),
    )
    .expect("complete dynamic GeneratorFunction Script");

    assert_eq!(tree.root_executable().index(), 0);
    assert_eq!(tree.functions().len(), 2);
    assert_eq!(
        tree.verified_bytecode().root().metadata().executable_kind(),
        CompilerExecutableKind::DynamicFunctionScript
    );
    let generator = tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(1))
        .expect("verified generator wrapper");
    assert_eq!(
        generator.metadata().executable_kind(),
        CompilerExecutableKind::GeneratorFunction
    );
    let generator_opcodes = opcodes(&tree.functions()[1]);
    assert_eq!(generator_opcodes[0].0, FinalOpcode::InitialYield);
    assert!(
        generator_opcodes
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::Yield)
    );
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
fn program_var_without_initializer_is_a_declared_realm_global() {
    let tree = compile_dynamic_function(&[], SourceFragment::new("}); var realmVar; ({"))
        .expect("Program var declaration");
    let root = tree.root();
    let definitions = tree.verified_bytecode().root().metadata().closures();

    assert_eq!(root.realm_globals().len(), 1);
    assert_eq!(root.realm_globals()[0].name(), "realmVar");
    assert_eq!(
        root.realm_globals()[0].source(),
        CompiledRealmGlobalSource::ConstructorRealm
    );
    assert_eq!(definitions.len(), 1);
    assert!(matches!(
        definitions[0].binding(),
        CompilerClosureBinding::RealmGlobal(policy)
            if policy.kind() == CompilerBindingKind::Var
                && policy.initialization()
                    == CompilerInitializationPolicy::UndefinedAtInstantiation
                && !policy.has_temporal_dead_zone()
    ));
    assert!(
        opcodes(root)
            .iter()
            .all(|(opcode, _)| *opcode != FinalOpcode::PutVar),
        "declaration instantiation creates the property; an absent initializer emits no write"
    );
}

#[test]
fn program_var_initializer_writes_the_declared_realm_global() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); var realmVar = 1; realmVar; ({"),
    )
    .expect("initialized Program var declaration");
    let root = tree.root();

    assert_eq!(root.realm_globals().len(), 1);
    assert!(opcodes(root).iter().any(|(opcode, operands)| {
        *opcode == FinalOpcode::PutVar
            && *operands == Operands::VarRef(root.realm_globals()[0].slot())
    }));
    assert!(opcodes(root).iter().any(|(opcode, operands)| {
        *opcode == FinalOpcode::GetVar
            && *operands == Operands::VarRef(root.realm_globals()[0].slot())
    }));
}

#[test]
fn declared_var_and_unresolved_name_share_the_realm_global_slot_domain() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); var declared = 1; unresolved = 2; declared + unresolved; ({"),
    )
    .expect("mixed declared and unresolved realm globals");
    let root = tree.root();

    assert_eq!(
        root.realm_globals()
            .iter()
            .map(|global| (global.name(), global.slot(), global.policy().kind()))
            .collect::<Vec<_>>(),
        [
            ("declared", 0, CompilerBindingKind::Var),
            ("unresolved", 1, CompilerBindingKind::GlobalReference),
        ]
    );
    assert_eq!(
        tree.verified_bytecode()
            .root()
            .metadata()
            .closures()
            .iter()
            .map(|definition| definition.binding().policy().kind())
            .collect::<Vec<_>>(),
        [
            CompilerBindingKind::Var,
            CompilerBindingKind::GlobalReference,
        ]
    );
}

#[test]
fn program_var_redeclarations_share_one_realm_global() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); var realmVar; var realmVar = 1; var realmVar; ({"),
    )
    .expect("Program var redeclarations");
    let root = tree.root();

    assert_eq!(root.realm_globals().len(), 1);
    assert_eq!(
        opcodes(root)
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::PutVar)
            .count(),
        1,
        "only the one source initializer writes the shared property"
    );
}

#[test]
fn child_reference_to_program_var_forwards_the_declared_realm_global() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); var shared = 1; (function read(){ return shared; }); ({"),
    )
    .expect("child reference to Program var");
    let root = tree.root();
    let wrapper = &tree.functions()[1];
    let child = &tree.functions()[2];

    assert_eq!(tree.functions().len(), 3);
    assert_eq!(root.realm_globals().len(), 1);
    assert!(wrapper.realm_globals().is_empty());
    assert_eq!(child.realm_globals().len(), 1);
    assert_eq!(child.realm_globals()[0].id(), root.realm_globals()[0].id());
    assert_eq!(
        child.realm_globals()[0].source(),
        CompiledRealmGlobalSource::ParentClosure(root.realm_globals()[0].slot())
    );
    assert!(root.closure_variables().is_empty());
    assert!(child.closure_variables().is_empty());
    assert!(matches!(
        tree.verified_bytecode().root().metadata().closures()[0].binding(),
        CompilerClosureBinding::RealmGlobal(policy)
            if policy.kind() == CompilerBindingKind::Var
    ));
    assert!(opcodes(child).iter().any(|(opcode, operands)| {
        *opcode == FinalOpcode::GetVar
            && *operands == Operands::VarRef(child.realm_globals()[0].slot())
    }));
}

#[test]
fn program_lexicals_are_evaluation_local_and_keep_tdz_metadata() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); let lexical = 1; const fixed = 2; lexical; fixed; ({"),
    )
    .expect("Program lexical declarations");
    let root = tree.root();
    let metadata = tree.verified_bytecode().root().metadata();
    let lexical_policies = metadata
        .variables()
        .iter()
        .map(quickjs_bytecode::VariableDefinition::policy)
        .filter(|policy| {
            matches!(
                policy.kind(),
                CompilerBindingKind::Let | CompilerBindingKind::Const
            )
        })
        .collect::<Vec<_>>();
    let root_opcodes = opcodes(root);

    assert!(root.realm_globals().is_empty());
    assert_eq!(lexical_policies.len(), 2);
    assert!(lexical_policies.iter().all(|policy| {
        policy.initialization() == CompilerInitializationPolicy::AtDeclaration
            && policy.has_temporal_dead_zone()
    }));
    assert_eq!(
        root_opcodes
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::SetLocUninitialized)
            .count(),
        2
    );
    assert_eq!(
        root_opcodes
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::GetLocCheck)
            .count(),
        2
    );
}

#[test]
fn child_reference_to_program_lexical_captures_the_evaluation_cell() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new(
            "}); let sharedLet = 1; const sharedConst = 2; \
             (function read(){ return sharedLet + sharedConst; }); ({",
        ),
    )
    .expect("child capture of Program lexical");
    let root = tree.root();
    let child = &tree.functions()[2];

    assert!(
        tree.functions()
            .iter()
            .all(|function| function.realm_globals().is_empty())
    );
    assert_eq!(root.locals().len(), 2);
    assert_eq!(child.closure_variables().len(), 2);
    let child_metadata = tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(2))
        .expect("verified child")
        .metadata();
    assert_eq!(
        child_metadata
            .closures()
            .iter()
            .map(|definition| definition.binding().policy().kind())
            .collect::<Vec<_>>(),
        [CompilerBindingKind::Let, CompilerBindingKind::Const]
    );
    assert!(
        child_metadata
            .closures()
            .iter()
            .all(|definition| definition.binding().policy().has_temporal_dead_zone())
    );
    assert_eq!(
        opcodes(child)
            .iter()
            .filter(|(opcode, _)| *opcode == FinalOpcode::GetVarRefCheck)
            .count(),
        2
    );
}

#[test]
fn program_function_declaration_is_hoisted_before_user_statements() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); declared; function declared(){ return declared; } ({"),
    )
    .expect("hoisted Program function declaration");
    let root = tree.root();
    let root_opcodes = opcodes(root);
    let definition = &tree.verified_bytecode().root().metadata().closures()[0];
    let initializer = definition
        .function_initializer()
        .expect("root function initializer constant");

    assert_eq!(root.realm_globals().len(), 1);
    assert!(matches!(
        definition.binding(),
        CompilerClosureBinding::RealmGlobal(policy)
            if policy.kind() == CompilerBindingKind::Function
                && policy.initialization()
                    == CompilerInitializationPolicy::FunctionAtInstantiation
    ));
    assert!(matches!(
        root_opcodes[0],
        (FinalOpcode::FClosure8, Operands::Const8(index))
            if u32::from(index) == initializer
    ));
    assert_eq!(
        root_opcodes[1],
        (
            FinalOpcode::PutVar,
            Operands::VarRef(root.realm_globals()[0].slot())
        )
    );
    assert!(
        root_opcodes[2..]
            .iter()
            .any(|(opcode, _)| *opcode == FinalOpcode::GetVar),
        "the source read executes after declaration instantiation"
    );
    let child_definition = &tree
        .verified_bytecode()
        .function(FunctionTemplateId::new(2))
        .expect("verified declared child")
        .metadata()
        .closures()[0];
    assert!(matches!(
        child_definition.binding(),
        CompilerClosureBinding::RealmGlobal(policy)
            if policy.kind() == CompilerBindingKind::Function
    ));
    assert_eq!(
        child_definition.function_initializer(),
        None,
        "descendants forward the realm binding without reinitializing it"
    );
}

#[test]
fn duplicate_program_functions_initialize_from_the_last_declaration() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new(
            "}); function declared(){ return 1; } \
             function declared(){ return 2; } ({",
        ),
    )
    .expect("duplicate Program function declarations");
    let root = tree.root();
    let definition = &tree.verified_bytecode().root().metadata().closures()[0];
    let initializer = definition
        .function_initializer()
        .expect("last declaration initializer") as usize;
    let initialized_child = root.constants()[initializer]
        .function()
        .expect("initializer is a function template")
        .executable();

    assert_eq!(tree.functions().len(), 4);
    assert_eq!(initialized_child, tree.functions()[3].executable());
    assert_ne!(initialized_child, tree.functions()[2].executable());
    assert_eq!(
        opcodes(root)
            .iter()
            .filter(|(opcode, _)| {
                matches!(opcode, FinalOpcode::FClosure | FinalOpcode::FClosure8)
            })
            .count(),
        2,
        "one hoisted initializer plus the synthetic wrapper expression execute"
    );
}

#[test]
fn program_function_initializers_form_an_absolute_prefix_before_lexical_setup() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new(
            "}); let captured = 1; \
             function first(){ return captured; } \
             function second(){ return captured; } ({",
        ),
    )
    .expect("multiple Program function declarations");
    let root = tree.root();
    let root_opcodes = opcodes(root);
    let definitions = tree.verified_bytecode().root().metadata().closures();

    assert_eq!(root.realm_globals().len(), 2);
    for (index, global) in root.realm_globals().iter().enumerate() {
        let initializer = definitions[index]
            .function_initializer()
            .expect("root function initializer");
        assert!(matches!(
            root_opcodes[index * 2],
            (FinalOpcode::FClosure8, Operands::Const8(constant))
                if u32::from(constant) == initializer
        ));
        assert_eq!(
            root_opcodes[index * 2 + 1],
            (FinalOpcode::PutVar, Operands::VarRef(global.slot()))
        );
    }
    assert_eq!(
        root_opcodes[4],
        (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        "all certified function pairs precede Program lexical TDZ setup"
    );
}

#[test]
fn hoisted_program_function_can_capture_a_program_lexical_cell() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); let captured = 1; function declared(){ return captured; } ({"),
    )
    .expect("Program function captures Program lexical");
    let root = tree.root();
    let child = &tree.functions()[2];
    let root_opcodes = opcodes(root);

    assert_eq!(
        root_opcodes[..3],
        [
            (FinalOpcode::FClosure8, Operands::Const8(1)),
            (
                FinalOpcode::PutVar,
                Operands::VarRef(root.realm_globals()[0].slot())
            ),
            (FinalOpcode::SetLocUninitialized, Operands::Loc(0)),
        ],
        "the global function captures the cell before lexical TDZ setup updates it"
    );
    assert_eq!(child.closure_variables().len(), 1);
    assert!(matches!(
        tree.verified_bytecode()
            .function(FunctionTemplateId::new(2))
            .expect("verified declared child")
            .metadata()
            .closures()[0]
            .binding(),
        CompilerClosureBinding::Captured(policy)
            if policy.kind() == CompilerBindingKind::Let
                && policy.has_temporal_dead_zone()
    ));
}

#[test]
fn var_initializer_runs_after_merged_function_instantiation() {
    let tree = compile_dynamic_function(
        &[],
        SourceFragment::new("}); var declared = 7; function declared(){ return 1; } declared; ({"),
    )
    .expect("merged var and function declaration");
    let root = tree.root();
    let root_opcodes = opcodes(root);
    let slot = root.realm_globals()[0].slot();
    let writes = root_opcodes
        .iter()
        .enumerate()
        .filter_map(|(index, (opcode, operands))| {
            (*opcode == FinalOpcode::PutVar && *operands == Operands::VarRef(slot)).then_some(index)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        root.realm_globals()[0].policy().kind(),
        CompilerBindingKind::Function
    );
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0], 1, "function instantiation is the root prologue");
    assert!(
        writes[1] > writes[0] + 1,
        "the source-order var initializer overwrites the hoisted function later"
    );
}
